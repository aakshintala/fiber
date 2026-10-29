//! Every event kind and its payload (`docs/events.md`, "Kinds").
//!
//! A line is read as an [`Envelope`], which any build can read, and its
//! payload as an [`Event`] when this build knows the kind.

mod action;
mod context;
mod fit;
mod host;
mod offer;
mod session;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use action::{
    Anchor, Answer, ArgumentRepair, AskStep, AssistantMessageCompleted, CallStatus, Control,
    DecidedBy, Decision, Escalation, FileChange, FormAnswer, Grant, Interaction,
    InteractionRequested, InteractionResolved, MessageOutcome, PermissionRequested,
    PermissionResolved, Progress, RanBy, ReasoningCompleted, Repair, RepairFix, ResolvedBy,
    ReviewedCall, ReviewerRef, RuleOffer, RuleScope, SkillLoad, StandingRule, TextCompleted,
    TextDelta, ToolCallArgumentsDelta, ToolCallCompleted, ToolCallRequested, ToolCallStarted,
};
pub use context::{
    CacheLifetime, ContextNudged, DateChanged, Environment, ExtensionSectionSent, Git,
    HandoffCompleted, HandoffStarted, HandoffTrigger, InstructionFile, InstructionFileSent,
    InstructionReason, InstructionSent, KeptMessage, ModelChanged, ModelSettings, Note, Notice,
    OpeningMessage, Outcome, PreambleBuilt, PreambleReason, QuotaNoticed, QuotaRearmed,
    RetryScheduled, ReviewerKept, ReviewerUse, SentTool, SkillListed, SkillSent, SkillSource,
    SkillsChanged, SkillsResent, SwitchSource, ToolReplaced, UsageRecorded,
};
pub use host::{
    CallResult, CommandAccepted, CommandInfo, CommandRejected, CommandResult, CredentialLabels,
    DelegateFinished, DelegateStarted, ExtensionExec, ExtensionLog, ExtensionMessage,
    ExtensionStateSet, ExtensionStateUnset, ExtensionUi, ExtensionsLoaded, FinishedWorktree,
    HookInfo, JobCompleted, JobDelta, JobLine, JobStarted, JobsPendingNotified, LoadedExtension,
    McpServerFailed, McpServerLayer, McpServerReady, McpServerRow, McpServerState, OnFork,
    PendingReason, QuotaCredit, QuotaInfo, QuotaWindow, ReloadFailure, Reloaded, ReloadedServers,
    ServerFailure, SkillInfo, ToolInfo, ToolSource, ToolState, Ui,
};
/// The import answer's names and reasons, re-exported beside the other
/// acknowledgement payloads.
pub use host::{ImportedNames, LeftBehindEntry};
pub use offer::{
    OfferDecision, OfferedItem, OfferedKind, RepositoryCodeOffered, RepositoryCodeResolved,
};
pub use session::{
    Clients, ContextAdded, ContextFill, DirectoryAdded, FiberExited, FiberStarted, FinalMessage,
    InputItem, Moved, NamedBy, Parent, QueuedMessage, Rewind, Rewound, SessionNamed,
    SessionStarted, SessionState, SessionStatus, ShellCommand, SteeringApplied, SteeringQueue,
    TurnCompleted, TurnOutcome, TurnStarted, Variables, VariablesSource, Waiting, WaitingKind,
};

use crate::{CommandId, Envelope, RequestId, SessionId};

/// Whether a kind's lines are durable, carrying `seq` and making up the log,
/// or ephemeral (`docs/events.md`, "Durable and ephemeral").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A client that was not listening needs the line to know the session's
    /// true state.
    Durable,
    /// A later durable line makes the line obsolete.
    Ephemeral,
}

/// The payload of a kind whose payload is `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Empty {}

/// Declares [`Event`] from one row per kind: the variant, its payload type,
/// the kind's name and its class.
macro_rules! kinds {
    ($($variant:ident($payload:ty) = $name:literal, $class:ident;)+) => {
        /// One event, by kind, with its payload. It serializes as its payload
        /// alone.
        #[derive(Debug, Clone, PartialEq, Serialize)]
        #[serde(untagged)]
        pub enum Event {
            $(
                #[doc = concat!("`", $name, "`.")]
                $variant($payload),
            )+
        }

        impl Event {
            /// The kind's name, as the envelope's `kind` carries it.
            pub fn kind(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $name,)+
                }
            }

            /// Whether the kind is durable or ephemeral.
            pub fn class(&self) -> Class {
                match self {
                    $(Self::$variant(_) => Class::$class,)+
                }
            }

            /// Reads a line's payload. A kind this build does not know is
            /// `Ok(None)`, for the reader to skip; keys it does not know are
            /// ignored (`docs/events.md`, "Versioning").
            pub fn from_envelope(line: &Envelope) -> Result<Option<Self>, serde_json::Error> {
                Ok(Some(match line.kind.as_str() {
                    $($name => Self::$variant(<$payload>::deserialize(&line.payload)?),)+
                    _ => return Ok(None),
                }))
            }
        }

        #[cfg(test)]
        const KINDS: &[(&str, Class)] = &[$(($name, Class::$class),)+];
    };
}

