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

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::connection::Hub;
use crate::feed::last_kind;
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
    let mut pause: Option<Arc<Pause>> = None;
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
        pause
            .get_or_insert_with(|| Pause::subscribed(hub.clock.as_ref()))
            .until(hub.clock.as_ref(), now + HELD_POLL);
        trusted = false;
    }
}

fn attempt(hub: &Hub, session: &SessionId, socket: &Path, trusted: bool) -> Step {
    if let Ok(stream) = UnixStream::connect(socket) {
        if trusted || !exited(&hub.home, session) {
            return Step::Done(Ok(stream));
        }
        // The exiting process still answers on its socket.
        return Step::Wait(None);
    }
    let Some(log) = find_log(&hub.home, session) else {
        return Step::Done(Err(not_found(session)));
    };
    let Some(workspace) = recorded_workspace(&log) else {
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
    };
    let started = match hub.starter.resume(session, &workspace) {
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

/// A wait on the injected clock, woken by every clock move: a fake clock
/// parks it until a test advances past its instant.
#[derive(Default)]
struct Pause {
    held: Mutex<()>,
    moved: Condvar,
}

impl Wake for Pause {
    fn wake(&self) {
        // Taken before the notify, so a waiter that has checked and not
        // yet parked cannot miss it.
        let _held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        self.moved.notify_all();
    }
}

impl Pause {
    fn subscribed(clock: &dyn Clock) -> Arc<Self> {
        let pause = Arc::new(Self::default());
        let wake: Arc<dyn Wake> = Arc::clone(&pause) as Arc<dyn Wake>;
        clock.subscribe(Arc::downgrade(&wake));
        pause
    }

    /// Returns once `clock` reads `until` or later.
    fn until(&self, clock: &dyn Clock, until: Instant) {
        loop {
            let guard = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            if clock.now() >= until {
                return;
            }
            let mut slot = Some(guard);
            clock.wait_until(Some(until), &mut |bound| {
                let Some(guard) = slot.take() else {
                    return;
                };
                slot = Some(match bound {
                    Some(limit) => {
                        self.moved
                            .wait_timeout(guard, limit)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                    None => self
                        .moved
                        .wait(guard)
                        .unwrap_or_else(PoisonError::into_inner),
                });
            });
        }
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
