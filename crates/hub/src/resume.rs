//! Resuming a session a relayed command names (`docs/invocation.md`,
//! "Lifecycle"): a session whose socket accepts no connection is a log to
//! resume. The hub finds the log by the session's id, starts the internal
//! session command with `--resume` in the launch directory the log selects,
//! the recorded repository when the workspace is gone and the workspace
//! otherwise, and waits for the socket as `start` does. A session that is running is
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

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::time::Duration;

use contract::diag::Op;
use contract::{ErrorCode, JobId, SessionId};
use log::diag::Severity;
use serde_json::Value;

use crate::connection::{Hub, Refused};
use crate::start::{self, Bind};

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
pub(crate) fn resume(
    hub: &Hub,
    session: &SessionId,
    credential: Option<&str>,
) -> Result<UnixStream, Refused> {
    reach(hub, session, credential, true)
}

/// A connection to a resumed `session` whose log ends in `fiber_exited`
/// while its process still answers: a socket that accepts while the log
/// still ends so is the exiting process's, and is waited out.
pub(crate) fn resume_exited(
    hub: &Hub,
    session: &SessionId,
    credential: Option<&str>,
) -> Result<UnixStream, Refused> {
    reach(hub, session, credential, false)
}

/// Whether `session`'s log ends in `fiber_exited`: it has exited, even
/// while its process is still shutting down.
pub(crate) fn exited(home: &Path, session: &SessionId) -> bool {
    crate::recent::lookup_logged(home, session)
        .as_ref()
        .is_some_and(|(_, dir)| exited_in(dir))
}

/// Whether the log in `dir` ends in `fiber_exited`.
fn exited_in(dir: &Path) -> bool {
    log::last_kind(dir).as_deref() == Some("fiber_exited")
}

/// Whether the log in `dir` is a delegate's. A log whose first line
/// cannot be read is not.
fn is_delegate_in(dir: &Path) -> bool {
    recorded(dir).is_some_and(|recorded| recorded.delegate)
}

/// Whether `session` is a finished delegate whose parent holds no open run
/// for its job: its log's tail is `fiber_exited`, its `session_started`
/// names a parent session and job, and the parent's log holds no
/// `job_started` for that job without a later `job_completed`. A parent
/// with no log, or one whose log cannot be read, holds no open run. One
/// log lookup each, no locks: a `full` subscribe answered here replays the
/// log instead of resuming or refusing the delegate.
pub(crate) fn exited_delegate(home: &Path, session: &SessionId) -> bool {
    let Some((_, dir)) = crate::recent::lookup_logged(home, session) else {
        return false;
    };
    if !exited_in(&dir) {
        return false;
    }
    let Some((parent, job)) = delegate_job(&dir) else {
        return false;
    };
    !parent_run_open(home, &parent, &job)
}

/// The parent session and job the log's `session_started` names, when both
/// are strings: the delegate's identity for the resume-window check. A log
/// whose history cannot be read names none.
fn delegate_job(dir: &Path) -> Option<(SessionId, JobId)> {
    let segments = log::history(dir).ok()?;
    let started = segments.first()?.range(0, 1).ok()?;
    let first = started.first()?;
    if first.kind != "session_started" {
        return None;
    }
    let parent = first.payload.get("parent")?;
    let session = parent.get("session_id")?.as_str()?;
    let job = parent.get("delegate_id")?.as_str()?;
    Some((SessionId(session.to_owned()), JobId(job.to_owned())))
}

