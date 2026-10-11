//! The doors (`docs/invocation.md`): `fiber ask`, whose argument or stdin is
//! the prompt and whose stdout is the event stream, and the internal session
//! command it runs, one process per session ("Processes"). The session
//! command owns the process boundary: the session's socket and its
//! `fiber_started` and `fiber_exited` lines.
//!
//! `main` builds the session's parts; this crate never sees a provider or
//! the loop (`docs/architecture.md`, "The call rules").

// The shared test support names `doors::Session` and `doors::mint`, so the
// unit tests that include it see this crate under that name too.
#[cfg(test)]
extern crate self as doors;

mod attach;
mod client;
mod close;
mod drive;
pub mod hub;
mod isolation;
mod pasted;
mod prompt_history;
mod reply;
mod rewind;
mod run_command;
mod session;
mod shell;
mod signals;
mod socket;
mod watch;

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use contract::shapes::Failure;
use contract::{ErrorCode, PreSessionExit};

pub use attach::attach;
pub use isolation::{Isolation, isolate};
pub use session::{Declare, Session};
pub use signals::{SHUTDOWN_BOUND, Signals, signal_code};
pub use watch::{Watched, watch};

/// A failure with Fiber's own sentence and nothing from a provider.
pub fn failure(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
        retry_after_ms: None,
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

/// `fiber ask`'s prompt (`docs/invocation.md`, "Getting a prompt in").
///
/// The source is the arguments alone. Stdin is read only when `dash` is set,
/// or when `arg` is absent and stdin is not a terminal. A whitespace-only
/// part is dropped. When both parts remain, the prompt is the argument, a
/// newline, then stdin.
pub fn prompt(
    arg: Option<String>,
    dash: bool,
    stdin: &mut dyn Read,
    terminal: bool,
) -> Result<String, Failure> {
    let supplied = arg.is_some();
    let kept = arg.filter(|text| !text.trim().is_empty());
    // An argument and no `-` never reads stdin, even when the argument is blank.
    if !dash && supplied {
        return kept.ok_or_else(no_prompt);
    }
    if !dash && terminal {
        return Err(no_prompt());
    }
    let mut piped = String::new();
    stdin.read_to_string(&mut piped).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            failure(ErrorCode::Usage, "stdin is not UTF-8 text.")
        } else {
            failure(ErrorCode::IoFailed, format!("stdin could not be read: {e}"))
        }
    })?;
    let piped = (!piped.trim().is_empty()).then_some(piped);
    match (kept, piped) {
        (Some(prompt), Some(stdin)) => Ok(format!("{prompt}\n{stdin}")),
        (Some(prompt), None) | (None, Some(prompt)) => Ok(prompt),
        (None, None) => Err(no_prompt()),
    }
}

fn no_prompt() -> Failure {
    failure(
        ErrorCode::Usage,
        "No prompt. Run `fiber ask \"<prompt>\"` or `fiber ask < <file>`.",
    )
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

/// A workspace's project and whether git found a repository there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// The project's identity path (`docs/state.md`, "Projects"): git's
    /// shared directory inside a repository, otherwise the launch directory
    /// itself, symlinks resolved.
    pub path: PathBuf,
    /// Whether `git rev-parse` found a repository at the launch directory,
    /// a bare repository and a `.git` directory included.
    pub in_repository: bool,
}

/// The project's identity path (`docs/state.md`, "Projects"), from
/// [`resolve_project`].
pub fn project(launch: &Path) -> PathBuf {
    resolve_project(launch).path
}

/// Resolves the project of `launch` with one `git rev-parse`: git's shared
/// directory when `launch` is inside a repository, otherwise `launch`
/// itself, symlinks resolved. The `GIT_DIR`, `GIT_WORK_TREE`,
/// `GIT_COMMON_DIR` and `GIT_INDEX_FILE` variables are removed first, so
/// the caller's environment cannot redirect the discovery.
pub fn resolve_project(launch: &Path) -> Project {
    let common = Command::new("git")
        .arg("-C")
        .arg(launch)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let (identity, in_repository) = match common {
        Ok(out) if out.status.success() => (
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n')),
            true,
        ),
        // Not a repository, or no git: the launch directory is the project.
        Ok(_) | Err(_) => (launch.to_path_buf(), false),
    };
    Project {
        path: identity.canonicalize().unwrap_or(identity),
        in_repository,
    }
}