impl Event {
    /// The payload, as the envelope's `payload` carries it, its keys sorted.
    pub fn payload(&self) -> Result<Map<String, Value>, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(self)?)
    }
}

/// `extension_hello`: the first line Fiber sends a process extension, with
/// the session's identity, why it started, its workspace, the extension's
/// data directories and its folded state. Sent again after a restart, with
/// the state as it stands in the session. A provider call outside a
/// session omits the session's id and carries reason `call`
/// (`docs/events.md`, `extension_hello`). The two runs are distinct
/// variants, so a sessionless hello cannot carry a session id and a
/// session hello cannot carry reason `call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtensionHello {
    /// A session's hello: the session's id and why it started.
    Session(SessionHello),
    /// A provider call outside a session: no session id, reason `call`.
    Call(CallHello),
}

/// A session's `extension_hello` payload: the session's id, why it
/// started, its workspace, the extension's data directories and its
/// folded state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHello {
    /// The session's id.
    pub session_id: SessionId,
    /// Why the session started.
    pub reason: SessionReason,
    /// The workspace root.
    pub workspace: String,
    /// The paths `host.data_dir` returns.
    pub data_dirs: ExtensionDataDirs,
    /// The extension's folded state, as `state.get` reads it.
    pub state: Map<String, Value>,
}

/// A sessionless `extension_hello` payload: reason `call`, the CLI
/// working directory, the real data directories and empty state, with
/// no session id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallHello {
    /// Why the run started: always `call` outside a session.
    pub reason: CallReason,
    /// The workspace root.
    pub workspace: String,
    /// The paths `host.data_dir` returns.
    pub data_dirs: ExtensionDataDirs,
    /// The extension's folded state, as `state.get` reads it: always
    /// empty outside a session.
    pub state: Map<String, Value>,
}

/// Why a session started, as a session `extension_hello` carries it: a
/// closed set. `call` is not here: a provider call outside a session
/// carries no session and uses [`ExtensionHello::Call`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionReason {
    /// A new session.
    New,
    /// A resumed session.
    Resume,
    /// A fork.
    Fork,
    /// A rewind.
    Rewind,
    /// A handoff.
    Handoff,
}

impl TryFrom<ExtensionReason> for SessionReason {
    type Error = ExtensionReason;

    /// Reads a session-start reason: every session start carries one of
    /// these. `call` never starts a session, so it reads back as the
    /// input it cannot narrow.
    fn try_from(reason: ExtensionReason) -> Result<Self, ExtensionReason> {
        match reason {
            ExtensionReason::New => Ok(Self::New),
            ExtensionReason::Resume => Ok(Self::Resume),
            ExtensionReason::Fork => Ok(Self::Fork),
            ExtensionReason::Rewind => Ok(Self::Rewind),
            ExtensionReason::Handoff => Ok(Self::Handoff),
            ExtensionReason::Call => Err(reason),
        }
    }
}

/// Why a sessionless run started, as a call `extension_hello` carries
/// it: only `call`, which never carries a session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallReason {
    /// A provider call outside a session.
    #[serde(rename = "call")]
    Call,
}

/// Why a session started, as `extension_hello` carries it: a closed set.
/// Session starts narrow this to [`SessionReason`]; `call` runs as
/// [`ExtensionHello::Call`] with no session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionReason {
    /// A new session.
    New,
    /// A resumed session.
    Resume,
    /// A fork.
    Fork,
    /// A rewind.
    Rewind,
    /// A handoff.
    Handoff,
    /// A provider call outside a session.
    Call,
}

/// The paths `host.data_dir` returns, as `extension_hello` carries them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDataDirs {
    /// The machine data directory.
    pub machine: String,
    /// The project data directory.
    pub project: String,
}

/// `extension_request`: one callback Fiber asks a process extension to run,
/// answered with a driver `reply` naming its `request_id`
/// (`docs/events.md`, `extension_request`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRequest {
    /// The request the extension answers.
    pub request_id: RequestId,
    /// The registering `call` command's id.
    pub registration: CommandId,
    /// The function's key, or its 1-based position as a string for a
    /// positional argument.
    pub key: String,
    /// The arguments the Lua function receives, as a JSON array.
    pub args: Vec<Value>,
}

