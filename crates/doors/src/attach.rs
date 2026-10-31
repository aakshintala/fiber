//! Attaching to a running session (`docs/invocation.md`, "Processes"): a
//! `full` subscription folds the log by `seq` first, then streams, and one
//! `prompt` command starts the turn this process prints. With a credential
//! label, a `credential` command switches it first, at the turn boundary
//! before the prompt's turn. Attach never sends
//! `close` and never opens the log: the holder keeps the only writer, and a
//! disconnect only ends the client.

use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use contract::commands::{
    Command, CommandLine, ContentArgs, CredentialArgs, SentPart, SubscribeArgs, SubscribeLevel,
};
use contract::events::EventKind;
use contract::shapes::Failure;
use contract::{CommandId, Envelope, ErrorCode, SCHEMA_VERSION, SessionId};
use serde_json::{Map, Value};

use crate::{SessionError, mint};

/// Sends `prompt` to the session `id` running under `home`, prints the
/// lines from the `turn_started` carrying this prompt's command id through
/// that turn's `turn_completed`, and nothing before it, and returns 0 for a
/// `completed` turn and 1 for any other outcome. With a credential label,
/// the `credential` command is sent after the subscribe answer and the
/// prompt only once it is accepted; a rejection is its code and message,
/// printing nothing, and the prompt is never sent. A `command_rejected` for
/// the prompt is the rejection's code and message, printing nothing.
/// `command_accepted` and `command_rejected` lines are never printed: the
/// prompt's own `command_accepted` arrives after its `turn_started`,
/// because the loop writes the turn before it accepts the prompt. A closed
/// connection or a `fiber_exited` before the turn completes is a failure,
/// not a hang. `refused` is returned when nothing accepts on the socket:
/// the caller names the holder, which attach never reads, as it never opens
/// the log.
pub fn attach(
    home: &Path,
    id: &SessionId,
    credential: Option<&str>,
    prompt: String,
    out: &mut dyn Write,
    refused: Failure,
) -> Result<i32, SessionError> {
    // One handshake for every client: connect, mint, write the subscribe,
    // then read until its answer and check the version, all on the raw
    // line. A session on another schema version may answer outside this
    // build's envelope, so the answer is matched before any envelope
    // parse (`docs/invocation.md`, "Processes"). The socket path is joined
    // here, where the Fiber-home allowance lives, and passed down; `open`
    // below never joins a home path itself.
    let socket = home.join("run").join(&id.0);
    let (mut reader, mut writer) = match subscribe_full(&socket, id) {
        Ok(pair) => pair,
        Err(Unsubscribed::Connect(_)) => return Err(SessionError::Forwarded(refused)),
        Err(Unsubscribed::Clone(e)) => {
            return Err(SessionError::SubscribeClone {
                socket: socket.clone(),
                source: e,
            });
        }
        Err(Unsubscribed::Write(f) | Unsubscribed::Rejected(f) | Unsubscribed::Version(f)) => {
            return Err(f);
        }
        Err(Unsubscribed::Closed) => return Err(ended(id)),
    };

    let command = mint("c_");
    if let Some(label) = credential {
        // The switch applies at the turn boundary before the prompt's
        // turn, so the prompt runs on the new label. Its answer is awaited
        // here and printed nowhere (`docs/invocation.md`, "Driver
        // commands").
        let switch = mint("c_");
        send(
            &mut writer,
            id,
            &CommandLine {
                id: CommandId(switch.clone()),
                command: Command::Credential(CredentialArgs {
                    label: label.to_owned(),
                }),
            },
        )?;
        loop {
            let Some((_, envelope)) = next_envelope(&mut reader) else {
                return Err(ended(id));
            };
            if answers(&envelope, EventKind::CommandRejected, &switch) {
                return Err(rejection(&envelope.payload));
            }
            if answers(&envelope, EventKind::CommandAccepted, &switch) {
                break;
            }
        }
    }
    send(
        &mut writer,
        id,
        &CommandLine {
            id: CommandId(command.clone()),
            command: Command::Prompt(ContentArgs {
                content: vec![SentPart::Text { text: prompt }],
            }),
        },
    )?;
    let mut printing = false;
    loop {
        let Some((raw, envelope)) = next_envelope(&mut reader) else {
            return Err(ended(id));
        };
        if answers(&envelope, EventKind::CommandRejected, &command) {
            // The prompt's own rejection, read from its payload so its
            // code and message match the handshake's.
            return Err(rejection(&envelope.payload));
        }
        // The kind is parsed once: a name this build does not know is
        // `Unknown`, which matches nothing below and is skipped or
        // printed like any other line outside the turn.
        let kind = EventKind::from_name(&envelope.kind);
        if kind == EventKind::CommandAccepted || kind == EventKind::CommandRejected {
            continue;
        }
        if starts_turn(&envelope, &command) {
            printing = true;
        } else if printing && kind == EventKind::TurnCompleted {
            emit(out, &raw)?;
            return Ok(exit_code(&envelope));
        } else if printing && kind == EventKind::FiberExited {
            return Err(ended(id));
        }
        if printing {
            emit(out, &raw)?;
        }
    }
}

