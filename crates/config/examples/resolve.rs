//! The `resolve` jig (`docs/testing.md`, "Jigs"): prints the merged
//! configuration for the Fiber home `FIBER_HOME` names and a project.
//!
//! ```text
//! FIBER_HOME=/tmp/home cargo run -p config --example resolve -- \
//!     <workspace> <project-key> [--model provider/model] [-c key=value]...
//! ```
//!
//! The merged JSON goes to stdout and each notice to stderr. A failure prints
//! its code and message to stderr and exits 1.

use std::io::Write;
use std::process::ExitCode;

use config::{Config, ConfigError, Sources};

const USAGE: &str =
    "usage: resolve <workspace> <project-key> [--model provider/model] [-c key=value]...";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(workspace), Some(project)) = (args.next(), args.next()) else {
        return fail(USAGE);
    };
    let mut model = None;
    let mut overrides = Vec::new();
    while let Some(flag) = args.next() {
        match (flag.as_str(), args.next()) {
            ("--model", Some(value)) => model = Some(value),
            ("-c", Some(value)) => overrides.push(value),
            _ => return fail(USAGE),
        }
    }
    match run(workspace, project, model.as_deref(), overrides) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&format!("{}: {e}", code(&e))),
    }
}

fn run(
    workspace: String,
    project: String,
    model: Option<&str>,
    overrides: Vec<String>,
) -> Result<(), ConfigError> {
    let home = config::fiber_home_from_env()?;
    let config = Config::load(Sources {
        home,
        workspace: workspace.into(),
        project,
        overrides,
    })?;
    let mut err = std::io::stderr().lock();
    for notice in config.notices() {
        writeln!(err, "notice: {}", notice.message).unwrap_or(());
    }
    let text = serde_json::to_string_pretty(&config.merged(model)).unwrap_or_default();
    writeln!(std::io::stdout().lock(), "{text}").unwrap_or(());
    Ok(())
}

fn code(e: &ConfigError) -> String {
    serde_json::to_value(e.code())
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

fn fail(message: &str) -> ExitCode {
    writeln!(std::io::stderr().lock(), "{message}").unwrap_or(());
    ExitCode::FAILURE
}
