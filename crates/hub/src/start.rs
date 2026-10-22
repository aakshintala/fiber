//! `start`: minting the session id, running the session command, waiting
//! for `run/<id>` to accept, and holding `content` as the first prompt.
//!
//! The hub mints the id with the session-id format and starts the internal
//! session command, never `--prompt`. It waits for `run/<id>` to accept a
//! connection before answering. With `content`, it subscribes `summary` on
//! a connection of its own before answering, and sends the prompt on that
//! connection after answering (`crate::first`).

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use contract::diag::Op;
use contract::{CommandId, ErrorCode, SessionId};
use log::diag::Severity;
use serde_json::Value;

use crate::connection::{Hub, Refused};

/// How long a session started by `start` has to bind `run/<id>` or exit.
pub(crate) const START_DEADLINE: Duration = Duration::from_secs(30);

/// How often `start` retries the session's socket on the injected clock:
/// the step the bind-wait tests park on.
pub(crate) const START_POLL: Duration = Duration::from_millis(10);

/// How long the `content` handshake waits for the session's acknowledgement.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// What `start` answers with.
pub(crate) enum Outcome {
    /// The session bound its socket, and accepted the hub's `summary`
    /// subscription when `content` was sent.
    Accepted {
        /// The minted session id.
        session_id: SessionId,
        /// The first prompt, still to send: `Some` exactly when `content`
        /// was sent.
        first: Option<Box<Held>>,
    },
    /// The `start` is rejected: why.
    Rejected(Refused),
}

/// The hub's own `summary` connection to a started session, subscribed,
/// with the first prompt it will send and the `start` that asked for it.
pub(crate) struct Held {
    stream: UnixStream,
    id: SessionId,
    command: CommandId,
    content: Value,
    started: Box<dyn crate::Started>,
}

/// Runs `start` (`command`) for `workspace`, with `model` and `content`
/// when the client named them, each of `overrides` passed to the session
/// as `-c`, in a new worktree of the workspace with
/// `worktree` (`docs/invocation.md`, "Isolation"). `workspace` is
/// absolute and an existing directory; anything else is
/// `invalid_arguments`. With `continues` naming a session, the session
/// runs in that session's recorded worktree instead: `continues` naming
/// the new session itself, `continues` with `worktree` true, a session
/// the hub does not know, a session with no recorded worktree, or a
/// worktree that is not the workspace is `invalid_arguments`, while a
/// missing or unreadable log is `session_not_found` and an unreadable
/// first line is `log_corrupt`.
/// With `content`, the
/// prompt is held, not sent: the caller answers first, then sends it with
/// [`prompt`].
#[allow(
    clippy::too_many_arguments,
    reason = "the start call takes each launch option once"
)]
pub(crate) fn run(
    hub: &Hub,
    command: &CommandId,
    workspace: &str,
    model: Option<&str>,
    overrides: &[&str],
    worktree: bool,
    content: Option<&Value>,
    continues: Option<&SessionId>,
) -> Outcome {
    // Admission first: the increment precedes the flag load, so a
    // sessions stop stored before this refuses it, and one stored
    // after waits for this bind in its zero-wait. The guard drops only
    // after `await_bind` returns, and on any arm but `Connected` after
    // `started.kill()`.
    let _admitted = match hub.admit() {
        Ok(admitted) => admitted,
        Err(refused) => return Outcome::Rejected(refused),
    };
    // Stamped at bind, emitted at return: a failure measures through
    // the failure, and an invalid request is an entered start.
    let mut timer = log::diag::timed(&hub.diag, Op::HubSessionStart, None, None);
    let workspace_path = Path::new(workspace);
    if !workspace_path.is_absolute() {
        return invalid("The workspace is not an absolute path.");
    }
    if !workspace_path.is_dir() {
        return invalid("The workspace is not an existing directory.");
    }
    let id = SessionId(mint("s_"));
    if let Some(old) = continues {
        if worktree {
            return invalid(
                "A start that continues a session keeps that session's worktree: it takes no new one.",
            );
        }
        if let Err(outcome) = check_continues(hub, &id, workspace, old) {
            return outcome;
        }
    }
    let started =
        match hub
            .starter
            .start(&id, workspace_path, model, overrides, worktree, continues)
        {
            Ok(started) => started,
            Err(error) => {
                return io_failed(hub, &id, &format!("{error}."));
            }
        };
    let socket = hub.home.join("run").join(&id.0);
    let stream = match await_bind(hub, &socket, started.as_ref()) {
        Bind::Connected(stream) => {
            // The session's socket accepts here: the content handshake
            // after this is its own wait, not this start's.
            if let Some(timer) = timer.as_mut() {
                timer.stop();
            }
            stream
        }
        Bind::Exited => return exited_or_io(hub, &id, started.as_ref()),
        Bind::Stopping => {
            started.kill();
            return Outcome::Rejected(crate::connection::stopping());
        }
        Bind::TimedOut => {
            // The guard releases after the kill below, as on every
            // other unsuccessful arm: a child that outlives the wait
            // must never bind after the stop's scans.
            started.kill();
            return io_failed(hub, &id, "it did not bind its socket.");
        }
        Bind::Failed(detail) => {
            started.kill();
            return io_failed(hub, &id, &detail);
        }
    };
    let first = match content {
        Some(content) => {
            if let Err(outcome) = subscribe_summary(&stream, &id, started.as_ref(), hub) {
                return outcome;
            }
            Some(Box::new(Held {
                stream,
                id: id.clone(),
                command: command.clone(),
                content: content.clone(),
                started,
            }))
        }
        None => None,
    };
    hub.diag.line(
        Severity::Info,
        Some(&id),
        "session_started",
        "Session started for local.",
    );
    Outcome::Accepted {
        session_id: id,
        first,
    }
}

