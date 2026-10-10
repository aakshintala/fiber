//! `fiber config get|set` (`docs/configuration.md`, "When Fiber writes";
//! `docs/invocation.md`, "Commands and flags"): `get` prints the effective
//! value and the layer it came from, `set` writes one key in one layer's
//! file. `main` parses argv and dispatches here; this crate takes plain
//! values.

use std::io::{self, Write};
use std::path::Path;

use config::{Config, Layer, ProjectKey, Sources};
use contract::ErrorCode;
use contract::events::Notice;
use contract::shapes::Failure;
use extensions::Providers;
use serde_json::Value;

use crate::{fail, failed, project_of};

/// Prints each notice as one line on standard error: exactly the notice's
/// message, nothing else (`docs/configuration.md`, "When Fiber reads
/// configuration"). Standard output and the exit status are untouched.
pub(crate) fn print_notices(err: &mut dyn Write, notices: &[Notice]) {
    for notice in notices {
        writeln!(err, "{}", notice.message).unwrap_or(());
    }
}

/// The effective value of `key` and the layer it came from, as `get` prints
/// them: the value as compact JSON, then ` from `, then the layer.
fn run_get(
    home: &Path,
    workspace: &Path,
    key: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .map_err(|e| failed(e.code(), e))?;
    // Each notice as one line on stderr: this load's, then the installed
    // providers', each once per command run.
    let mut notices: Vec<Notice> = config.notices().to_vec();
    if let Ok((_, loading)) = Providers::load(home) {
        notices.extend(loading);
    }
    print_notices(err, &notices);
    // The notes print as the reviewer reads them: each layer's text under
    // its heading, with no layer named (`docs/permissions.md`, "What the
    // person tells it").
    if key == "reviewer.context" {
        let notes = config.reviewer_context();
        if notes.is_empty() {
            writeln!(err, "`{key}` is not set.")
                .map_err(|e| failed(ErrorCode::IoFailed, format!("standard error: {e}")))?;
            return Ok(());
        }
        writeln!(out, "{notes}")
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
        return Ok(());
    }
    match config.get(key, None) {
        Some((value, source)) => {
            let text = serde_json::to_string(&value)
                .map_err(|e| failed(ErrorCode::IoFailed, format!("a value for `{key}`: {e}")))?;
            writeln!(out, "{text} from {source}")
                .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
            Ok(())
        }
        None if !config::known_key(key) => Err(crate::usage(format!(
            "`{key}` is not a configuration key. Run `fiber --help` for usage."
        ))),
        None => {
            writeln!(err, "`{key}` is not set.")
                .map_err(|e| failed(ErrorCode::IoFailed, format!("standard error: {e}")))?;
            Ok(())
        }
    }
}

/// Writes one key in one layer's file, parsing the value as JSON, or a bare
/// string when it does not parse, as `-c` does. On success prints each
/// loading notice as one line on standard error, each load's once per
/// command run; nothing else.
fn run_set(
    home: &Path,
    workspace: &Path,
    layer: Layer,
    key: &str,
    value: &str,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let (_, project) = project_of(home, workspace)?;
    let parsed = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.into()));
    let mut notices = Vec::new();
    if key == "model"
        && let Some(typed) = parsed.as_str()
    {
        notices.extend(check_model(home, workspace, &project, typed, err)?);
    } else {
        // No other load happens on this path: one lenient load surfaces
        // this command's configuration notices. A configuration that
        // fails to load fails no set.
        if let Ok(config) = Config::load(Sources {
            home: home.to_path_buf(),
            workspace: workspace.to_path_buf(),
            project: project.clone(),
            overrides: Vec::new(),
        }) {
            notices.extend(config.notices().iter().cloned());
        }
    }
    config::set(home, workspace, &project, layer, key, parsed).map_err(|e| failed(e.code(), e))?;
    print_notices(err, &notices);
    Ok(())
}

