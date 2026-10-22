//! `fiber doctor`: the startup report. It reads the home directory and
//! the local hub, runs no session, loads no Lua, connects to nothing but
//! the hub socket, and runs no credential command.

use std::io::{self, Write};
use std::path::Path;
use std::time::SystemTime;

use contract::shapes::Failure;

use crate::CliError;

mod files;
mod startup;

pub(crate) use files::{age, clean, newest_crash, newest_error};
pub(crate) use startup::startup;

#[cfg(test)]
#[path = "doctor/doctor_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "doctor/socket_path_tests.rs"]
mod socket_path_tests;

/// The hub's state for one report: the injected seam `run` tests through,
/// and the bounded `probe` production fills.
#[derive(Clone)]
pub(crate) enum Hub {
    /// No hub socket, or a refused one.
    NotRunning,
    /// A hub answered the connect: its environment may still be unknown.
    Running {
        /// The hub's variables, or why they are unknown.
        environment: Result<contract::events::Variables, String>,
    },
    /// The connect failed another way.
    Unknown(String),
}

/// Everything `run` reads: the report's inputs, all injected but home.
struct Inputs<'a> {
    home: &'a Path,
    workspace: &'a Path,
    version: &'a str,
    now: SystemTime,
    env: &'a dyn Fn(&str) -> Option<String>,
    has_userinfo: &'a dyn Fn(&str) -> bool,
    hub: &'a dyn Fn(&Path, &dyn contract::clock::Clock) -> Hub,
    clock: &'a dyn contract::clock::Clock,
}

/// Prints the report on `out`, notices on `err`, and says whether a
/// session can start. A closed `out` is the only failure once home and
/// workspace resolve.
fn run(inputs: &Inputs<'_>, out: &mut dyn Write, err: &mut dyn Write) -> Result<bool, CliError> {
    let checked = startup(
        inputs.home,
        inputs.workspace,
        &[],
        inputs.env,
        inputs.has_userinfo,
    );
    for notice in &checked.notices {
        // A notice lost to a closed stderr changes nothing.
        drop(writeln!(err, "fiber: {}", clean(&notice.message)));
    }
    let mut report = |line: &str| writeln!(out, "{line}").map_err(CliError::DoctorReport);
    report(&format!("fiber {}", inputs.version))?;
    for line in &checked.lines {
        report(line)?;
    }
    hub(inputs, &mut report)?;
    for line in &checked.secrets {
        report(line)?;
    }
    logs(inputs, &mut report);
    Ok(checked.blocker.is_none())
}

