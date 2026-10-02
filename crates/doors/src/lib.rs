//! The doors (`docs/invocation.md`): `fiber ask`, whose argument or stdin is
//! the prompt and whose stdout is the event stream, and the internal session
//! command it runs, one process per session ("Processes"). The session
//! command owns the process boundary: the session's socket and its
//! `fiber_started` and `fiber_exited` lines.
//!
//! `main` builds the session's parts; this crate never sees a provider or
//! the loop (`docs/architecture.md`, "The call rules").

mod session;

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use contract::shapes::Failure;
use contract::{ErrorCode, PreSessionExit};

pub use session::Session;

/// A failure with Fiber's own sentence and nothing from a provider.
pub fn failure(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
        retry_after: None,
        provider: None,
    }
}

/// The exit code a failure ends the process with: 2 for `usage`, 1 for any
/// other (`docs/errors.md`, "Before a session exists").
pub fn exit_code(failure: &Failure) -> i32 {
    if failure.code == ErrorCode::Usage {
        2
    } else {
        1
    }
}

/// `fiber ask`'s prompt (`docs/invocation.md`, "Getting a prompt in"): the
/// argument, or stdin when it is not a terminal. Both, or neither, is a
/// `usage` failure. Stdin that is not a terminal is read to its end, so
/// empty stdin beside an argument is not a second prompt.
pub fn prompt(
    arg: Option<String>,
    stdin: &mut dyn Read,
    terminal: bool,
) -> Result<String, Failure> {
    let mut piped = String::new();
    // ponytail: an argument with stdin left as an open pipe blocks here until
    // the pipe closes; how to tell that case apart is #333.
    if !terminal {
        stdin.read_to_string(&mut piped).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                failure(ErrorCode::Usage, "stdin is not UTF-8 text.")
            } else {
                failure(ErrorCode::IoFailed, format!("stdin could not be read: {e}"))
            }
        })?;
    }
    let piped = (!piped.trim().is_empty()).then_some(piped);
    let arg = arg.filter(|a| !a.trim().is_empty());
    match (arg, piped) {
        (Some(prompt), None) | (None, Some(prompt)) => Ok(prompt),
        (Some(_), Some(_)) => Err(failure(
            ErrorCode::Usage,
            "The prompt came both as an argument and on stdin; give one.",
        )),
        (None, None) => Err(failure(
            ErrorCode::Usage,
            "No prompt. Run `fiber ask \"<prompt>\"` or `fiber ask < <file>`.",
        )),
    }
}

/// What `fiber install` shows before it installs (`docs/extensions.md`,
/// "What an install shows"), for an extension that registers only
/// providers as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSummary {
    /// The extension's name.
    pub name: String,
    /// Where it is installed from.
    pub source: String,
    /// Its version.
    pub version: String,
    /// For an update, what changed since the installed commit.
    pub changes: Option<String>,
    /// Each provider it registers, with its models' base URLs.
    pub providers: Vec<(String, Vec<String>)>,
}

/// Whether `fiber install` goes ahead (`docs/extensions.md`, "Installing"):
/// in a terminal it writes each of `summaries` to `out` and asks once, and
/// only `y` or `yes` goes ahead; without a terminal it goes ahead without
/// asking.
pub fn install_approved(
    summaries: &[InstallSummary],
    terminal: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<bool, Failure> {
    if !terminal {
        return Ok(true);
    }
    let io = |e: io::Error| failure(ErrorCode::IoFailed, format!("the terminal: {e}"));
    let mut text = String::new();
    for summary in summaries {
        let verb = if summary.changes.is_some() {
            "Update"
        } else {
            "Install"
        };
        text.push_str(&format!(
            "{verb} {} from {}\nVersion {}\n",
            summary.name, summary.source, summary.version
        ));
        if let Some(changes) = &summary.changes {
            text.push_str(&format!("Changes since the installed commit:\n{changes}"));
            if !changes.ends_with('\n') {
                text.push('\n');
            }
        }
        if summary.providers.is_empty() {
            text.push_str("It registers no provider.\n");
        }
        for (provider, urls) in &summary.providers {
            text.push_str(&format!("Provider {provider}: {}\n", urls.join(", ")));
        }
    }
    text.push_str("Go ahead? [y/N] ");
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(io)?;
    let mut answer = String::new();
    input.read_line(&mut answer).map_err(io)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// Ends a process that failed before any session existed: the
/// `fiber_exited` line with no `session_id` on `out`, and one sentence on
/// `err` (`docs/errors.md`, "Before a session exists"). Returns the exit
/// code.
pub fn exit_before_session(failure: Failure, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let code = exit_code(&failure);
    let message = failure.message.clone();
    // Nothing is left to report a failed write to.
    if let Ok(mut line) = serde_json::to_vec(&PreSessionExit::new(code, failure)) {
        line.push(b'\n');
        out.write_all(&line)
            .and_then(|()| out.flush())
            .unwrap_or(());
    }
    writeln!(err, "fiber: {message}").unwrap_or(());
    code
}

/// A new id from random bytes, such as a session's or a command's
/// (`docs/events.md`, "Identity and ordering"). `RandomState` seeds its keys
/// from the operating system's randomness.
pub fn mint(prefix: &str) -> String {
    format!("{prefix}{:016x}", RandomState::new().hash_one(()))
}

/// The project's identity path (`docs/state.md`, "Projects"): git's shared
/// directory when `launch` is inside a repository, otherwise `launch`
/// itself, symlinks resolved.
pub fn project(launch: &Path) -> PathBuf {
    let common = Command::new("git")
        .arg("-C")
        .arg(launch)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let identity = match common {
        Ok(out) if out.status.success() => {
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'))
        }
        // Not a repository, or no git: the launch directory is the project.
        Ok(_) | Err(_) => launch.to_path_buf(),
    };
    identity.canonicalize().unwrap_or(identity)
}
