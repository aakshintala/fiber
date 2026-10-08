//! Every driver command (`docs/invocation.md`, "Driver commands").
//!
//! A line with a key the command line or the command does not take fails to
//! read, so an older Fiber says no to a newer client's key instead of ignoring
//! it.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::events::Decision;
use crate::shapes::True;
use crate::{CommandId, JobId, RequestId, Seq, SessionId};

/// One command line: one JSON object on the driver channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandLine {
    /// Minted by the client from random bytes. Acknowledgements and events
    /// name the command by it, as `command_id`.
    pub id: CommandId,
    /// The command, as `command` and its `args`.
    #[serde(flatten)]
    pub command: Command,
}

impl<'de> Deserialize<'de> for CommandLine {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if let Some(args) = value.get("args")
            && args.is_object()
            && contains_null(args)
        {
            return Err(D::Error::custom("an optional key is absent, never null"));
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Line {
            id: CommandId,
            #[serde(flatten)]
            command: Command,
        }
        // A missing `args` is read as `{}`, so a command whose keys are all
        // optional may leave it out. A command that takes no `args` reads
        // without it, so a missing `args` is tried as `{}` and an empty one
        // as missing, each second.
        let line = match Line::deserialize(&value) {
            Ok(line) => line,
            Err(first) => {
                let mut retry = value;
                let Some(map) = retry
                    .as_object_mut()
                    .filter(|map| map.contains_key("command"))
                else {
                    return Err(D::Error::custom(first));
                };
                match map.get("args") {
                    None => {
                        map.insert("args".into(), Value::Object(Default::default()));
                    }
                    Some(Value::Object(args)) if args.is_empty() => {
                        map.remove("args");
                    }
                    Some(_) => return Err(D::Error::custom(first)),
                }
                Line::deserialize(retry).map_err(D::Error::custom)?
            }
        };
        Ok(Self {
            id: line.id,
            command: line.command,
        })
    }
}

fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

/// A driver command, keyed by `command`, with its keys under `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "args", rename_all = "snake_case")]
pub enum Command {
    /// The first command on every connection.
    Subscribe(SubscribeArgs),
    /// Starts a turn.
    Prompt(ContentArgs),
    /// Sends a steering message.
    Steer(ContentArgs),
    /// Removes a queued steering message.
    SteerDrop(SteerDrop),
    /// Delivers a session message from another session.
    Message(Message),
    /// Ends the running turn.
    Cancel,
    /// Answers an interaction the loop raised.
    Reply(Reply),
    /// Stops a running job.
    JobStop(JobStop),
    /// Moves every shell call running in the current turn to the background.
    Background,
    /// Re-reads configuration and declares the tool set again.
    Reload,
    /// Answers with every declared tool.
    Tools,
    /// Answers with every `/name` the session runs.
    Commands,
    /// Answers with every skill discovery found, switched-off and shadowed ones included.
    Skills,
    /// Answers with durable log lines in a seq range.
    History(HistoryArgs),
    /// Switches model or thinking at the next turn boundary.
    Model(ModelArgs),
    /// Switches the credential label at the next turn boundary.
    Credential(CredentialArgs),
    /// Sets the session's name.
    Name(Name),
    /// Starts a handoff.
    Handoff(Handoff),
    /// Starts a new session that continues a session from an earlier point.
    Rewind(RewindArgs),
    /// Runs a shell command the person typed.
    Shell(Shell),
    /// Runs an extension's command by name.
    Command(RunCommand),
    /// Accepts no more prompts, finishes what is running, and exits; with `now`, shuts down.
    Close(CloseArgs),
}

/// A content part a client sends. An image is sent as its bytes; Fiber writes
/// it to `artifacts/` and logs it by path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SentPart {
    /// Text.
    Text {
        /// The text.
        text: String,
    },
    /// An image.
    Image {
        /// The image's bytes, in base64.
        data: String,
        /// Its type, such as `image/png`.
        mime_type: String,
    },
}

/// The `args` of `prompt` and `steer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentArgs {
    /// The message.
    pub content: Vec<SentPart>,
}

/// The `args` of `steer_drop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerDrop {
    /// The `steer` command's id.
    pub command_id: CommandId,
}

/// The `args` of `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    /// The sending session.
    pub from_session_id: SessionId,
    /// The message.
    pub text: String,
}

/// The `args` of `reply`: the request and its answer. The answer keys are
/// the keys of the line the reply causes. [`ReplyAnswer`] refuses a key it
/// does not take, which `deny_unknown_fields` here cannot do beside `flatten`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// The request answered.
    pub request_id: RequestId,
    /// The answer.
    #[serde(flatten)]
    pub answer: ReplyAnswer,
}