/// The hub lines: running or not, and whose environment it runs with.
fn hub(
    inputs: &Inputs<'_>,
    report: &mut impl FnMut(&str) -> Result<(), CliError>,
) -> Result<(), CliError> {
    match (inputs.hub)(inputs.home, inputs.clock) {
        Hub::NotRunning => report("hub: not running"),
        Hub::Unknown(reason) => report(&format!("hub: unknown ({})", clean(&reason))),
        Hub::Running { environment } => {
            report("hub: running")?;
            match environment {
                Ok(variables) => {
                    match variables.source {
                        contract::events::VariablesSource::LoginShell => {
                            report("hub environment: login shell")?;
                        }
                        contract::events::VariablesSource::Inherited => {
                            report(
                                "hub environment: inherited, because the login-shell capture failed",
                            )?;
                        }
                    }
                    report(&format!("hub PATH: {}", clean(&variables.path)))?;
                    let names = variables
                        .names
                        .iter()
                        .map(|name| clean(name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    report(&format!("hub variables: {names}"))
                }
                Err(reason) => report(&format!("hub environment: unknown ({})", clean(&reason))),
            }
        }
    }
}

/// The logs, crash and docs lines: what the last runs left behind.
fn logs(inputs: &Inputs<'_>, report: &mut impl FnMut(&str) -> Result<(), CliError>) {
    let home = inputs.home;
    drop(report(&format!(
        "logs: {}",
        clean(&config::logs_dir(home).display().to_string())
    )));
    match newest_error(&config::logs_dir(home)) {
        Some((file, ts, code, message)) => {
            let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ts);
            drop(report(&format!(
                "newest error: {}, {} old, {}: {}",
                clean(&file),
                age(inputs.now, at),
                clean(&code),
                clean(&message)
            )));
        }
        None => {
            drop(report("newest error: none"));
        }
    }
    match newest_crash(&config::crashes_dir(home)) {
        Some((path, mtime)) => {
            drop(report(&format!(
                "newest crash: {}, {} old",
                clean(&path.display().to_string()),
                age(inputs.now, mtime)
            )));
        }
        None => {
            drop(report("newest crash: none"));
        }
    }
    if !config::docs_dir(home).is_dir() {
        drop(report("user guide not installed: run `fiber update`"));
    }
}

/// The hub's state over the bounded diagnostic exchange: one connection
/// within [`STATUS_WITHIN`], starting nothing. A hub that answers the
/// connect but stays silent, refuses, or garbles its variables is still
/// running, with its environment unknown.
fn probe(home: &Path, clock: &dyn contract::clock::Clock) -> Hub {
    use crate::hub_status::{Diagnostic, STATUS_WITHIN, diagnose};
    match diagnose(home, clock, STATUS_WITHIN) {
        Diagnostic::Absent => Hub::NotRunning,
        Diagnostic::Connected(result) => {
            let environment = match result {
                Ok(reply) => reply.variables,
                Err(failure) => Err(failure.message),
            };
            Hub::Running { environment }
        }
        Diagnostic::Failed(failure) => Hub::Unknown(failure.message),
    }
}

/// The public entry point: resolves home without creating anything and
/// reports. 0 when a session can start, 1 with the first blocker,
/// otherwise the failure's code.
pub fn doctor(
    version: &str,
    clock: &dyn contract::clock::Clock,
    remote: &dyn contract::McpRemote,
) -> i32 {
    let home = match config::fiber_home_path_from_env() {
        Ok(home) => home,
        Err(error) => return crate::fail(CliError::Config(error).into()),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(source) => {
            return crate::fail(CliError::DoctorDirectory(source).into());
        }
    };
    let now = clock.wall();
    match run(
        &Inputs {
            home: &home,
            workspace: &workspace,
            version,
            now,
            env: &|name| std::env::var(name).ok(),
            has_userinfo: &|url| remote.has_userinfo(url),
            hub: &probe,
            clock,
        },
        &mut io::stdout(),
        &mut io::stderr(),
    ) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => crate::fail(error.into()),
    }
}

/// Runs the production report into a buffer and returns it: what
/// `doctor` prints on stdout, through the same `run` and `probe` seams
/// with the production `has_userinfo` wiring. The caller decides what
/// the text is for.
pub fn capture(
    home: &Path,
    workspace: &Path,
    version: &str,
    clock: &dyn contract::clock::Clock,
    remote: &dyn contract::McpRemote,
) -> Result<String, CliError> {
    let mut out = Vec::new();
    let mut err = Vec::new();
    run(
        &Inputs {
            home,
            workspace,
            version,
            now: clock.wall(),
            env: &|name| std::env::var(name).ok(),
            has_userinfo: &|url| remote.has_userinfo(url),
            hub: &probe,
            clock,
        },
        &mut out,
        &mut err,
    )?;
    // Every line reaches the buffer through `writeln!` of a `str`, so
    // the bytes are UTF-8 by construction; the mapping only types the
    // impossible.
    String::from_utf8(out).map_err(|error| {
        CliError::DoctorReport(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
}

/// Annotates a fresh session's failure with the doctor line for the same
/// cause: `<line>\n<original>`, keeping code, provider and retry. Only a
/// key blocker whose code matches, with no provider detail, annotates.
pub fn with_fix(failure: Failure, overrides: &[String]) -> Failure {
    let (Ok(home), Ok(workspace)) = (config::fiber_home_path_from_env(), std::env::current_dir())
    else {
        return failure;
    };
    fix_at(failure, overrides, &home, &workspace, &|name| {
        std::env::var(name).ok()
    })
}

/// `with_fix` against explicit inputs, so tests name the home, workspace
/// and environment instead of the process's.
pub(crate) fn fix_at(
    failure: Failure,
    overrides: &[String],
    home: &Path,
    workspace: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Failure {
    let checked = startup(home, workspace, overrides, env, &|_| false);
    let Some(blocker) = checked.blocker else {
        return failure;
    };
    if !blocker.key || blocker.code != failure.code || failure.provider.is_some() {
        return failure;
    }
    Failure {
        message: format!("{}\n{}", blocker.line, failure.message),
        ..failure
    }
}