kinds! {
    FiberStarted(FiberStarted) = "fiber_started", Durable;
    FiberExited(FiberExited) = "fiber_exited", Durable;
    SessionStarted(SessionStarted) = "session_started", Durable;
    Rewound(Rewound) = "rewound", Durable;
    Moved(Moved) = "moved", Durable;
    TurnStarted(TurnStarted) = "turn_started", Durable;
    StepStarted(Empty) = "step_started", Durable;
    TurnCompleted(TurnCompleted) = "turn_completed", Durable;
    SteeringApplied(SteeringApplied) = "steering_applied", Durable;
    SteeringQueue(SteeringQueue) = "steering_queue", Ephemeral;
    ShellCommand(ShellCommand) = "shell_command", Durable;
    DirectoryAdded(DirectoryAdded) = "directory_added", Durable;
    SessionNamed(SessionNamed) = "session_named", Durable;
    Clients(Clients) = "clients", Ephemeral;
    SessionStatus(SessionStatus) = "session_status", Ephemeral;
    ContextAdded(ContextAdded) = "context_added", Durable;
    AssistantMessageStarted(Empty) = "assistant_message_started", Durable;
    AssistantMessageDelta(TextDelta) = "assistant_message_delta", Ephemeral;
    AssistantMessageCompleted(AssistantMessageCompleted) = "assistant_message_completed", Durable;
    TextCompleted(TextCompleted) = "text_completed", Durable;
    ToolCallArgumentsDelta(ToolCallArgumentsDelta) = "tool_call_arguments_delta", Ephemeral;
    ReasoningStarted(Empty) = "reasoning_started", Durable;
    ReasoningDelta(TextDelta) = "reasoning_delta", Ephemeral;
    ReasoningCompleted(ReasoningCompleted) = "reasoning_completed", Durable;
    ToolCallRequested(ToolCallRequested) = "tool_call_requested", Durable;
    ToolCallStarted(ToolCallStarted) = "tool_call_started", Durable;
    ToolCallDelta(Progress) = "tool_call_delta", Ephemeral;
    ToolCallCompleted(ToolCallCompleted) = "tool_call_completed", Durable;
    PermissionRequested(PermissionRequested) = "permission_requested", Durable;
    PermissionResolved(PermissionResolved) = "permission_resolved", Durable;
    InteractionRequested(InteractionRequested) = "interaction_requested", Durable;
    InteractionResolved(InteractionResolved) = "interaction_resolved", Durable;
    RepositoryCodeOffered(RepositoryCodeOffered) = "repository_code_offered", Durable;
    RepositoryCodeResolved(RepositoryCodeResolved) = "repository_code_resolved", Durable;
    UsageRecorded(UsageRecorded) = "usage_recorded", Durable;
    QuotaNoticed(QuotaNoticed) = "quota_noticed", Durable;
    QuotaRearmed(QuotaRearmed) = "quota_rearmed", Durable;
    RetryScheduled(RetryScheduled) = "retry_scheduled", Ephemeral;
    Notice(Notice) = "notice", Ephemeral;
    PreambleBuilt(PreambleBuilt) = "preamble_built", Durable;
    ModelChanged(ModelChanged) = "model_changed", Durable;
    OpeningMessage(OpeningMessage) = "opening_message", Durable;
    InstructionFile(InstructionFile) = "instruction_file", Durable;
    DateChanged(DateChanged) = "date_changed", Durable;
    SkillsChanged(SkillsChanged) = "skills_changed", Durable;
    HandoffStarted(HandoffStarted) = "handoff_started", Durable;
    HandoffCompleted(HandoffCompleted) = "handoff_completed", Durable;
    SkillsResent(SkillsResent) = "skills_resent", Durable;
    ContextNudged(ContextNudged) = "context_nudged", Durable;
    ReviewerKept(ReviewerKept) = "reviewer_kept", Durable;
    McpServerFailed(McpServerFailed) = "mcp_server_failed", Durable;
    McpServerReady(McpServerReady) = "mcp_server_ready", Durable;
    Reloaded(Reloaded) = "reloaded", Durable;
    ExtensionsLoaded(ExtensionsLoaded) = "extensions_loaded", Durable;
    ExtensionStateSet(ExtensionStateSet) = "extension_state_set", Durable;
    ExtensionStateUnset(ExtensionStateUnset) = "extension_state_unset", Durable;
    ExtensionUi(ExtensionUi) = "extension_ui", Ephemeral;
    ExtensionMessage(ExtensionMessage) = "extension_message", Ephemeral;
    ExtensionLog(ExtensionLog) = "extension_log", Ephemeral;
    ExtensionExec(ExtensionExec) = "extension_exec", Durable;
    ExtensionHello(ExtensionHello) = "extension_hello", Ephemeral;
    ExtensionRequest(ExtensionRequest) = "extension_request", Ephemeral;
    JobStarted(JobStarted) = "job_started", Durable;
    DelegateStarted(DelegateStarted) = "delegate_started", Durable;
    JobDelta(JobDelta) = "job_delta", Ephemeral;
    JobLine(JobLine) = "job_line", Durable;
    DelegateFinished(DelegateFinished) = "delegate_finished", Durable;
    JobCompleted(JobCompleted) = "job_completed", Durable;
    JobsPendingNotified(JobsPendingNotified) = "jobs_pending_notified", Durable;
    CommandAccepted(CommandAccepted) = "command_accepted", Ephemeral;
    CommandRejected(CommandRejected) = "command_rejected", Ephemeral;
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
