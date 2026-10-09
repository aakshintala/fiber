//! Resuming a session a relayed command names (`docs/invocation.md`,
//! "Lifecycle"): a session whose socket accepts no connection is a log to
//! resume. The hub finds the log by the session's id, starts the internal
//! session command with `--resume` in the workspace the log recorded, and
//! waits for the socket as `start` does. A session that is running is
//! attached to instead: one resume at a time per hub, and the socket is
//! tried again first, so two commands for one exited session start one
//! process.
//!
//! A session whose log ends in `fiber_exited` may still be shutting down:
//! its socket or its lock is waited out for up to [`SHUTDOWN_BOUND`]
//! before the client gets `session_held`.
//!
//! A delegate, a session whose `session_started` names a `parent`, is never
//! resumed through the hub: it resumes only through its parent
//! (`docs/delegates.md`, "Talking to a delegate"). A command for one that
//! is not running is refused `session_not_found`, at once when its process
//! is still shutting down. A running delegate is attached to as any session.
//!
//! A log with no complete first line is not corrupt: the hub answers
//! `session_not_found`, as for a missing log.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::PoisonError;
use std::time::Duration;

use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::connection::Hub;
use crate::feed::follow::last_kind;
use crate::start::{self, Bind};

/// Why a relayed command's session could not be reached: the rejection's
/// code and Fiber's own sentence.
pub(crate) struct Refused {
    /// The rejection code.
    pub(crate) code: ErrorCode,
    /// What the client is told.
    pub(crate) message: String,
}

/// How long a session takes to shut down at most (`docs/invocation.md`,
/// "Shutdown"): the hub waits this long for an exiting process to release
/// its session.
pub(crate) const SHUTDOWN_BOUND: Duration = Duration::from_secs(5);

/// How often the hub tries an exiting session again on the injected clock.
const HELD_POLL: Duration = Duration::from_millis(100);

/// A connection to `session`'s socket: the running session's, or, when
/// none accepts, a resumed one's. `session` has the minted shape. Under the
/// hub's resume gate the socket is tried first, so the starter is never
/// asked to resume a session whose socket accepts.
pub(crate) fn resume(hub: &Hub, session: &SessionId) -> Result<UnixStream, Refused> {
    reach(hub, session, true)
}

/// A connection to a resumed `session` whose log ends in `fiber_exited`
/// while its process still answers: a socket that accepts while the log
/// still ends so is the exiting process's, and is waited out.
pub(crate) fn resume_exited(hub: &Hub, session: &SessionId) -> Result<UnixStream, Refused> {
    reach(hub, session, false)
}

/// Whether `session`'s log ends in `fiber_exited`: it has exited, even
/// while its process is still shutting down.
pub(crate) fn exited(home: &Path, session: &SessionId) -> bool {
    find_log(home, session).is_some_and(|log| ends_exited(&log))
}

/// Whether `session`'s log is a delegate's. A log whose first line cannot
/// be read is not.
fn is_delegate(home: &Path, session: &SessionId) -> bool {
    find_log(home, session)
        .and_then(|log| recorded(&log))
        .is_some_and(|recorded| recorded.delegate)
}

fn ends_exited(log: &Path) -> bool {
    last_kind(log).as_deref() == Some("fiber_exited")
}

/// One pass of [`reach`]: an answer, or a wait for the exiting process,
/// with the `session_held` message its resume failed with, if any.
enum Step {
    Done(Result<UnixStream, Refused>),
    Wait(Option<String>),
}

/// The resume loop under the resume gate. A session whose log ends in
/// `fiber_exited` and is still held is tried again every [`HELD_POLL`],
/// until [`SHUTDOWN_BOUND`] after the first clock read; past it the client
/// gets `session_held`. With `trusted`, a socket that accepts on the first
/// pass is used as it is.
fn reach(hub: &Hub, session: &SessionId, trusted: bool) -> Result<UnixStream, Refused> {
    let socket = hub.home.join("run").join(&session.0);
    let _gate = hub
        .resume_gate
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let deadline = hub.clock.now() + SHUTDOWN_BOUND;
    let mut trusted = trusted;
    let mut held = None;
    loop {
        match attempt(hub, session, &socket, trusted) {
            Step::Done(result) => return result,
            Step::Wait(message) => held = message.or(held),
        }
        let now = hub.clock.now();
        if now >= deadline {
            return Err(Refused {
                code: ErrorCode::SessionHeld,
                message: held.unwrap_or_else(|| {
                    format!(
                        "Session {} is still held by its exiting process.",
                        session.0
                    )
                }),
            });
        }
        hub.tick.until(hub.clock.as_ref(), now + HELD_POLL);
        trusted = false;
    }
}