/// Sends one command line. A write that fails means the session is gone.
pub(crate) fn send(
    writer: &mut UnixStream,
    id: &SessionId,
    line: &CommandLine,
) -> Result<(), SessionError> {
    let bytes =
        serde_json::to_vec(line).map_err(|source| SessionError::CommandEncode { source })?;
    support::framing::write_line(writer, &bytes).map_err(|_| ended(id))
}

/// The next line from the session, with its bytes for printing. A line that
/// is not JSON is skipped: the session writes JSON, and a stray line must
/// not end the attach. `None` is a closed connection.
pub(crate) fn next_line(reader: &mut BufReader<UnixStream>) -> Option<(Vec<u8>, Value)> {
    loop {
        match support::framing::read_line(reader, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => return None,
            Ok(Some(raw)) => {
                if let Ok(line) = serde_json::from_slice::<Value>(&raw) {
                    return Some((raw, line));
                }
            }
        }
    }
}

pub(crate) fn kind(line: &Value) -> &str {
    line.get("kind").and_then(Value::as_str).unwrap_or("")
}

/// Whether `line` answers `command` with `kind`, on the raw JSON line.
/// This stays on `Value` for the readers that match before any envelope
/// parse: the subscribe handshake (a session on another schema version
/// may answer outside this build's envelope), `steer`, and the sessions
/// poll loop. Envelope readers past the handshake use [`answers`] instead.
pub(crate) fn is_answer(line: &Value, kind: &str, command: &str) -> bool {
    self::kind(line) == kind
        && line.pointer("/payload/command_id").and_then(Value::as_str) == Some(command)
}

/// One open subscription: the connection past its subscribe write, still
/// waiting on the answer.
pub(crate) struct Opened {
    pub(crate) reader: BufReader<UnixStream>,
    pub(crate) writer: UnixStream,
    pub(crate) sub: CommandId,
}

/// What a subscribe wait ended on, before its caller maps it onto its own
/// errors.
pub(crate) enum Unsubscribed {
    /// Nothing accepts on the socket.
    Connect(io::Error),
    /// The reader side did not clone.
    Clone(io::Error),
    /// The subscribe write failed.
    Write(SessionError),
    /// The connection closed before the answer.
    Closed,
    /// The subscribe was rejected, with its code and message.
    Rejected(SessionError),
    /// The session runs another schema version.
    Version(SessionError),
}

/// Connects to the session socket, mints the subscribe id and writes the
/// `level` subscribe. The caller joins the socket path under its own
/// Fiber-home allowance and passes it in: this never joins a home path
/// itself. `steer` subscribes `summary` and pauses between this and
/// [`subscribed`]; `attach` and `watch` subscribe `full` through
/// [`subscribe_full`].
pub(crate) fn open(
    socket: &Path,
    id: &SessionId,
    level: SubscribeLevel,
) -> Result<Opened, Unsubscribed> {
    let stream = UnixStream::connect(socket).map_err(Unsubscribed::Connect)?;
    let reader = BufReader::new(stream.try_clone().map_err(Unsubscribed::Clone)?);
    let mut writer = stream;
    let sub = mint("c_");
    send(
        &mut writer,
        id,
        &CommandLine {
            id: CommandId(sub.clone()),
            command: Command::Subscribe(SubscribeArgs {
                level,
                credential: None,
            }),
        },
    )
    .map_err(Unsubscribed::Write)?;
    Ok(Opened {
        reader,
        writer,
        sub: CommandId(sub),
    })
}

