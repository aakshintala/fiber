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

mod artifact;
mod attach;
mod client;
mod close;
mod drive;
mod error;
mod offered;
pub use error::Error;
pub mod hub;
mod isolation;
mod message;
mod notes;
mod pasted;
mod prompt_history;
mod reply;
mod rewind;
mod run_command;
mod session;
mod session_error;
mod sessions;
mod shell;
mod signals;
mod socket;
#[cfg(test)]
#[path = "socket_timing_tests.rs"]
mod socket_timing_tests;
mod steer;
mod watch;

use std::io::{self, Read, Write};

use contract::shapes::Failure;
use contract::{ErrorCode, PreSessionExit};

pub use attach::attach;
pub use isolation::{
    DelegateCreateError, Isolation, RecreateError, Recreated, RemovalHold, Shared, WorktreeOpener,
    adopt_delegate_worktree, create_delegate_worktree, end_delegate_worktree, isolate, recreate,
    share, try_hold_removal,
};
pub use notes::{ReadOnlyOutcome, ReadOnlySummary};
pub use session::{Declare, McpLogin, Replacement, ServersRefresh, Session};
pub use session_error::SessionError;
pub use sessions::SessionSockets;
pub use sessions::session_entries;
pub use signals::{SHUTDOWN_BOUND, Signals, signal_code};
pub use socket::{SOCKET_PATH_MAX, check_socket_path};
pub use steer::{Steered, steer};
pub use watch::{Watched, watch, watch_after};

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
) -> Result<String, SessionError> {
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
            SessionError::StdinNotUtf8
        } else {
            SessionError::StdinUnreadable { source: e }
        }
    })?;
    let piped = (!piped.trim().is_empty()).then_some(piped);
    match (kept, piped) {
        (Some(prompt), Some(stdin)) => Ok(format!("{prompt}\n{stdin}")),
        (Some(prompt), None) | (None, Some(prompt)) => Ok(prompt),
        (None, None) => Err(no_prompt()),
    }
}

fn no_prompt() -> SessionError {
    SessionError::NoPrompt
}

/// Ends a process that failed before any session existed: the
/// `fiber_exited` line with no `session_id` on `out`, and one sentence on
/// `err` (`docs/errors.md`, "Before a session exists"). Returns the exit
/// code.
pub fn exit_before_session(failure: Failure, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let code = exit_code(&failure);
    let message = failure.message.clone();
    // Nothing is left to report a failed write to.
    if let Ok(line) = serde_json::to_vec(&PreSessionExit::new(code, failure)) {
        support::framing::write_line(out, &line).unwrap_or(());
    }
    writeln!(err, "fiber: {message}").unwrap_or(());
    code
}

pub use support::mint;