/// How waiting for a started session process to bind its socket ended.
pub(crate) enum Bind {
    /// `run/<id>` accepted a connection.
    Connected(UnixStream),
    /// The process exited before its socket accepted.
    Exited,
    /// [`START_DEADLINE`] passed with the socket missing or refusing.
    TimedOut,
    /// A sessions stop landed while the socket was missing or refusing:
    /// the starter is killed, the bind refused. Only from the retry
    /// branch, never when the socket accepts.
    Stopping,
    /// Connecting failed in a way waiting cannot mend: the error, as a
    /// sentence.
    Failed(String),
}

/// Waits for the process `started` to bind `socket`, retrying every
/// [`START_POLL`] on the injected clock until [`START_DEADLINE`]: shared by
/// `start` and the resume a relayed command makes.
pub(crate) fn await_bind(hub: &Hub, socket: &Path, started: &dyn crate::Started) -> Bind {
    let deadline = hub.clock.now() + START_DEADLINE;
    loop {
        if started.exited().is_some() {
            return Bind::Exited;
        }
        match UnixStream::connect(socket) {
            Ok(stream) => return Bind::Connected(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                if hub.sessions_stopping.load(Ordering::SeqCst) {
                    return Bind::Stopping;
                }
                if hub.clock.now() >= deadline {
                    return Bind::TimedOut;
                }
                hub.clock.sleep(START_POLL);
            }
            Err(error) => return Bind::Failed(format!("{error}.")),
        }
    }
}

