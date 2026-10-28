//! Running extension test cases and printing their verdicts (`docs/testing.md`, "Testing an extension").

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::clock::Clock;
use extensions::case::{Case, CaseRun, Input, SessionCase, Verdict, run_call};
use serde_json::Value;

/// Runs one case file and prints its child verdict.
pub(crate) fn extension_case(
    path: PathBuf,
    process_clock: Arc<dyn Clock>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let fallback_name = path
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return malformed(&fallback_name, &[format!("{}: {error}", path.display())]);
        }
    };
    let check_script = |script_path: &Path, script_bytes: &[u8]| {
        provider::scripted::Script::parse(script_path, script_bytes)
            .map(|_| ())
            .map_err(|error| error.to_string())
    };
    let case = match Case::parse(&path, &bytes, &check_script) {
        Ok(case) => case,
        Err(error) => return malformed(&fallback_name, &[error.to_string()]),
    };
    let (name, case) = match case {
        Case::Call(call) => {
            let name = call.name.clone().unwrap_or(fallback_name);
            let verdict = run_call(call, process_clock);
            return match verdict {
                Verdict::Pass => report(&name, &[]),
                Verdict::Fail(failures) => report(&name, &failures),
                Verdict::Malformed(failures) => malformed(&name, &failures),
            };
        }
        Case::Session(mut session) => {
            let name = session.name.clone().unwrap_or(fallback_name);
            session.name = Some(name.clone());
            (name, session)
        }
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => return report(&name, &[format!("the case workspace: {error}")]),
    };
    if let Err(error) = write_inputs(&case, &workspace) {
        return report(&name, &[error]);
    }
    let prompt = match &case.input {
        Input::Prompt(prompt) => Some(prompt.clone()),
        Input::Command(_) => None,
    };
    let case_run = CaseRun::new(case, Arc::clone(&process_clock));
    let signals = match crate::shutdown::install(process_clock) {
        Ok(signals) => signals,
        Err(error) => return report(&name, &[format!("case signals: {error}")]),
    };
    let result = crate::session_command::new_session(
        contract::SessionId(doors::mint("s_")),
        crate::per_run(Some("scripted/script.json".to_owned()), Vec::new()),
        prompt,
        false,
        false,
        None,
        case_run.session_clock(),
        &signals,
        fiber,
        Some(Arc::clone(&case_run)),
        None,
        false,
        Vec::new(),
    );
    let failures = case_run
        .verdict()
        .unwrap_or_else(|| vec![format!("session setup failed with exit code {result}")]);
    report(&name, &failures)
}

fn write_inputs(case: &SessionCase, workspace: &Path) -> Result<(), String> {
    let script_path = workspace.join("script.json");
    let script_bytes = serde_json::to_vec(&case.script)
        .map_err(|error| format!("script: cannot encode: {error}"))?;
    fs::write(&script_path, script_bytes)
        .map_err(|error| format!("{}: {error}", script_path.display()))?;
    if let Some(config) = &case.config {
        let home = config::fiber_home_from_env().map_err(|error| error.to_string())?;
        let config_path = config::global_config_file(&home);
        write_config(&config_path, config)?;
    }
    if let Some(settings) = &case.settings {
        let home = config::fiber_home_from_env().map_err(|error| error.to_string())?;
        write_settings(&home, settings)?;
    }
    Ok(())
}

/// Writes each entry of the case's `settings` object as that extension's
/// global settings file under `home/config`, creating a missing parent
/// directory per file. A non-object never reaches this function:
/// `SessionCase::parse` rejects it.
fn write_settings(home: &Path, settings: &Value) -> Result<(), String> {
    for (name, value) in settings.as_object().into_iter().flatten() {
        write_config(&config::settings_file(home, name), value)?;
    }
    Ok(())
}

/// Writes the scripted `config.json` whole into `path`, creating a
/// missing parent directory. The bytes are the case's raw JSON, with no
/// type check and no lock: only the rename is atomic.
fn write_config(path: &Path, config: &Value) -> Result<(), String> {
    let bytes =
        serde_json::to_vec(config).map_err(|error| format!("config: cannot encode: {error}"))?;
    config::write_atomic(path, &bytes, 0o666).map_err(|error| error.to_string())
}

pub(super) fn report(name: &str, failures: &[String]) -> i32 {
    verdict(name, failures, 1)
}

/// A malformed case: invalid JSON, an unsupported function or a bad field.
/// The case never ran, so this is a wrong invocation, which exits 2
/// (`docs/invocation.md`, "Commands and flags").
pub(super) fn malformed(name: &str, failures: &[String]) -> i32 {
    verdict(name, failures, 2)
}

#[allow(
    clippy::print_stdout,
    reason = "the hidden child reports one verdict on stdout (`docs/testing.md`, \"Testing an extension\")"
)]
fn verdict(name: &str, failures: &[String], code: i32) -> i32 {
    if failures.is_empty() {
        println!("ok {name}");
        return 0;
    }
    println!("FAIL {name}");
    for failure in failures {
        println!("  {failure}");
    }
    code
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