/// `fiber config set` for another front end, such as the terminal's
/// `/settings`: the same parsing, type check, layer refusal and `model`
/// check, with each warning line `set` prints on standard error returned
/// instead.
pub fn config_set_text(
    home: &Path,
    workspace: &Path,
    layer: Layer,
    key: &str,
    text: &str,
) -> Result<Vec<String>, Failure> {
    let mut err = Vec::new();
    run_set(home, workspace, layer, key, text, &mut err)?;
    Ok(String::from_utf8_lossy(&err)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Runs `run` with Fiber home and the current directory, mapping any failure
/// to its exit code.
fn with_dirs(run: impl FnOnce(&Path, &Path) -> Result<(), Failure>) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            run(&home, &workspace)
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// Checks a `model` value against the installed providers' cached model
/// lists, by the rules in `docs/model-routing.md`, "Naming a model"
/// (`docs/configuration.md`, "When Fiber writes"). With no cached list
/// the value is accepted, and no network is touched for any key. The typed
/// text is written as typed, never normalised. Returns every notice the
/// check's loads raised, for the caller to print once per command run.
fn check_model(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    typed: &str,
    err: &mut dyn Write,
) -> Result<Vec<Notice>, Failure> {
    let (providers, loading) = Providers::load(home).map_err(|e| failed(e.code(), e))?;
    let mut notices = loading;
    // Without a cached list the reference is accepted unchecked: one
    // naming a provider that is not installed, or one installed with an
    // empty list, and a bare id when every installed list is empty. This
    // runs on the unfilled load, so a provider whose every model is
    // unconfigured still counts as cached.
    let (base, _) = Providers::split_thinking(typed);
    let uncached = match base.split_once('/') {
        Some((name, _)) => providers
            .get(name)
            .is_none_or(|provider| provider.models.is_empty()),
        None => providers.names().all(|name| {
            providers
                .get(name)
                .is_some_and(|provider| provider.models.is_empty())
        }),
    };
    if uncached {
        // No cached list to check against, so no strict load happens on
        // this path: one lenient load still surfaces this command's
        // configuration notices. A configuration that fails to load fails
        // no set.
        if let Ok(config) = Config::load(Sources {
            home: home.to_path_buf(),
            workspace: workspace.to_path_buf(),
            project: project.clone(),
            overrides: Vec::new(),
        }) {
            notices.extend(config.notices().iter().cloned());
        }
        return Ok(notices);
    }
    // The fill reads the person's own files with no overrides, and the
    // process environment, touching no network: the person may set the
    // host next, so a reference left out with `model_unconfigured` warns
    // and is written. Notices for other left-out models are dropped.
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: Vec::new(),
    })
    .map_err(|e| failed(e.code(), e))?;
    notices.extend(config.notices().iter().cloned());
    let mut providers = providers;
    let _notices = providers
        .fill_placeholders(&config, &|name| std::env::var(name).ok())
        .map_err(|e| failed(e.code(), e))?;
    match providers.resolve(typed) {
        Ok(_) => Ok(notices),
        Err(e @ extensions::Error::Unconfigured { .. }) => {
            writeln!(err, "fiber: {e}").unwrap_or(());
            Ok(notices)
        }
        Err(extensions::Error::UnknownModel { .. } | extensions::Error::ModelMissing { .. }) => {
            Err(no_model(&providers, typed))
        }
        Err(e @ extensions::Error::Ambiguous { .. }) => {
            Err(failed(ErrorCode::ModelAmbiguous, e.to_string()))
        }
        Err(e) => Err(failed(e.code(), e.to_string())),
    }
}

/// The `no_model` failure: what was typed, the closest installed
/// references, and where to list them.
fn no_model(providers: &Providers, typed: &str) -> Failure {
    let near: Vec<String> = closest(providers, typed);
    let mut message = format!("No installed model matches `{typed}`.");
    if !near.is_empty() {
        let listed = near
            .iter()
            .map(|reference| format!("`{reference}`"))
            .collect::<Vec<_>>()
            .join(", ");
        message.push_str(&format!(" Closest: {listed}."));
    }
    message.push_str(" Run `fiber models` to list them.");
    failed(ErrorCode::NoModel, message)
}

/// Up to 3 installed `provider/model` references nearest the typed text,
/// with any `:<level>` suffix stripped, nearest first and ties in sorted
/// reference order.
fn closest(providers: &Providers, typed: &str) -> Vec<String> {
    let (base, _) = Providers::split_thinking(typed);
    let mut refs: Vec<String> = providers
        .names()
        .flat_map(|name| {
            providers.get(name).into_iter().flat_map(move |provider| {
                provider
                    .models
                    .iter()
                    .map(move |model| format!("{name}/{}", model.id))
            })
        })
        .collect();
    refs.sort();
    refs.sort_by_key(|candidate| distance(base, candidate));
    refs.truncate(3);
    refs
}

/// Edits between two references, in characters, ranking the closest matches.
fn distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut prev: Vec<usize> = (0..=right.len()).collect();
    for (i, l) in left.iter().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(i + 1);
        for (j, r) in right.iter().enumerate() {
            let cost = usize::from(l != r);
            let above = prev.get(j + 1).map_or(usize::MAX, |v| v.saturating_add(1));
            let beside = prev.get(j).map_or(usize::MAX, |v| v.saturating_add(cost));
            let side = current.get(j).map_or(usize::MAX, |v| v.saturating_add(1));
            current.push(above.min(beside).min(side));
        }
        prev = current;
    }
    prev.get(right.len()).copied().unwrap_or(usize::MAX)
}

/// `fiber config get <key>` in the current directory: prints the effective
/// value and the layer it came from.
pub fn config_get(key: &str) -> i32 {
    with_dirs(|home, workspace| run_get(home, workspace, key, &mut io::stdout(), &mut io::stderr()))
}

/// `fiber config set [--project | --repo] <key> <value>` in the current
/// directory: writes one key in one layer's file.
pub fn config_set(layer: Layer, key: &str, value: &str) -> i32 {
    with_dirs(|home, workspace| run_set(home, workspace, layer, key, value, &mut io::stderr()))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