/// Subscribes `summary` on the hub's own connection: a session answers no
/// other command before `subscribe`, and `summary` never counts in
/// `clients`. A rejection is the `start` rejection, carrying the session's
/// code and message.
fn subscribe_summary(
    stream: &UnixStream,
    id: &SessionId,
    started: &dyn crate::Started,
    hub: &Hub,
) -> Result<(), Outcome> {
    if stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).is_err() {
        return Err(io_failed(hub, id, "its acknowledgement could not be read."));
    }
    let subscribe_id = mint("c_");
    let subscribe = serde_json::json!({
        "id": subscribe_id,
        "command": "subscribe",
        "args": { "level": "summary" },
    });
    if write_line(stream, &subscribe).is_err() {
        return Err(exited_or_io(hub, id, started));
    }
    match acknowledge(stream, &subscribe_id) {
        Ack::Accepted => Ok(()),
        Ack::Rejected(refused) => {
            // The log keeps the code and a fixed sentence: the session's
            // message may hold model text. The rejection keeps what the
            // session said.
            hub.diag.line(
                Severity::Warn,
                Some(id),
                &code_name(&refused.code),
                &format!("Session {} rejected the hub's subscription.", id.0),
            );
            Err(Outcome::Rejected(refused))
        }
        Ack::Lost => Err(exited_or_io(hub, id, started)),
    }
}

/// Sends the held prompt and reads the session's acknowledgement for it.
/// `start` is already answered, so a rejection, an exit or a failed read
/// is written to the hub log with the session id, the `start` command id
/// and the code, never the session's message, and is returned for tests.
pub(crate) fn prompt(held: Held, hub: &Hub) -> Result<(), Outcome> {
    let Held {
        stream,
        id,
        command,
        content,
        started,
    } = held;
    let prompt_id = mint("c_");
    let line = serde_json::json!({
        "id": prompt_id,
        "command": "prompt",
        "args": { "content": content },
    });
    let ack = if write_line(&stream, &line).is_err() {
        Ack::Lost
    } else {
        acknowledge(&stream, &prompt_id)
    };
    let (code, message, sentence) = match ack {
        Ack::Accepted => return Ok(()),
        Ack::Rejected(refused) => (refused.code, refused.message, "rejected"),
        Ack::Lost => match started.exited() {
            Some(failure) => (failure.code, failure.message, "exited before it took"),
            None => (
                ErrorCode::IoFailed,
                format!("Session {} did not take its first prompt.", id.0),
                "did not take",
            ),
        },
    };
    hub.diag.line(
        Severity::Warn,
        Some(&id),
        &code_name(&code),
        &format!(
            "Session {} {sentence} the first prompt of command {}.",
            id.0, command.0
        ),
    );
    Err(Outcome::Rejected(Refused { code, message }))
}

/// How the session answered one of the hub's own commands.
enum Ack {
    Accepted,
    Rejected(Refused),
    /// The connection ended, or could not be read, first.
    Lost,
}

