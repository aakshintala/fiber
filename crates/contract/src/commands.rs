//! Every driver command (`docs/invocation.md`, "Driver commands").
//!
//! A line with a key the command line or the command does not take fails to
//! read, so an older Fiber says no to a newer client's key instead of ignoring
//! it.

use serde::{Deserialize, Serialize};

use crate::events::Decision;
use crate::shapes::{Mode, True};
use crate::{CommandId, JobId, RequestId, Seq, SessionId};

/// One command line: one JSON object on the driver channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandLine {
    /// Minted by the client from random bytes. Acknowledgements and events
    /// name the command by it, as `command_id`.
    pub id: CommandId,
    /// The command, as `command` and its `args`.
    #[serde(flatten)]
    pub command: Command,
    /// On `steer` and `reply`, the delegate the command is for; absent means
    /// the session this client drives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// A driver command, keyed by `command`, with its keys under `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "args", rename_all = "snake_case")]
pub enum Command {
    /// Starts a turn.
    Prompt(ContentArgs),
    /// Sends a steering message.
    Steer(ContentArgs),
    /// Replaces a queued steering message's text.
    SteerAmend(SteerAmend),
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
    /// Switches model, effort or thinking at the next turn boundary.
    Model(ModelArgs),
    /// Switches the permission mode at the next turn boundary.
    Mode(ModeArgs),
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
    /// Accepts no more prompts, finishes what is running, and exits.
    Close,
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

/// The `args` of `steer_amend`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerAmend {
    /// The `steer` command's id.
    pub command_id: CommandId,
    /// The new message.
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
/// or an approval's.
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

/// The `args` of `model`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelArgs {
    /// A model reference as a person types one.
    pub model: String,
    /// The reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The thinking level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// The `args` of `mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeArgs {
    /// The permission mode.
    pub mode: Mode,
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

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
