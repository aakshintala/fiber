//! The in-process driver (`docs/extensions.md`, "Host calls"): what an
//! extension's `host.drive` sends through. It builds the same JSON line a
//! socket client would send and runs it through the same `classify`,
//! `built` and `CommandLine` checks and the same `dispatch` as a socket
//! line, as a connection already subscribed (so a `subscribe` is rejected
//! `invalid_arguments` as a second one). Its acknowledgement goes to the
//! host call, not to any stream, through the answer sink on its driven
//! connection (`client.rs`).

use std::sync::{Arc, Weak};

use contract::commands::{Command, CommandLine, ReplyAnswer};
use contract::inbox::{Ack, Rejection};
use contract::shapes::Origin;
use contract::{CommandId, ErrorCode};
use serde_json::{Map, Value};

use crate::client;
use crate::session::Gate;

/// The in-process driver: it holds the gate weakly, so the extensions never
/// keep the door alive after [`crate::Session::close`].
pub(crate) struct Driver {
    gate: Weak<Gate>,
}

impl Driver {
    pub(crate) fn new(gate: Weak<Gate>) -> Self {
        Self { gate }
    }
}

/// An extension never approves a tool call, here or in a hook.
const NO_APPROVAL: &str = "An extension never answers an approval.";

impl contract::extension::Drive for Driver {
    /// `extension` becomes `Origin::Extension` on any message the command
    /// carries. After the session's door closed the answer is `closing`: the
    /// gate is checked after upgrading, because a retained handle (a release
    /// closure, a shell thread) can keep it alive past the close.
    fn drive(&self, extension: &str, command: &str, args: Map<String, Value>, answer: Ack) {
        let Some(gate) = self.gate.upgrade() else {
            // drive_after_close_is_closing: the gate is gone.
            answer.0(Err(Rejection {
                code: ErrorCode::Closing,
                message: client::ENDED.to_owned(),
            }));
            return;
        };
        if gate.stopped() {
            // drive_after_close_is_closing: a retained handle keeps the gate
            // alive past `Session::close`.
            answer.0(Err(Rejection {
                code: ErrorCode::Closing,
                message: client::ENDED.to_owned(),
            }));
            return;
        }
        let mut line = Map::new();
        let id = CommandId(crate::mint("c_"));
        line.insert("id".into(), Value::String(id.0.clone()));
        line.insert("command".into(), Value::String(command.to_owned()));
        line.insert("args".into(), Value::Object(args));
        let Ok(bytes) = serde_json::to_vec(&Value::Object(line)) else {
            answer.0(Err(Rejection {
                code: ErrorCode::InvalidArguments,
                message: client::UNFIT.to_owned(),
            }));
            return;
        };
        let classified = match classify(&bytes) {
            Ok(classified) => classified,
            Err(id) => {
                answer.0(Err(Rejection {
                    code: ErrorCode::Malformed,
                    message: client::MALFORMED.to_owned(),
                }));
                let _ = id;
                return;
            }
        };
        if !gate.reserve(&classified.id) {
            answer.0(Err(Rejection {
                code: ErrorCode::DuplicateCommand,
                message: client::DUPLICATE.to_owned(),
            }));
            return;
        }
        if classified.command == "subscribe" {
            // drive_subscribe_is_rejected: already subscribed, as a second one.
            gate.release(&classified.id);
            answer.0(Err(Rejection {
                code: ErrorCode::InvalidArguments,
                message: client::ALREADY.to_owned(),
            }));
            return;
        }
        let (parsed, name) = match parse(classified) {
            Ok(parsed) => parsed,
            Err((id, code, message)) => {
                // drive_unknown_command_is_rejected: frees the minted id.
                gate.release(&id);
                answer.0(Err(Rejection { code, message }));
                return;
            }
        };
        if let Command::Reply(reply) = &parsed.command
            && matches!(reply.answer, ReplyAnswer::Approval { .. })
        {
            // drive_approval_reply_is_rejected: never reaches the inbox.
            gate.release(&parsed.id);
            answer.0(Err(Rejection {
                code: ErrorCode::InvalidArguments,
                message: NO_APPROVAL.to_owned(),
            }));
            return;
        }
        let mut conn = client::Conn::driven(gate, answer, extension);
        client::dispatch(&mut conn, parsed, &name);
    }
}

/// One command line, as `classify` reads it.
pub(crate) struct Classified {
    pub(crate) id: CommandId,
    pub(crate) command: String,
    pub(crate) value: Value,
}

/// Reads one JSON object line with a string `id` and `command`: the parse
/// step the socket line and the in-process driver share. `Err` carries
/// `command_id` when the line had a string `id`.
pub(crate) fn classify(bytes: &[u8]) -> Result<Classified, Option<CommandId>> {
    let text = std::str::from_utf8(bytes).map_err(|_| None)?;
    let value: Value = serde_json::from_str(text).map_err(|_| None)?;
    let Some(map) = value.as_object() else {
        return Err(None);
    };
    let id = match map.get("id") {
        Some(Value::String(id)) => Some(CommandId(id.clone())),
        _ => None,
    };
    if map
        .keys()
        .any(|key| key != "id" && key != "command" && key != "args")
    {
        return Err(id);
    }
    let Some(id) = id else {
        return Err(None);
    };
    let Some(Value::String(command)) = map.get("command") else {
        return Err(Some(id));
    };
    match map.get("args") {
        None | Some(Value::Object(_)) => {}
        Some(_) => return Err(Some(id)),
    }
    Ok(Classified {
        id,
        command: command.clone(),
        value,
    })
}

/// The `built` and `CommandLine` checks, shared by the socket line and the
/// in-process driver: `classify` runs first in both.
pub(crate) fn parse(
    line: Classified,
) -> Result<(CommandLine, String), (CommandId, ErrorCode, String)> {
    if !client::built(&line.command) {
        let message = client::not_built(&line.command);
        return Err((line.id, ErrorCode::UnknownCommand, message));
    }
    match serde_json::from_value::<CommandLine>(line.value) {
        Ok(parsed) => Ok((parsed, line.command)),
        Err(_) => Err((
            line.id,
            ErrorCode::InvalidArguments,
            client::UNFIT.to_owned(),
        )),
    }
}

impl client::Conn {
    /// A connection for the in-process driver: already subscribed, with no stream.
    pub(crate) fn driven(gate: Arc<Gate>, answer: Ack, extension: &str) -> Self {
        Self {
            id: 0,
            gate,
            direct: None,
            writer: None,
            subscribed: true,
            full: true,
            outbox: None,
            switches: None,
            gone: false,
            drive: Some((
                answer,
                Origin::Extension {
                    extension: extension.to_owned(),
                },
            )),
        }
    }

    /// The sender a driven `prompt` or `steer` carries; a socket's is `Driver`.
    pub(crate) fn origin(&self) -> Origin {
        self.drive
            .as_ref()
            .map(|(_, origin)| origin.clone())
            .unwrap_or(Origin::Driver)
    }
}

#[cfg(test)]
#[path = "drive_tests.rs"]
mod tests;