/// Reads lines until the session's acknowledgement for `command_id`,
/// skipping every other line.
fn acknowledge(stream: &UnixStream, command_id: &str) -> Ack {
    let Ok(clone) = stream.try_clone() else {
        return Ack::Lost;
    };
    let mut read = BufReader::new(clone);
    loop {
        let buf = match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => return Ack::Lost,
            Ok(Some(buf)) => buf,
        };
        let ack: Value = match serde_json::from_slice(&buf) {
            Ok(ack) => ack,
            Err(_) => continue,
        };
        let named = ack
            .get("payload")
            .and_then(|payload| payload.get("command_id"))
            .and_then(Value::as_str)
            == Some(command_id);
        if !named {
            continue;
        }
        match ack.get("kind").and_then(Value::as_str) {
            Some("command_accepted") => return Ack::Accepted,
            Some("command_rejected") => {
                let payload = ack.get("payload");
                let code = payload
                    .and_then(|payload| payload.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("io_failed");
                let code: ErrorCode = serde_json::from_value(Value::String(code.into()))
                    .unwrap_or(ErrorCode::IoFailed);
                let message = payload
                    .and_then(|payload| payload.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("The session rejected the command.")
                    .to_owned();
                return Ack::Rejected(Refused { code, message });
            }
            _ => continue,
        }
    }
}

/// The session exited first with a failure, or `io_failed` naming it when
/// there is none. The log keeps the code and a fixed sentence: a failure's
/// message may hold prompt or model text. The rejection keeps what the
/// session said.
fn exited_or_io(hub: &Hub, id: &SessionId, started: &dyn crate::Started) -> Outcome {
    match started.exited() {
        Some(failure) => {
            hub.diag.line(
                Severity::Warn,
                Some(id),
                &code_name(&failure.code),
                &format!("Session {} exited before it answered.", id.0),
            );
            Outcome::Rejected(Refused {
                code: failure.code,
                message: failure.message,
            })
        }
        None => io_failed(hub, id, "its acknowledgement could not be read."),
    }
}

pub(crate) fn io_failed(hub: &Hub, id: &SessionId, detail: &str) -> Outcome {
    // The log keeps the code and a fixed sentence: `detail` may hold an
    // io error, a path, or model text. The rejection keeps what happened.
    hub.diag.line(
        Severity::Warn,
        Some(id),
        "io_failed",
        &format!("Session {} could not start.", id.0),
    );
    Outcome::Rejected(Refused {
        code: ErrorCode::IoFailed,
        message: format!("Session {} could not start: {detail}", id.0),
    })
}

/// Checks the `continues` session `old` names for the new session `new`:
/// naming itself is `invalid_arguments`, before any log is read. Its
/// log's recorded worktree must identify `workspace`, so the new session
/// runs where the old one does (`docs/invocation.md`, "Isolation").
/// `Ok` carries nothing; `Err` carries the refusal. A session the hub
/// does not know, or whose first line is still being written, is
/// `session_not_found`, as for a missing log; an unreadable first line
/// is `log_corrupt`; anything else that is not the workspace is
/// `invalid_arguments`.
fn check_continues(
    hub: &Hub,
    new: &SessionId,
    workspace: &str,
    old: &SessionId,
) -> Result<(), Outcome> {
    if old == new {
        return Err(invalid(&format!(
            "Session `{}` cannot continue itself.",
            new.0
        )));
    }
    let Some((_, dir)) = crate::recent::lookup_logged(&hub.home, old) else {
        return Err(Outcome::Rejected(crate::resume::not_found(old)));
    };
    let recorded = match log::started_first(&dir) {
        Ok(recorded) => recorded,
        Err(log::FirstLineError::Incomplete) => {
            return Err(Outcome::Rejected(crate::resume::not_found(old)));
        }
        Err(log::FirstLineError::Corrupt) => {
            hub.diag.line(
                Severity::Warn,
                Some(old),
                "log_corrupt",
                &format!("Session {} has an unreadable log.", old.0),
            );
            return Err(Outcome::Rejected(Refused {
                code: ErrorCode::LogCorrupt,
                message: format!(
                    "The log of session `{}` does not start with its workspace.",
                    old.0
                ),
            }));
        }
    };
    match recorded.worktree_record() {
        Ok(None) => {
            return Err(invalid(&format!(
                "Session `{}` runs in no worktree to continue.",
                old.0
            )));
        }
        Err(_) => {
            hub.diag.line(
                Severity::Warn,
                Some(old),
                "log_corrupt",
                &format!("Session {} has an unreadable log.", old.0),
            );
            return Err(Outcome::Rejected(Refused {
                code: ErrorCode::LogCorrupt,
                message: format!(
                    "The log of session `{}` does not start with its workspace.",
                    old.0
                ),
            }));
        }
        Ok(Some(record)) => {
            if record.path != workspace {
                return Err(invalid(&format!(
                    "Session `{}` runs in a worktree that is not the workspace.",
                    old.0
                )));
            }
        }
    }
    Ok(())
}

fn invalid(message: &str) -> Outcome {
    Outcome::Rejected(Refused {
        code: ErrorCode::InvalidArguments,
        message: message.to_owned(),
    })
}

fn write_line(stream: &UnixStream, line: &Value) -> std::io::Result<()> {
    // The hub's own line carries no line feed; the writer adds it.
    let bytes = serde_json::to_vec(line)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut out: &UnixStream = stream;
    support::framing::write_line(&mut out, &bytes)
}

pub(crate) use support::mint;

pub(crate) fn code_name(code: &ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "io_failed".to_owned())
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
