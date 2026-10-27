//! Every driver command (`docs/invocation.md`, "Driver commands").
//!
//! A line with a key the command line or the command does not take fails to
//! read, so an older Fiber says no to a newer client's key instead of ignoring
//! it.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::events::Decision;
use crate::shapes::True;
use crate::{CommandId, ErrorCode, JobId, RequestId, Seq, SessionId};

pub mod hub;

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
            && let Some(map) = args.as_object()
            && args_refuse_null(value.get("command").and_then(Value::as_str), map)
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

/// Whether a command line's `args` object breaks the null rule: a null
/// anywhere but the arbitrary-JSON positions the process wire carries — a
/// `call`'s argument array and a `reply`'s `result`. A null in a control
/// key, in any other position, or on any other command is still refused: an
/// optional key is absent, never `null`.
fn args_refuse_null(command: Option<&str>, args: &Map<String, Value>) -> bool {
    args.iter().any(|(key, item)| {
        let wired = matches!(
            (command, key.as_str()),
            (Some("call"), "args") | (Some("reply"), "result")
        );
        !wired && contains_null(item)
    })
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
    /// Types into a running job started with `tty`.
    JobInput(JobInput),
    /// Moves every shell call running in the current turn to the background.
    Background,
    /// Re-reads configuration and declares the tool set again.
    Reload,
    /// Copies another harness's configuration into Fiber home, once.
    Import(ImportArgs),
    /// Answers with every declared tool.
    Tools,
    /// Answers with every hook and watcher the session registered.
    Hooks,
    /// Answers with every configured MCP server and its state.
    McpServers,
    /// Runs the OAuth login flow for one MCP server, then starts it.
    McpLogin(McpServerArgs),
    /// Restarts one failed MCP server.
    McpReconnect(McpServerArgs),
    /// Answers with every `/name` the session runs.
    Commands,
    /// Answers with every skill discovery found, switched-off and shadowed ones included.
    Skills,
    /// Answers with each provider credential label's cached quota.
    Quota(QuotaArgs),
    /// Answers with the labels of the provider a `credential` command would
    /// switch now, and the label that provider uses.
    Credentials,
    /// Answers with durable log lines in a seq range.
    History(HistoryArgs),
    /// Answers with a page of an artifact's full output.
    ArtifactPage(ArtifactPageArgs),
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
    /// Proposes reviewer-note lines for one topic, without conversation.
    ProposeNotes(crate::notes::ProposeNotesArgs),
    /// Runs an extension's command by name.
    Command(RunCommand),
    /// Accepts no more prompts, finishes what is running, and exits; with `now`, shuts down.
    Close(CloseArgs),
    /// Reads one file listed by a pending repository offer.
    OfferedFile(OfferedFile),
    /// Sends one process-extension call: a registration, a `host` call or
    /// a `state` call, answered with the call's return. Process extensions
    /// only, on their own pipe (`docs/extensions.md`, "Process extensions").
    Call(CallArgs),
    /// Ends a process extension's registrations. Process extensions only,
    /// on their own pipe.
    Ready,
    /// A command this Fiber does not build: never sent by a client type,
    /// answered `unknown_command` with the line's own command name.
    #[serde(other)]
    #[serde(skip_serializing)]
    Unknown,
}

/// The `args` of `call`: one process-extension call, with the Lua call's
/// arguments as a JSON array. A function the Lua call passes as a value is
/// sent as `true`, and Fiber calls it with an `extension_request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallArgs {
    /// The call's name as Lua writes it, such as `fiber.tool`.
    pub call: String,
    /// The Lua call's arguments, as a JSON array; any JSON value allowed.
    pub args: Vec<Value>,
    /// The request answered, naming the call as made inside that request's
    /// callback; absent before `ready` and on timer calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
}

/// One file listed by a pending repository offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferedFile {
    /// The pending offer.
    pub request_id: RequestId,
    /// One of its items' `files`, exactly as listed.
    pub path: String,
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
    /// The sender's parent session, when the sender has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
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
    /// Answers a process extension's callback request with a value: what
    /// the Lua function returns; any JSON value, null included.
    Value {
        /// The return.
        result: Value,
    },
    /// Fails a process extension's callback request, as a Lua error does.
    Failure {
        /// The failure, `{ code, message }`.
        error: ProcessError,
    },
}

/// A process extension's callback failure, which fails the call as a Lua
/// error does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessError {
    /// The failure's code.
    pub code: ErrorCode,
    /// Fiber's own sentence.
    pub message: String,
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

/// The `args` of `job_input`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobInput {
    /// The running job.
    pub job_id: JobId,
    /// The keys as typed, escape sequences included.
    pub text: String,
}

/// The `args` of `mcp_login` and `mcp_reconnect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerArgs {
    /// The server's name.
    pub server: String,
}

/// The `args` of `history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryArgs {
    /// The session whose log `from_seq` and `to_seq` count in; absent means this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The first `seq` to return, inclusive; absent means the last lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_seq: Option<Seq>,
    /// The last `seq` to return, inclusive; absent means the latest line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_seq: Option<Seq>,
}

/// The `args` of `artifact_page`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactPageArgs {
    /// The path as a result's `artifact` names it.
    pub path: String,
    /// The byte offset to start reading from; absent means zero.
    #[serde(default)]
    pub offset: u64,
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

/// The `args` of `import`: `claude-code` or `codex`; absent means both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    /// The source to import; absent means both sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// The `args` of `quota`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaArgs {
    /// With `true`, starts a fetch for every label and answers once every
    /// fetch it started has ended; absent means answer from the cache.
    #[serde(default)]
    pub fetch: bool,
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
    /// A credential label of the session model's provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
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
    "effort",
    "credential",
    "scoped-models",
    "context",
    "usage",
    "cost",
    "quota",
    "status",
    "diff",
    "delegates",
    "agents",
    "copy",
    "pwd",
    "cwd",
    "tools",
    "hooks",
    "mcp",
    "panel",
    "rail",
    "rules",
    "reviewer-setup",
    "settings",
    "config",
    "theme",
    "keys",
    "keybindings",
    "skills",
    "rewind",
    "handoff",
    "name",
    "rename",
    "login",
    "logout",
    "approvals",
    "reload",
    "import",
    "clear",
    "close",
    "quit",
    "exit",
    "?",
    "help",
    "feedback",
    "bug",
];

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
