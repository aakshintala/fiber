//! Resuming a session a relayed command names (`docs/invocation.md`,
//! "Lifecycle"): a session whose socket accepts no connection is a log to
//! resume. The hub finds the log by the session's id, starts the internal
//! session command with `--resume` in the workspace the log recorded, and
//! waits for the socket as `start` does. A session that is running is
//! attached to instead: one resume at a time per hub, and the socket is
//! tried again first, so two commands for one exited session start one
//! process.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::PoisonError;

use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::connection::Hub;
use crate::start::{self, Bind};

/// Why a relayed command's session could not be reached: the rejection's
/// code and Fiber's own sentence.
pub(crate) struct Refused {
    /// The rejection code.
    pub(crate) code: ErrorCode,
    /// What the client is told.
    pub(crate) message: String,
}

/// A connection to `session`'s socket: the running session's, or, when
/// none accepts, a resumed one's. `session` has the minted shape. Under the
/// hub's resume gate the socket is tried first, so the starter is never
/// asked to resume a session whose socket accepts.
pub(crate) fn resume(hub: &Hub, session: &SessionId) -> Result<UnixStream, Refused> {
    let socket = hub.home.join("run").join(&session.0);
    let _gate = hub
        .resume_gate
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    let Some(log) = find_log(&hub.home, session) else {
        return Err(not_found(session));
    };
    let Some(workspace) = recorded_workspace(&log) else {
        hub.diag.warn_session(
            session,
            "log_corrupt",
            &format!("Session {} has an unreadable log.", session.0),
        );
        return Err(Refused {
            code: ErrorCode::LogCorrupt,
            message: format!(
                "The log of session `{}` does not start with its workspace.",
                session.0
            ),
        });
    };
    let started = match hub.starter.resume(session, &workspace) {
        Ok(started) => started,
        Err(error) => return Err(io_failed(hub, session, &format!("{error}."))),
    };
    match start::await_bind(hub, &socket, started.as_ref()) {
        Bind::Connected(stream) => {
            hub.diag
                .info_session(session, "session_resumed", "Session resumed for local.");
            Ok(stream)
        }
        // The process lost the session to another that bound first, such
        // as a resume another starter made: that one is running, so the
        // command is passed to it.
        Bind::Exited => match UnixStream::connect(&socket) {
            Ok(stream) => Ok(stream),
            Err(_) => Err(match started.exited() {
                Some(failure) => {
                    hub.diag.warn_session(
                        session,
                        &start::code_name(&failure.code),
                        &format!("Session {} exited before it resumed.", session.0),
                    );
                    Refused {
                        code: failure.code,
                        message: failure.message,
                    }
                }
                None => io_failed(hub, session, "it exited."),
            }),
        },
        Bind::TimedOut => Err(io_failed(hub, session, "it did not bind its socket.")),
        Bind::Failed(detail) => Err(io_failed(hub, session, &detail)),
    }
}

/// The rejection for a session the hub cannot find: `session_not_found`.
pub(crate) fn not_found(session: &SessionId) -> Refused {
    Refused {
        code: ErrorCode::SessionNotFound,
        message: format!("No session `{}`.", session.0),
    }
}

/// The first `projects/*/sessions/<session>/events.jsonl` under `home`, in
/// project-key order. `session` has the minted shape, so the path never
/// leaves the project's `sessions/`.
fn find_log(home: &Path, session: &SessionId) -> Option<PathBuf> {
    let mut projects: Vec<PathBuf> = std::fs::read_dir(home.join("projects"))
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    projects.sort();
    projects
        .into_iter()
        .map(|project| {
            project
                .join("sessions")
                .join(&session.0)
                .join("events.jsonl")
        })
        .find(|log| log.is_file())
}

/// The workspace the log's first line, its `session_started`, recorded.
/// `None` when that line cannot be read or holds no string workspace.
fn recorded_workspace(log: &Path) -> Option<PathBuf> {
    let mut first = String::new();
    BufReader::new(File::open(log).ok()?)
        .read_line(&mut first)
        .ok()?;
    let line: Value = serde_json::from_str(&first).ok()?;
    if line.get("kind")?.as_str()? != "session_started" {
        return None;
    }
    let workspace = line.get("payload")?.get("workspace")?.as_str()?;
    Some(PathBuf::from(workspace))
}

fn io_failed(hub: &Hub, session: &SessionId, detail: &str) -> Refused {
    // The log keeps the code and a fixed sentence: `detail` may hold an io
    // error or a path. The rejection keeps what happened.
    hub.diag.warn_session(
        session,
        "io_failed",
        &format!("Session {} could not resume.", session.0),
    );
    Refused {
        code: ErrorCode::IoFailed,
        message: format!("Session {} could not resume: {detail}", session.0),
    }
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