/// Reads until the subscribe's answer and checks the version, on the raw
/// line: a session on another schema version may answer outside this
/// build's envelope, so the answer is matched before any envelope parse. A
/// rejection is answered before the version, as the session checks the
/// command first.
pub(crate) fn subscribed(
    opened: Opened,
    id: &SessionId,
) -> Result<(BufReader<UnixStream>, UnixStream), Unsubscribed> {
    let Opened {
        mut reader,
        writer,
        sub,
    } = opened;
    // The acknowledgement is written before the writer starts, so it is the
    // first line this connection reads, ahead of the fold.
    loop {
        let Some((_, line)) = next_line(&mut reader) else {
            return Err(Unsubscribed::Closed);
        };
        if is_answer(&line, "command_rejected", &sub.0) {
            return Err(Unsubscribed::Rejected(rejected(&line)));
        }
        if is_answer(&line, "command_accepted", &sub.0) {
            if let Err(failure) = check_version(id, &line) {
                return Err(Unsubscribed::Version(failure));
            }
            return Ok((reader, writer));
        }
    }
}

/// [`open`] at `full`, then [`subscribed`]: the whole handshake. Takes the
/// already-joined socket path, as [`open`] does.
pub(crate) fn subscribe_full(
    socket: &Path,
    id: &SessionId,
) -> Result<(BufReader<UnixStream>, UnixStream), Unsubscribed> {
    let opened = open(socket, id, SubscribeLevel::Full)?;
    subscribed(opened, id)
}

/// The next envelope from the session, with its bytes for printing. A JSON
/// line that is not an envelope is skipped: every line a session writes is
/// an envelope, so no real line is lost. `None` is a closed connection.
pub(crate) fn next_envelope(reader: &mut BufReader<UnixStream>) -> Option<(Vec<u8>, Envelope)> {
    loop {
        let (raw, line) = next_line(reader)?;
        if let Ok(envelope) = serde_json::from_value::<Envelope>(line) {
            return Some((raw, envelope));
        }
    }
}

/// Whether `envelope` answers `command` with `kind`.
fn answers(envelope: &Envelope, kind: EventKind, command: &str) -> bool {
    EventKind::from_name(&envelope.kind) == kind
        && envelope.payload.get("command_id").and_then(Value::as_str) == Some(command)
}

/// Whether `envelope` is the `turn_started` carrying this prompt's command
/// id in its input.
fn starts_turn(envelope: &Envelope, command: &str) -> bool {
    EventKind::from_name(&envelope.kind) == EventKind::TurnStarted
        && envelope
            .payload
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("command_id").and_then(Value::as_str) == Some(command))
            })
}

/// The exit code for `envelope`'s outcome: 0 for `completed`, 1 for the rest.
fn exit_code(envelope: &Envelope) -> i32 {
    if envelope.payload.get("outcome").and_then(Value::as_str) == Some("completed") {
        0
    } else {
        1
    }
}

/// Prints one line of the turn.
fn emit(out: &mut dyn Write, raw: &[u8]) -> Result<(), SessionError> {
    support::framing::write_line(out, raw).map_err(|source| SessionError::StdoutFailed { source })
}

/// Any command's rejection code and message, read from the answer line's
/// payload map. Envelope readers pass `envelope.payload` to [`rejection`]
/// directly; raw-line readers pass the whole line here.
pub(crate) fn rejected(line: &Value) -> SessionError {
    let empty = Map::new();
    let payload = line
        .get("payload")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    rejection(payload)
}

/// Any command's rejection code and message, read from the answer's payload
/// map. Envelope readers pass `envelope.payload` directly; raw-line readers
/// use [`rejected`] on the whole line.
pub(crate) fn rejection(payload: &Map<String, Value>) -> SessionError {
    let code = payload
        .get("code")
        .cloned()
        .and_then(|code| serde_json::from_value(code).ok())
        .unwrap_or(ErrorCode::Other("unknown".to_owned()));
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    SessionError::Rejected { code, message }
}

/// Declines on a `schema_version` different from this build's
/// (`docs/invocation.md`, "Processes"): an additive difference never bumps
/// the version (`docs/events.md`, "Versioning"), so any other one cannot be
/// read. Another process does hold the session, so the code stays
/// `session_held`, and the message names the session's version and this one.
pub(crate) fn check_version(id: &SessionId, line: &Value) -> Result<(), SessionError> {
    let version = line
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if u64::from(SCHEMA_VERSION) != version {
        return Err(SessionError::VersionMismatch {
            id: id.clone(),
            version,
        });
    }
    Ok(())
}

/// The session ended before answering: its connection closed, or its
/// `fiber_exited` arrived before this turn completed.
fn ended(id: &SessionId) -> SessionError {
    SessionError::SessionEnded { id: id.clone() }
}