/// Whether the parent's log holds an open run for `job`: the latest
/// `job_started` or `job_completed` naming it is a start. A parent with
/// no log, or one whose log cannot be read, holds no open run. Other
/// lines naming the job (a `job_line` delivery, say) never close a run,
/// so only the two lifecycle kinds count.
fn parent_run_open(home: &Path, parent: &SessionId, job: &JobId) -> bool {
    let Some((_, dir)) = crate::recent::lookup_logged(home, parent) else {
        return false;
    };
    let Ok(segments) = log::history(&dir) else {
        return false;
    };
    for segment in segments.iter().rev() {
        let Ok(lines) = segment.lines(0) else {
            return false;
        };
        if let Some(started) = lines.iter().rev().find_map(|line| {
            if line.payload.get("job_id").and_then(Value::as_str) != Some(job.0.as_str()) {
                return None;
            }
            match line.kind.as_str() {
                "job_started" => Some(true),
                "job_completed" => Some(false),
                _ => None,
            }
        }) {
            return started;
        }
    }
    false
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
fn reach(
    hub: &Hub,
    session: &SessionId,
    credential: Option<&str>,
    trusted: bool,
) -> Result<UnixStream, Refused> {
    // Admission first: the guard spans the whole reach, so a sessions
    // stop stored before this refuses it, and one stored after waits
    // for this bind in its zero-wait. Declared before the gate: the
    // guard's drop writes after the gate releases, and every attempt
    // shares this one guard.
    let _admitted = hub.admit()?;
    let _timer = log::diag::timed(&hub.diag, Op::HubResume, None, None);
    let socket = hub.home.join("run").join(&session.0);
    let _gate = hub
        .resume_gate
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let deadline = hub.clock.now() + SHUTDOWN_BOUND;
    let mut trusted = trusted;
    let mut held = None;
    loop {
        // Re-checked each pass: a waiter queued behind the resume gate
        // leaves within one pass after the store.
        if hub.sessions_stopping.load(Ordering::SeqCst) {
            return Err(crate::connection::stopping());
        }
        match attempt(hub, session, &socket, credential, trusted) {
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

fn attempt(
    hub: &Hub,
    session: &SessionId,
    socket: &Path,
    credential: Option<&str>,
    trusted: bool,
) -> Step {
    if let Ok(stream) = UnixStream::connect(socket) {
        if trusted {
            return Step::Done(Ok(stream));
        }
        // One lookup per pass, shared by both predicates: a refused
        // connect resumes without one.
        let found = crate::recent::lookup_logged(&hub.home, session);
        if found.as_ref().is_none_or(|(_, dir)| !exited_in(dir)) {
            return Step::Done(Ok(stream));
        }
        // A delegate is never resumed, so its exiting process is not
        // waited out.
        if found.as_ref().is_some_and(|(_, dir)| is_delegate_in(dir)) {
            return Step::Done(Err(delegate_refused()));
        }
        // The exiting process still answers on its socket.
        return Step::Wait(None);
    }
    let Some((_, dir)) = crate::recent::lookup_logged(&hub.home, session) else {
        return Step::Done(Err(not_found(session)));
    };
    let recorded = match log::started_first(&dir) {
        Ok(view) => {
            let Some(workspace) = view.workspace else {
                hub.diag.line(
                    Severity::Warn,
                    Some(session),
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
            Recorded {
                workspace: PathBuf::from(workspace),
                repository: view.repository.map(PathBuf::from),
                delegate: view.delegate,
            }
        }
        // The session has not bound its socket yet: its log is empty or
        // its first line is still being written.
        Err(log::FirstLineError::Incomplete) => return Step::Done(Err(not_found(session))),
        Err(log::FirstLineError::Corrupt) => {
            hub.diag.line(
                Severity::Warn,
                Some(session),
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
    // A workspace that is missing with no recorded repository to make it
    // from cannot be recovered: refuse before starting a process.
    if recorded.repository.is_none() && missing(&recorded.workspace) {
        return Step::Done(Err(io_failed(
            hub,
            session,
            &format!("{} no longer exists", recorded.workspace.display()),
        )));
    }
    // Whether the launch target was already missing before the start
    // was tried: a starter failure then is the confirmed unrecoverable
    // case, and the hub names it with the ruled shape, without the
    // starter-error wrapper's period. A target that was present is the
    // race the ruling leaves alone, so its refusal keeps the wrapper's
    // period and renders like any other refusal. This observes only: the
    // hub never refuses on the launch target, whose repository a test
    // fake may never create.
    let launch_was_missing = missing(recorded.launch());
    let started = match hub.starter.resume(session, recorded.launch(), credential) {
        Ok(started) => started,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && launch_was_missing => {
            return Step::Done(Err(io_failed(hub, session, &error.to_string())));
        }
        Err(error) => return Step::Done(Err(io_failed(hub, session, &format!("{error}.")))),
    };
    Step::Done(match start::await_bind(hub, socket, started.as_ref()) {
        Bind::Connected(stream) => {
            hub.diag.line(
                Severity::Info,
                Some(session),
                "session_resumed",
                "Session resumed for local.",
            );
            Ok(stream)
        }
        Bind::Exited => {
            let failure = started.exited();
            // The exiting process still holds the lock: wait it out.
            if let Some(failure) = &failure
                && failure.code == ErrorCode::SessionHeld
                && exited_in(&dir)
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
                        hub.diag.line(
                            Severity::Warn,
                            Some(session),
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
        Bind::Stopping => {
            started.kill();
            Err(crate::connection::stopping())
        }
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

/// What the log's first line, its `session_started`, recorded, over
/// [`log::started_first`]. `None` when that line cannot be read, is
/// incomplete, is not `session_started` or holds no string workspace.
pub(crate) struct Recorded {
    /// The session's workspace.
    pub(crate) workspace: PathBuf,
    /// The repository the recorded worktree came from, when its
    /// `session_started` recorded one.
    repository: Option<PathBuf>,
    /// Whether it names a `parent` that is not `null`: a delegate's. A
    /// parent of any shape counts, so a malformed one is never resumed.
    delegate: bool,
}

impl Recorded {
    /// Where the resumed process starts: the recorded repository when the
    /// workspace is gone and a repository is recorded, else the workspace.
    /// Anything but a missing workspace keeps the workspace: a dangling
    /// symlink at the path exists, and any other lookup error resumes
    /// where the worktree is.
    pub(crate) fn launch(&self) -> &Path {
        match self.repository.as_ref() {
            Some(repository) if missing(&self.workspace) => repository,
            Some(_) | None => &self.workspace,
        }
    }
}

/// Whether `path` is missing: only `NotFound` counts, so any other
/// lookup error keeps the workspace. The hub's resume guard and the real
/// starter's resume check share it.
pub(crate) fn missing(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_err_and(|source| source.kind() == std::io::ErrorKind::NotFound)
}

/// What the log in `dir` recorded: its workspace, repository and
/// delegate bit. `None` when that line cannot be read, is incomplete,
/// is not `session_started` or holds no string workspace.
pub(crate) fn recorded(dir: &Path) -> Option<Recorded> {
    let view = log::started_first(dir).ok()?;
    Some(Recorded {
        workspace: PathBuf::from(view.workspace?),
        repository: view.repository.map(PathBuf::from),
        delegate: view.delegate,
    })
}

fn io_failed(hub: &Hub, session: &SessionId, detail: &str) -> Refused {
    // The log keeps the code and a fixed sentence: `detail` may hold an io
    // error or a path. The rejection keeps what happened.
    hub.diag.line(
        Severity::Warn,
        Some(session),
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
