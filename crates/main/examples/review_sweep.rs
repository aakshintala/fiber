//! Person-run sweep runner (`docs/testing/fakes.md`, "Live calls and
//! evals"): runs the committed labelled set once per `--models` reviewer
//! model through the public `routing::reviewer` seam and prints the
//! miss/false-block table. `--check` only parses and counts a file. Live
//! calls are opt-in by running this example; no test can enable one.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the runner's table and counts are its interface"
)]

#[path = "../tests/support/review_sweep.rs"]
mod sweep;

use std::io;
use std::path::Path;
use std::process::{Command, ExitCode, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;
use contract::shapes::Failure;

fn main() -> ExitCode {
    match run(&std::env::args().collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("review_sweep: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(argv: &[String]) -> Result<(), String> {
    match sweep::parse_args(argv)? {
        sweep::Mode::Check { file } => {
            let entries = sweep::read_set_file(&file)?;
            println!("{}: {} lines", file.display(), entries.len());
            Ok(())
        }
        sweep::Mode::Sweep { models, set } => {
            let entries = sweep::read_set_file(&set)?;
            let home = config::fiber_home_path_from_env().map_err(|error| error.to_string())?;
            let workspace = std::env::current_dir()
                .map_err(|error| format!("the current directory: {error}"))?;
            let clock: Arc<dyn Clock> = Arc::new(fakes::clock::SystemClock);
            let mut reports = Vec::with_capacity(models.len());
            for model in &models {
                let provider = reviewer_provider(&home, &workspace, model, &clock)?;
                let outcomes = entries
                    .iter()
                    .map(|entry| sweep::drive(entry, &provider, &clock))
                    .collect::<Vec<_>>();
                for outcome in &outcomes {
                    if let sweep::Outcome::Errored(errored) = outcome {
                        eprintln!(
                            "review_sweep: error: {model} {}: {}",
                            errored.id, errored.reason
                        );
                    }
                }
                reports.push(sweep::score(model, &outcomes));
            }
            print!("{}", sweep::render_table(&reports));
            Ok(())
        }
    }
}

/// Runs a credential `command` source with `timeout`: kills the child and
/// reports a timeout when it overruns, so a hung helper cannot hang the
/// sweep.
#[allow(
    clippy::disallowed_methods,
    reason = "person-run credential runner: bounds a hung helper on the wall clock; tests never run it"
)]
fn run_command(
    command: &mut Command,
    timeout: Duration,
    clock: &Arc<dyn Clock>,
) -> io::Result<Output> {
    command.stdin(Stdio::null()).stderr(Stdio::null());
    let mut child = command.spawn()?;
    let start = clock.now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if clock.now().saturating_duration_since(start) >= timeout {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "review_sweep: the credential command timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Builds the reviewer provider for one `--models` entry through the
/// public `routing::reviewer::{reference, model, choose}` seam. The
/// `--models` entry rides `reviewer.model`, so `reference` returns it
/// before reading the session; the session model only feeds the fallback
/// that never runs. Resolution failures name `reviewer.model`.
fn reviewer_provider(
    home: &Path,
    workspace: &Path,
    bare: &str,
    clock: &Arc<dyn Clock>,
) -> Result<Arc<dyn contract::provider::Provider>, String> {
    let config = config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: config::ProjectKey::new("review-sweep").map_err(|error| error.to_string())?,
        overrides: vec![format!("reviewer.model={bare}")],
    })
    .map_err(|error| error.to_string())?;
    let (providers, _notices) =
        extensions::Providers::load(home).map_err(|error| error.to_string())?;
    // The session is the candidate itself: `reviewer.model` is set, so the
    // lookup below returns before reading it.
    let session = providers
        .resolve(bare)
        .map_err(|error| format!("review_sweep: reviewer.model {bare}: {error}"))?;
    let here = routing::connect::Here {
        workspace: workspace.to_path_buf(),
        clock: Arc::clone(clock),
        cache_diagnostics: false,
    };
    let mut lookup =
        |data: &config::ProviderData| -> Result<(String, routing::lua_providers::Access), Failure> {
            let label = config.credentials().credential_label(data);
            let lua = providers.lua(&data.name);
            let (key, signer) =
                routing::lua_providers::session_credential(lua, data, &label, || {
                    let clock = Arc::clone(clock);
                    config
                        .credentials()
                        .credential_with(data, &label, &|command, timeout| {
                            run_command(command, timeout, &clock)
                        })
                        .map(|read| read.secret)
                        .map_err(|error| Failure {
                            code: error.code(),
                            message: error.to_string(),
                            retry_after_ms: None,
                            provider: None,
                        })
                })?;
            Ok((
                label,
                routing::lua_providers::Access::new(lua, (key, signer)),
            ))
        };
    let chosen = routing::reviewer::choose(&providers, &config, &session, &here, &mut lookup, None)
        .map_err(|error| format!("review_sweep: reviewer.model {bare}: {}", error.message))?
        .ok_or_else(|| format!("review_sweep: reviewer.model {bare} names no reviewer"))?;
    Ok(chosen.provider)
}
