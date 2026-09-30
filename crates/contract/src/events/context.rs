//! Payloads of `docs/events.md`, "Usage and notices", "Preamble", "Opening
//! message" and "Handoff".

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::shapes::{Failure, Tokens};
use crate::{ActionId, ErrorCode, GenerationId};

/// `usage_recorded`: one per model call, whatever started it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecorded {
    /// The provider's id for the generation.
    pub generation_id: GenerationId,
    /// The model reference, `provider/model`.
    pub model: String,
    /// The call's tokens.
    pub tokens: Tokens,
    /// Hosted web searches, where the provider reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_searches: Option<u64>,
    /// In US dollars; `null` when unknown.
    pub cost: Option<f64>,
    /// The extension whose `host.model` made the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

/// `quota_noticed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaNoticed {
    /// The provider's name.
    pub provider: String,
    /// The window's name, as the provider reports it.
    pub window: String,
    /// The percent used when the notice was given.
    pub percent_used: f64,
    /// When the window resets, where the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    /// The threshold it crossed, `quota.notice_at`.
    pub notice_at: f64,
}

/// `retry_scheduled`. The envelope's `action_id` is the failed assistant
/// message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryScheduled {
    /// The failed call's `error.code`.
    pub code: ErrorCode,
    /// The attempt about to be made.
    pub attempt: u32,
    /// The wait before it.
    pub delay_ms: u64,
}

/// `notice`: a failure outside any action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// An open set; a consumer shows the message for an unknown code.
    pub code: ErrorCode,
    /// Fiber's own sentence.
    pub message: String,
    /// The extension it concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

/// Why a preamble was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreambleReason {
    /// A new session.
    Start,
    /// A resumed session.
    Resume,
    /// A reload.
    Reload,
    /// A model switch.
    Switch,
}

/// A prompt cache lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheLifetime {
    /// Five minutes.
    #[serde(rename = "5m")]
    FiveMinutes,
    /// One hour.
    #[serde(rename = "1h")]
    OneHour,
}

/// One tool as a preamble sent it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentTool {
    /// The tool's name.
    pub name: String,
    /// Whether its definition was deferred.
    pub deferred: bool,
    /// The definition in the protocol's own shape.
    pub definition: Map<String, Value>,
}

/// `preamble_built` (`docs/prompt-cache.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreambleBuilt {
    /// Why it was built.
    pub reason: PreambleReason,
    /// The model reference.
    pub model: String,
    /// The model's context window, in tokens.
    pub context_window: u64,
    /// The context size at which an automatic handoff runs; absent when
    /// automatic handoff is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_at: Option<u64>,
    /// The reasoning effort, where the model takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The thinking level, where the model takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// The tool choice as sent.
    pub tool_choice: String,
    /// The cache lifetime.
    pub cache_lifetime: CacheLifetime,
    /// The system prompt text as sent.
    pub system_prompt: String,
    /// Each tool as sent.
    pub tools: Vec<SentTool>,
}

/// A model and its settings, as on `preamble_built`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSettings {
    /// The model reference.
    pub model: String,
    /// The reasoning effort, where the model takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The thinking level, where the model takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// The cache lifetime.
    pub cache_lifetime: CacheLifetime,
}

/// Who asked for a model switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum SwitchSource {
    /// A client's command.
    Driver,
    /// An extension.
    Extension {
        /// The extension's name.
        extension: String,
    },
}

/// `model_changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChanged {
    /// The settings before the switch.
    pub before: ModelSettings,
    /// The settings after it.
    pub after: ModelSettings,
    /// Who asked for it.
    #[serde(flatten)]
    pub source: SwitchSource,
}

/// `opening_message` (`docs/system-prompt.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningMessage {
    /// The environment.
    pub environment: Environment,
    /// Each instruction file sent, in order.
    pub instruction_files: Vec<InstructionFileSent>,
    /// The skills listing; its entries are not yet specified
    /// (`docs/system-prompt.md`, "Skills listing").
    pub skills: Vec<Value>,
}

/// The environment an opening message describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    /// The date, `YYYY-MM-DD`.
    pub date: String,
    /// The operating system, such as `linux` or `macos`.
    pub os: String,
    /// The architecture, such as `x86_64` or `aarch64`.
    pub arch: String,
    /// The shell.
    pub shell: String,
    /// The workspace.
    pub workspace: String,
    /// Present in a git repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<Git>,
    /// The session log's path.
    pub session_log: String,
}

/// The git state an opening message describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Git {
    /// The branch; `null` when HEAD is detached.
    pub branch: Option<String>,
}

/// One instruction file an opening message sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionFileSent {
    /// The file's path.
    pub path: String,
    /// Its content.
    pub content: String,
}

/// Why an instruction file line was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionReason {
    /// Found in a subdirectory.
    Subdirectory,
    /// Created.
    Created,
    /// Changed.
    Changed,
    /// Deleted.
    Deleted,
    /// Changed by the session's own call.
    OwnEdit,
}

/// What the model was sent about an instruction file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionSent {
    /// The whole file.
    Full,
    /// A diff.
    Diff,
    /// That it was deleted.
    Deleted,
    /// Nothing.
    None,
}

/// `instruction_file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionFile {
    /// The file's path.
    pub path: String,
    /// Why it was written.
    pub reason: InstructionReason,
    /// The file's content now; absent when deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// What the model was sent.
    pub sent: InstructionSent,
}

/// `date_changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateChanged {
    /// The new date, `YYYY-MM-DD`.
    pub date: String,
}

/// What started a handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffTrigger {
    /// The automatic threshold.
    Auto,
    /// A person.
    Person,
    /// A context overflow.
    Overflow,
}

/// `handoff_started` (`docs/handoff.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffStarted {
    /// What started it.
    pub trigger: HandoffTrigger,
}

/// How a handoff or a job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It completed.
    Completed,
    /// It failed.
    Failed,
    /// It was cancelled.
    Cancelled,
}

/// `handoff_completed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffCompleted {
    /// How it ended.
    pub outcome: Outcome,
    /// On `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
    /// The actions carrying the note, in call order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Vec<ActionId>>,
    /// A note a `before_handoff` hook wrote, in place of `note`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_text: Option<String>,
    /// With `note_text`, the hook's extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// The context size before the handoff, in tokens.
    pub tokens_before: u64,
    /// The person's instructions, when there were any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// `context_nudged`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextNudged {
    /// The context size when the nudge was given.
    pub tokens: u64,
    /// The context size at which an automatic handoff runs.
    pub trigger_at: u64,
}