fn attempt(hub: &Hub, session: &SessionId, socket: &Path, trusted: bool) -> Step {
    if let Ok(stream) = UnixStream::connect(socket) {
        if trusted || !exited(&hub.home, session) {
            return Step::Done(Ok(stream));
        }
        // A delegate is never resumed, so its exiting process is not
        // waited out.
        if is_delegate(&hub.home, session) {
            return Step::Done(Err(delegate_refused()));
        }
        // The exiting process still answers on its socket.
        return Step::Wait(None);
    }
    let Some(log) = find_log(&hub.home, session) else {
        return Step::Done(Err(not_found(session)));
    };
    let recorded = match first_line(&log) {
        FirstLine::Recorded(recorded) => recorded,
        // The session has not bound its socket yet: its log is empty or
        // its first line is still being written.
        FirstLine::Incomplete => return Step::Done(Err(not_found(session))),
        FirstLine::Corrupt => {
            hub.diag.warn_session(
                session,
                "log_corrupt",
                &format!("Session {} has an unreadable log.", session.0),
            );
            return Step::Done(Err(Refused {
                code: ErrorCode::LogCorrupt,
                message: format!(
                    "The log of session `{}` does not start with its workspace.",
                    session.0
                ),
            }));
        }
    };
    if recorded.delegate {
        return Step::Done(Err(delegate_refused()));
    }
    let started = match hub.starter.resume(session, &recorded.workspace) {
        Ok(started) => started,
        Err(error) => return Step::Done(Err(io_failed(hub, session, &format!("{error}.")))),
    };
    Step::Done(match start::await_bind(hub, socket, started.as_ref()) {
        Bind::Connected(stream) => {
            hub.diag
                .info_session(session, "session_resumed", "Session resumed for local.");
            Ok(stream)
        }
        Bind::Exited => {
            let failure = started.exited();
            // The exiting process still holds the lock: wait it out.
            if let Some(failure) = &failure
                && failure.code == ErrorCode::SessionHeld
                && ends_exited(&log)
            {
                return Step::Wait(Some(failure.message.clone()));
            }
            // The process lost the session to another that bound first,
            // such as a resume another starter made: that one is running,
            // so the command is passed to it.
            match UnixStream::connect(socket) {
                Ok(stream) => Ok(stream),
                Err(_) => Err(match failure {
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
            }
        }
        Bind::TimedOut => Err(io_failed(hub, session, "it did not bind its socket.")),
        Bind::Failed(detail) => Err(io_failed(hub, session, &detail)),
    })
}

/// The rejection for a session the hub cannot find: `session_not_found`.
pub(crate) fn not_found(session: &SessionId) -> Refused {
    Refused {
        code: ErrorCode::SessionNotFound,
        message: format!("No session `{}`.", session.0),
    }
}

/// The rejection for a delegate that is not running: it resumes only
/// through its parent, so the hub has no session to reach.
fn delegate_refused() -> Refused {
    Refused {
        code: ErrorCode::SessionNotFound,
        message: "A delegate resumes only through its parent.".to_owned(),
    }
}

/// The first `projects/*/sessions/<session>/events.jsonl` under `home`, in
/// project-key order. `session` has the minted shape, so the path never
/// leaves the project's `sessions/`.
pub(crate) fn find_log(home: &Path, session: &SessionId) -> Option<PathBuf> {
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

/// What the log's first line, its `session_started`, recorded.
pub(crate) struct Recorded {
    /// The session's workspace.
    pub(crate) workspace: PathBuf,
    /// Whether it names a `parent` that is not `null`: a delegate's. A
    /// parent of any shape counts, so a malformed one is never resumed.
    delegate: bool,
}

/// What the log's first line holds: a complete `session_started` with a
/// workspace, no complete first line yet, or a corrupt line. Completeness
/// is decided on bytes before any decoding, so a line still being written
/// that ends inside a multi-byte character is incomplete, not corrupt.
/// An I/O error opening or reading the file is corrupt, as is a complete
/// line that does not decode, does not parse, is not `session_started`,
/// or names no string workspace.
enum FirstLine {
    Recorded(Recorded),
    Incomplete,
    Corrupt,
}

/// Reads the log's first line as bytes with `read_until(b'\n')`.
fn first_line(log: &Path) -> FirstLine {
    let file = match File::open(log) {
        Ok(file) => file,
        Err(_) => return FirstLine::Corrupt,
    };
    let mut bytes = Vec::new();
    match BufReader::new(file).read_until(b'\n', &mut bytes) {
        Ok(_) => {}
        Err(_) => return FirstLine::Corrupt,
    }
    if bytes.is_empty() || bytes.last() != Some(&b'\n') {
        return FirstLine::Incomplete;
    }
    let first = match String::from_utf8(bytes) {
        Ok(first) => first,
        Err(_) => return FirstLine::Corrupt,
    };
    let line: Value = match serde_json::from_str(&first) {
        Ok(line) => line,
        Err(_) => return FirstLine::Corrupt,
    };
    let kind = line.get("kind").and_then(Value::as_str);
    if kind != Some("session_started") {
        return FirstLine::Corrupt;
    }
    let payload = line.get("payload");
    let workspace = payload
        .and_then(|payload| payload.get("workspace"))
        .and_then(Value::as_str);
    match workspace {
        Some(workspace) => FirstLine::Recorded(Recorded {
            workspace: PathBuf::from(workspace),
            delegate: payload
                .and_then(|payload| payload.get("parent"))
                .is_some_and(|parent| !parent.is_null()),
        }),
        None => FirstLine::Corrupt,
    }
}

/// What the log's first line recorded. `None` when that line cannot be
/// read, is incomplete, is not `session_started` or holds no string
/// workspace.
pub(crate) fn recorded(log: &Path) -> Option<Recorded> {
    match first_line(log) {
        FirstLine::Recorded(recorded) => Some(recorded),
        FirstLine::Incomplete | FirstLine::Corrupt => None,
    }
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