/// A reply's answer: an interaction's answer keys (`interaction_resolved`),
/// an approval's, or an offer's decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ReplyAnswer {
    /// Declines an interaction.
    Declined {
        /// `true`.
        declined: True,
    },
    /// Answers `confirm`.
    Confirmed {
        /// Yes or no.
        confirmed: bool,
    },
    /// Answers `select` (one label) or `multi_select`.
    Labels {
        /// The chosen labels.
        labels: Vec<String>,
    },
    /// Answers `text_input`.
    Text {
        /// The typed answer.
        text: String,
    },
    /// Answers a `form`.
    Form {
        /// One per field, in field order.
        answers: Vec<SentFormAnswer>,
        /// The person's note on the whole form.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Answers an approval.
    Approval {
        /// Allow or deny.
        decision: Decision,
        /// With `deny`, what the person typed; the model receives it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
        /// With `allow`, on a request that offers a `rule`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<Remember>,
    },
    /// Answers a repository's offer, one per item in the offer's order.
    Decisions {
        /// `approve`, `skip` or `never` for each item.
        decisions: Vec<crate::events::OfferDecision>,
    },
}

/// One field's answer in a form reply. Unlike the logged
/// [`FormAnswer`](crate::events::FormAnswer), it refuses a key it does not
/// take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum SentFormAnswer {
    /// The person skipped the field.
    Skipped {
        /// `true`.
        skipped: True,
    },
    /// The person answered it.
    Answered {
        /// The chosen labels, possibly none.
        labels: Vec<String>,
        /// What the person typed, when they typed any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// What an allow remembers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remember {
    /// Where it is remembered.
    pub scope: RememberScope,
    /// The request's `rule.subject` or `rule.prefix`.
    pub prefix: String,
}

/// Where an allow is remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberScope {
    /// A session grant.
    Session,
    /// A standing rule in the project's rules file.
    Project,
}

/// The `args` of `job_stop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobStop {
    /// The running job.
    pub job_id: JobId,
}

/// The `args` of `history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryArgs {
    /// The first `seq` to return, inclusive.
    pub from_seq: Seq,
    /// The last `seq` to return, inclusive; absent means the latest line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_seq: Option<Seq>,
}

/// The `args` of `model`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelArgs {
    /// A model reference as a person types one.
    pub model: String,
    /// The thinking level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// The `args` of `credential`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialArgs {
    /// A credential label of the session model's provider.
    pub label: String,
}

/// How much of the stream a connection receives (`docs/invocation.md`,
/// "Driver commands", `subscribe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeLevel {
    /// The latest `session_status` and `extensions_loaded` only.
    Summary,
    /// The session's whole stream, folded from the log first.
    Full,
}

/// The `args` of `subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeArgs {
    /// `summary` or `full`.
    pub level: SubscribeLevel,
}

/// The `args` of `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Name {
    /// The name; empty clears the person's name and unpins it.
    pub text: String,
}

/// The `args` of `handoff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    /// What the next stretch of work focuses on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// The `args` of `rewind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewindArgs {
    /// The session to rewind; absent means this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_session_id: Option<SessionId>,
    /// The point; absent means the start of the latest turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<Seq>,
    /// Whether to summarise the path after the point.
    #[serde(default)]
    pub summarise: bool,
    /// The jobs started after the point that the new session keeps; every
    /// other such job stops.
    #[serde(default)]
    pub adopt: Vec<JobId>,
}

/// The `args` of `shell`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shell {
    /// The command.
    pub command: String,
    /// Whether the output joins the next turn's input, logged as
    /// `shell_command`.
    #[serde(default)]
    pub send: bool,
}

/// The `args` of `command`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCommand {
    /// The extension command's name.
    pub name: String,
    /// What the person typed after the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// The `args` of `close`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseArgs {
    /// With `true`, shuts down instead of finishing what is running.
    #[serde(default)]
    pub now: bool,
}

/// The built-in commands are the terminal's (`docs/tui.md`, "Slash commands"):
/// the names of that table, which the extension command check reads.
pub const BUILT_IN_COMMANDS: &[&str] = &[
    "home",
    "new",
    "resume",
    "model",
    "thinking",
    "credential",
    "scoped-models",
    "context",
    "usage",
    "tools",
    "panel",
    "rules",
    "settings",
    "keys",
    "skills",
    "rewind",
    "handoff",
    "name",
    "login",
    "approvals",
    "reload",
    "close",
    "quit",
    "?",
    "help",
];

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
