//! The shapes several payloads share (`docs/events.md`, "Payload types").
//! An optional key is absent when it does not apply, never `null`.

use std::collections::BTreeMap;

use serde::de::{Error as _, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{CommandId, ErrorCode, Seq, SessionId};

/// `error`: why something failed (`docs/errors.md`, "The shape").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    /// A stable label a consumer switches on.
    pub code: ErrorCode,
    /// Fiber's own sentence, saying what to do when there is a fix.
    pub message: String,
    /// On a failed model call, the seconds the provider asked Fiber to wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<f64>,
    /// On a failed model call, what the provider itself said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderFailure>,
}

/// The provider's side of a failed model call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFailure {
    /// The provider's name.
    pub name: String,
    /// The HTTP status.
    pub status: u16,
    /// The provider's own message.
    pub message: String,
}

/// `process`: how a process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    /// The exit code, when the process exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The signal's name, such as `SIGKILL`, when a signal ended the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// Whether Fiber stopped it at its timeout.
    pub timed_out: bool,
}

/// One content part, keyed by `type`. The set is open: a part this build does
/// not know reads as [`ContentPart::Unknown`], which a consumer shows as a
/// placeholder and never writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Text.
    Text {
        /// The text.
        text: String,
    },
    /// An image file in the session's `artifacts/`. The log never holds an
    /// image's bytes.
    Image {
        /// The file's path, relative to the session directory.
        path: String,
        /// Its type, such as `image/png`.
        mime_type: String,
        /// Its width in pixels.
        width: u32,
        /// Its height in pixels.
        height: u32,
    },
    /// A part this build does not know.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// One effect a tool call declares (`docs/permissions.md`, "Effects").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// It reads.
    Reads,
    /// It writes.
    Writes,
    /// It executes.
    Executes,
    /// It uses the network.
    Network,
}

/// Declared effects, on `tool_call_started` and `permission_requested`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredEffects {
    /// Each effect that applies; empty when the call declared none.
    pub effects: Vec<Effect>,
    /// Whether the call is reversible.
    pub reversible: bool,
    /// The paths the call touches, where the tool declared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
}

/// `tokens`: a model call's token counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Input tokens neither read from nor written to the cache.
    pub input: u64,
    /// Input tokens read from the cache.
    pub cache_read: u64,
    /// Input tokens written to the cache, keyed by cache lifetime (`"5m"`,
    /// `"1h"`); empty when nothing was written.
    pub cache_write: BTreeMap<String, u64>,
    /// Output tokens, reasoning included.
    pub output: u64,
}

/// `usage`: totals over some set of model calls, folded from their
/// `usage_recorded` lines. They are output, never a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// The calls' tokens, summed.
    pub tokens: Tokens,
    /// US dollars billed per token, summed over the calls without
    /// `subscription` whose cost is known; `0` when there were none; `null`
    /// when there were some and none had a known cost.
    #[serde(deserialize_with = "nullable")]
    pub cost: Option<f64>,
    /// US dollars at API prices for the calls with `subscription`; `0` when
    /// there were none.
    pub subscription_cost: f64,
}

/// One `ask_user` question as the model called it (`docs/tools.md`, "The
/// call"). Its keys are the tool's argument names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// The question's short header.
    pub header: String,
    /// The question.
    pub question: String,
    /// The options offered.
    pub options: Vec<Choice>,
    /// Whether several options may be chosen.
    #[serde(
        rename = "multiSelect",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub multi_select: Option<bool>,
}

/// An option a question or an interaction offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// The option's label, which an answer names.
    pub label: String,
    /// What the option means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Where a message came from: `source`, and the key that goes with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Origin {
    /// A client's command.
    Driver,
    /// An extension's `host.drive`.
    Extension {
        /// The extension's name.
        extension: String,
    },
    /// Another session's `session_message`.
    Session {
        /// The sending session.
        from_session_id: SessionId,
    },
}

/// The keys of "Where a message came from".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// Who sent it.
    #[serde(flatten)]
    pub origin: Origin,
    /// The `prompt`, `steer` or `message` command that sent it.
    pub command_id: CommandId,
}

/// A point in a session's log that a fork or a rewind continues from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    /// The session.
    pub session_id: SessionId,
    /// The position in its log.
    pub seq: Seq,
}

/// A git worktree a delegate runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// Its path.
    pub path: String,
    /// Its branch.
    pub branch: String,
}

/// A marker key whose only value is `true`, such as `declined` or `skipped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct True;

impl Serialize for True {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for True {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Ok(True)
        } else {
            Err(D::Error::invalid_value(Unexpected::Bool(false), &"true"))
        }
    }
}

/// Reads a key that is required but may be `null`. serde reads a missing
/// `Option` key as `None` unless a field names its own deserializer, so this
/// makes a missing key an error while `null` stays `None`.
pub(crate) fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}
