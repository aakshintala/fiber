use contract::CommandId;
use contract::commands::CommandLine;

use super::{ParsedLine, ReplyAnswer};

/// Parses one complete stdout line through `CommandLine`, sharing the
/// doors' rule: a line that is no JSON object with a string `id` and
/// `command` is malformed, or ignored when no id names the answer; a
/// full parse failure answers `unknown_command` for an unbuilt name and
/// `invalid_arguments` for a built one.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn parse_line(line: &[u8]) -> ParsedLine {
    let value: serde_json::Value = match std::str::from_utf8(line)
        .ok()
        .and_then(|text| serde_json::from_str(text).ok())
    {
        Some(value) => value,
        None => return ParsedLine::Ignored,
    };
    let Some(map) = value.as_object() else {
        return ParsedLine::Ignored;
    };
    let id: Option<CommandId> = map
        .get("id")
        .and_then(|id| serde_json::from_value(id.clone()).ok());
    let classified = match &id {
        Some(id) => classify(map, id.clone()),
        None => return ParsedLine::Ignored,
    };
    let Some(classified) = classified else {
        let Some(id) = id else {
            return ParsedLine::Ignored;
        };
        return ParsedLine::Malformed { id };
    };
    let id = classified.id.clone();
    // A forwarded command's arguments read straight off the line: the
    // classify step already held `args` to absent or an object, and the
    // full parse validated the command, so no shape is left to refuse.
    let forward_args = map
        .get("args")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let parsed: Result<CommandLine, _> = serde_json::from_value(value);
    let Ok(parsed) = parsed else {
        return unbuilt_or_unfit(classified);
    };
    match parsed.command {
        contract::commands::Command::Call(call) => ParsedLine::Call {
            id,
            call: call.call,
            args: call.args,
            request_id: call.request_id,
        },
        contract::commands::Command::Ready => ParsedLine::Ready { id },
        contract::commands::Command::Reply(reply) => match reply.answer {
            contract::commands::ReplyAnswer::Value { result } => ParsedLine::Reply {
                id,
                request_id: reply.request_id,
                answer: ReplyAnswer::Value(result),
            },
            contract::commands::ReplyAnswer::Failure { error } => ParsedLine::Reply {
                id,
                request_id: reply.request_id,
                answer: ReplyAnswer::Failure {
                    code: error.code,
                    message: error.message,
                },
            },
            // Only what the Lua function returns, or its error, answers
            // a process request: interaction answers ride the session's
            // own channel, never this pipe.
            contract::commands::ReplyAnswer::Declined { .. }
            | contract::commands::ReplyAnswer::Confirmed { .. }
            | contract::commands::ReplyAnswer::Labels { .. }
            | contract::commands::ReplyAnswer::Text { .. }
            | contract::commands::ReplyAnswer::Form { .. }
            | contract::commands::ReplyAnswer::Approval { .. }
            | contract::commands::ReplyAnswer::Decisions { .. } => ParsedLine::BadReply { id },
        },
        contract::commands::Command::Unknown => ParsedLine::Unknown {
            id,
            command: classified.command,
        },
        // Every other driver command forwards through the `Drive` seam:
        // adding a command to the wire extends this arm, or the compiler
        // refuses it — never a silent drop.
        contract::commands::Command::Subscribe(_)
        | contract::commands::Command::Prompt(_)
        | contract::commands::Command::Steer(_)
        | contract::commands::Command::SteerDrop(_)
        | contract::commands::Command::Message(_)
        | contract::commands::Command::Cancel
        | contract::commands::Command::JobStop(_)
        | contract::commands::Command::JobInput(_)
        | contract::commands::Command::Background
        | contract::commands::Command::Reload
        | contract::commands::Command::Import(_)
        | contract::commands::Command::Tools
        | contract::commands::Command::Hooks
        | contract::commands::Command::McpServers
        | contract::commands::Command::McpLogin(_)
        | contract::commands::Command::McpReconnect(_)
        | contract::commands::Command::Commands
        | contract::commands::Command::Skills
        | contract::commands::Command::Quota(_)
        | contract::commands::Command::Credentials
        | contract::commands::Command::History(_)
        | contract::commands::Command::ArtifactPage(_)
        | contract::commands::Command::Model(_)
        | contract::commands::Command::Credential(_)
        | contract::commands::Command::Name(_)
        | contract::commands::Command::Handoff(_)
        | contract::commands::Command::Rewind(_)
        | contract::commands::Command::ChangeWorkspace(_)
        | contract::commands::Command::Shell(_)
        | contract::commands::Command::ProposeNotes(_)
        | contract::commands::Command::AddDirectory(_)
        | contract::commands::Command::Command(_)
        | contract::commands::Command::Close(_)
        | contract::commands::Command::OfferedFile(_) => ParsedLine::Forward {
            id,
            command: classified.command,
            args: forward_args,
        },
    }
}

/// One command line, as `classify` reads it: the doors' shared shape.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
struct Classified {
    id: CommandId,
    command: String,
}

/// Reads one JSON object line with a string `id` and `command`, holding
/// only `id`, `command` and `args` keys with `args` absent or an object:
/// the shape the doors classify before parsing. `None` reads as no
/// command, answered `malformed` when an id names the answer.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
fn classify(map: &serde_json::Map<String, serde_json::Value>, id: CommandId) -> Option<Classified> {
    if map
        .keys()
        .any(|key| key != "id" && key != "command" && key != "args")
    {
        return None;
    }
    let command = map
        .get("command")
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    match map.get("args") {
        None | Some(serde_json::Value::Object(_)) => {}
        Some(_) => return None,
    }
    Some(Classified { id, command })
}

/// Answers a full parse failure by the name alone, sharing the doors'
/// rule: an unbuilt name answers `unknown_command`, anything else — a
/// built command with unfit arguments — `invalid_arguments`. The probe
/// replays the name with empty arguments: a name the wire does not build
/// parses to `Unknown`, while a built name either parses or refuses for
/// want of its arguments — either way the name is built.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
fn unbuilt_or_unfit(classified: Classified) -> ParsedLine {
    let probe = serde_json::json!({
        "id": "probe",
        "command": &classified.command,
        "args": {},
    });
    let unbuilt = serde_json::from_value::<CommandLine>(probe)
        .map(|line| matches!(line.command, contract::commands::Command::Unknown))
        .unwrap_or(false);
    if unbuilt {
        ParsedLine::Unknown {
            id: classified.id,
            command: classified.command,
        }
    } else {
        ParsedLine::Unfit { id: classified.id }
    }
}
