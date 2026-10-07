//! `start`: minting the session id, running the session command, waiting
//! for `run/<id>` to accept, and delivering `content` as the first prompt.
//!
//! The hub mints the id with the session-id format and starts the internal
//! session command, never `--prompt`. It waits for `run/<id>` to accept a
//! connection before answering.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::connection::Hub;

/// How long a session started by `start` has to bind `run/<id>` or exit.
pub(crate) const START_DEADLINE: Duration = Duration::from_secs(30);

/// How often `start` retries the session's socket on the injected clock.
const START_POLL: Duration = Duration::from_millis(10);

/// How long the `content` handshake waits for the session's acknowledgement.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// What `start` answers with.
pub(crate) enum Outcome {
    /// The session bound its socket, and accepted the first prompt when
    /// `content` was sent.
    Accepted {
        /// The minted session id.
        session_id: SessionId,
    },
    /// The `start` is rejected with this code and message.
    Rejected {
        /// The rejection code.
        code: ErrorCode,
        /// Fiber's own sentence.
        message: String,
    },
}

/// Runs `start` for `workspace`, with `model` and `content` when the client
/// named them. `workspace` is absolute and an existing directory; anything
/// else is `invalid_arguments`.
pub(crate) fn run(
    hub: &Hub,
    workspace: &str,
    model: Option<&str>,
    content: Option<&Value>,
) -> Outcome {
    let workspace_path = Path::new(workspace);
    if !workspace_path.is_absolute() {
        return invalid("The workspace is not an absolute path.");
    }
    if !workspace_path.is_dir() {
        return invalid("The workspace is not an existing directory.");
    }
    let id = SessionId(mint("s_"));
    let started = match hub.starter.start(&id, workspace_path, model) {
        Ok(started) => started,
        Err(error) => {
            return io_failed(hub, &id, &format!("{error}."));
        }
    };
    let socket = hub.home.join("run").join(&id.0);
    let deadline = hub.clock.now() + START_DEADLINE;
    let stream = loop {
        if started.exited().is_some() {
            return exited_or_io(hub, &id, started.as_ref());
        }
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                if hub.clock.now() >= deadline {
                    return io_failed(hub, &id, "it did not bind its socket.");
                }
                hub.clock.sleep(START_POLL);
            }
            Err(error) => {
                return io_failed(hub, &id, &format!("{error}."));
            }
        }
    };
    if let Some(content) = content {
        match deliver(stream, &id, content, started.as_ref(), hub) {
            Ok(()) => {}
            Err(outcome) => return outcome,
        }
    }
    hub.diag
        .info_session(&id, "session_started", "Session started for local.");
    Outcome::Accepted { session_id: id }
}

/// Sends `content` as the session's first prompt on a short connection of
/// the hub's own, and reads lines until the session's acknowledgement for
/// it. The connection subscribes `summary` first: a session answers no
/// other command before `subscribe`, and `summary` never counts in
/// `clients`. Accepted: `Ok`. Rejected: the `start` rejection, carrying the
/// session's code and message.
fn deliver(
    stream: UnixStream,
    id: &SessionId,
    content: &Value,
    started: &dyn crate::Started,
    hub: &Hub,
) -> Result<(), Outcome> {
    let mut stream = stream;
    if stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).is_err() {
        return Err(io_failed(hub, id, "its acknowledgement could not be read."));
    }
    let subscribe_id = mint("c_");
    let subscribe = serde_json::json!({
        "id": subscribe_id,
        "command": "subscribe",
        "args": { "level": "summary" },
    });
    if write_line(&mut stream, &subscribe).is_err() {
        return Err(exited_or_io(hub, id, started));
    }
    acknowledge(&stream, &subscribe_id, started, hub, id)?;
    let prompt_id = mint("c_");
    let line = serde_json::json!({
        "id": prompt_id,
        "command": "prompt",
        "args": { "content": content },
    });
    if write_line(&mut stream, &line).is_err() {
        return Err(exited_or_io(hub, id, started));
    }
    acknowledge(&stream, &prompt_id, started, hub, id)
}
/// Reads lines until the session's acknowledgement for `command_id`:
/// `Ok` when accepted, the `start` rejection with the session's code and
/// message when rejected.
fn acknowledge(
    stream: &UnixStream,
    command_id: &str,
    started: &dyn crate::Started,
    hub: &Hub,
    id: &SessionId,
) -> Result<(), Outcome> {
    let mut read = BufReader::new(
        stream
            .try_clone()
            .map_err(|_| io_failed(hub, id, "its acknowledgement could not be read."))?,
    );
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return Err(exited_or_io(hub, id, started)),
            Ok(_) => {}
        }
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
            Some("command_accepted") => return Ok(()),
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
                    .unwrap_or("The session rejected its first prompt.")
                    .to_owned();
                // The log keeps the code and a fixed sentence: the session's
                // message may hold prompt or model text. The rejection keeps
                // what the session said.
                hub.diag.warn_session(
                    id,
                    &code_name(&code),
                    &format!("Session {} rejected its first prompt.", id.0),
                );
                return Err(Outcome::Rejected { code, message });
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
            hub.diag.warn_session(
                id,
                &code_name(&failure.code),
                &format!("Session {} exited before it answered.", id.0),
            );
            Outcome::Rejected {
                code: failure.code,
                message: failure.message,
            }
        }
        None => io_failed(hub, id, "its acknowledgement could not be read."),
    }
}

fn io_failed(hub: &Hub, id: &SessionId, detail: &str) -> Outcome {
    // The log keeps the code and a fixed sentence: `detail` may hold an
    // io error, a path, or model text. The rejection keeps what happened.
    hub.diag.warn_session(
        id,
        "io_failed",
        &format!("Session {} could not start.", id.0),
    );
    Outcome::Rejected {
        code: ErrorCode::IoFailed,
        message: format!("Session {} could not start: {detail}", id.0),
    }
}

fn invalid(message: &str) -> Outcome {
    Outcome::Rejected {
        code: ErrorCode::InvalidArguments,
        message: message.to_owned(),
    }
}

fn write_line(stream: &mut UnixStream, line: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(line)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()
}

/// A new id from random bytes, as `doors::mint` makes one. `hub` keeps its
/// own copy because it may not depend on `doors`.
fn mint(prefix: &str) -> String {
    format!("{prefix}{:016x}", RandomState::new().hash_one(()))
}

pub(crate) fn code_name(code: &ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "io_failed".to_owned())
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
