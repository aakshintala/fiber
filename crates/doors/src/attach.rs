//! Attaching to a running session (`docs/invocation.md`, "Processes"): a
//! `full` subscription folds the log by `seq` first, then streams, and one
//! `prompt` command starts the turn this process prints. Attach never sends
//! `close` and never opens the log: the holder keeps the only writer, and a
//! disconnect only ends the client.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use contract::shapes::Failure;
use contract::{ErrorCode, SCHEMA_VERSION, SessionId};
use serde_json::Value;

use crate::{failure, mint};

/// Sends `prompt` to the session `id` running under `home`, prints the
/// lines from the `turn_started` carrying this prompt's command id through
/// that turn's `turn_completed`, and nothing before it, and returns 0 for a
/// `completed` turn and 1 for any other outcome. A `command_rejected` for
/// the prompt is the rejection's code and message, printing nothing. A
/// closed connection or a `fiber_exited` before the turn completes is a
/// failure, not a hang.
pub fn attach(
    home: &Path,
    id: &SessionId,
    prompt: String,
    out: &mut dyn Write,
) -> Result<i32, Failure> {
    let socket = home.join("run").join(&id.0);
    let stream = UnixStream::connect(&socket).map_err(|_| held(home, id))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| io_failed(&socket, &e))?);
    let mut writer = stream;

    let sub = mint("c_");
    send(
        &mut writer,
        id,
        &sub,
        "subscribe",
        serde_json::json!({"level": "full"}),
    )?;
    // The acknowledgement is written before the writer starts, so it is the
    // first line this connection reads, ahead of the fold.
    loop {
        let Some((_, line)) = next_line(&mut reader) else {
            return Err(ended(id));
        };
        if is_answer(&line, "command_rejected", &sub) {
            return Err(rejection(&line));
        }
        if is_answer(&line, "command_accepted", &sub) {
            check_version(id, &line)?;
            break;
        }
    }

    let command = mint("c_");
    send(
        &mut writer,
        id,
        &command,
        "prompt",
        serde_json::json!({"content": [{"type": "text", "text": prompt}]}),
    )?;
    let mut printing = false;
    loop {
        let Some((raw, line)) = next_line(&mut reader) else {
            return Err(ended(id));
        };
        if is_answer(&line, "command_rejected", &command) {
            return Err(rejection(&line));
        }
        if starts_own_turn(&line, &command) {
            printing = true;
        } else if printing && kind(&line) == "turn_completed" {
            emit(out, &raw)?;
            return Ok(outcome(&line));
        } else if printing && kind(&line) == "fiber_exited" {
            return Err(ended(id));
        }
        if printing {
            emit(out, &raw)?;
        }
    }
}

/// Sends one command line. A write that fails means the session is gone.
fn send(
    writer: &mut UnixStream,
    id: &SessionId,
    command: &str,
    name: &str,
    args: Value,
) -> Result<(), Failure> {
    let mut bytes = serde_json::to_vec(&serde_json::json!({
        "id": command,
        "command": name,
        "args": args,
    }))
    .map_err(|e| {
        failure(
            ErrorCode::IoFailed,
            format!("a command could not be written as JSON: {e}"),
        )
    })?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|_| ended(id))
}

/// The next line from the session, with its bytes for printing. A line that
/// is not JSON is skipped: the session writes JSON, and a stray line must
/// not end the attach. `None` is a closed connection.
fn next_line(reader: &mut BufReader<UnixStream>) -> Option<(Vec<u8>, Value)> {
    loop {
        let mut raw = Vec::new();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        if raw.ends_with(b"\n") {
            raw.pop();
        }
        if let Ok(line) = serde_json::from_slice::<Value>(&raw) {
            return Some((raw, line));
        }
    }
}

fn kind(line: &Value) -> &str {
    line.get("kind").and_then(Value::as_str).unwrap_or("")
}

/// Whether `line` answers `command` with `kind`.
fn is_answer(line: &Value, kind: &str, command: &str) -> bool {
    self::kind(line) == kind
        && line.pointer("/payload/command_id").and_then(Value::as_str) == Some(command)
}

/// Whether `line` is the `turn_started` carrying this prompt's command id in
/// its input.
fn starts_own_turn(line: &Value, command: &str) -> bool {
    self::kind(line) == "turn_started"
        && line
            .pointer("/payload/input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("command_id").and_then(Value::as_str) == Some(command))
            })
}

/// The exit code for `line`'s outcome: 0 for `completed`, 1 for the rest.
fn outcome(line: &Value) -> i32 {
    if line.pointer("/payload/outcome").and_then(Value::as_str) == Some("completed") {
        0
    } else {
        1
    }
}

/// Prints one line of the turn.
fn emit(out: &mut dyn Write, raw: &[u8]) -> Result<(), Failure> {
    out.write_all(raw)
        .and_then(|()| out.write_all(b"\n"))
        .map_err(|e| {
            failure(
                ErrorCode::IoFailed,
                format!("stdout could not be written: {e}"),
            )
        })
}

fn rejection(line: &Value) -> Failure {
    failure(rejection_code(line), rejection_message(line))
}

fn rejection_code(line: &Value) -> ErrorCode {
    line.pointer("/payload/code")
        .cloned()
        .and_then(|code| serde_json::from_value(code).ok())
        .unwrap_or(ErrorCode::Other("unknown".to_owned()))
}

fn rejection_message(line: &Value) -> String {
    line.pointer("/payload/message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Declines on a `schema_version` different from this build's
/// (`docs/invocation.md`, "Processes"): an additive difference never bumps
/// the version (`docs/events.md`, "Versioning"), so any other one cannot be
/// read. Another process does hold the session, so the code stays
/// `session_held`, and the message names the session's version and this one.
fn check_version(id: &SessionId, line: &Value) -> Result<(), Failure> {
    let version = line
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if u64::from(SCHEMA_VERSION) != version {
        return Err(failure(
            ErrorCode::SessionHeld,
            format!(
                "session {} runs schema version {version}, this Fiber runs schema version {}; \
                 close the session or let it exit, then resume",
                id.0, SCHEMA_VERSION,
            ),
        ));
    }
    Ok(())
}

/// The session ended before answering: its connection closed, or its
/// `fiber_exited` arrived before this turn completed.
fn ended(id: &SessionId) -> Failure {
    failure(
        ErrorCode::Closing,
        format!("session {} ended before its turn completed", id.0),
    )
}

/// The lock is held but nothing accepts: `session_held`, naming the holder
/// as [`log::Log::open`] does.
fn held(home: &Path, id: &SessionId) -> Failure {
    failure(
        ErrorCode::SessionHeld,
        format!(
            "session {} is held by {}; only one Fiber process may write a session",
            id.0,
            holder(home, id),
        ),
    )
}

/// The holder named in the session's lock file, read without taking it, as
/// [`log::Log::open`] names it. Attach never opens the log.
fn holder(home: &Path, id: &SessionId) -> String {
    let projects = home.join("projects");
    let Ok(keys) = fs::read_dir(&projects) else {
        return unknown_holder();
    };
    for key in keys.flatten() {
        let lock = key.path().join("sessions").join(&id.0).join("session.lock");
        if let Ok(pid) = fs::read_to_string(&lock)
            && !pid.trim().is_empty()
        {
            return format!("process {}", pid.trim());
        }
    }
    unknown_holder()
}

fn unknown_holder() -> String {
    "a process whose pid is not yet recorded".to_owned()
}

fn io_failed(path: &Path, e: &std::io::Error) -> Failure {
    failure(ErrorCode::IoFailed, format!("{}: {e}", path.display()))
}
