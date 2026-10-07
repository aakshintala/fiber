//! Every event kind and its payload (`docs/events.md`, "Kinds").
//!
//! A line is read as an [`Envelope`], which any build can read, and its
//! payload as an [`Event`] when this build knows the kind.

mod action;
mod context;
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
    ReviewerRef, RuleOffer, RuleScope, StandingRule, TextCompleted, TextDelta,
    ToolCallArgumentsDelta, ToolCallCompleted, ToolCallRequested, ToolCallStarted,
};
pub use context::{
    CacheLifetime, ContextNudged, DateChanged, Environment, ExtensionSectionSent, Git,
    HandoffCompleted, HandoffStarted, HandoffTrigger, InstructionFile, InstructionFileSent,
    InstructionReason, InstructionSent, ModelChanged, ModelSettings, Note, Notice, OpeningMessage,
    Outcome, PreambleBuilt, PreambleReason, QuotaNoticed, RetryScheduled, SentTool, SkillListed,
    SkillSent, SkillSource, SkillsChanged, SkillsResent, SwitchSource, ToolReplaced, UsageRecorded,
};
pub use host::{
    CommandAccepted, CommandRejected, CommandResult, DelegateFinished, DelegateStarted,
    ExtensionExec, ExtensionLog, ExtensionMessage, ExtensionStateSet, ExtensionStateUnset,
    ExtensionUi, ExtensionsLoaded, FinishedWorktree, JobCompleted, JobDelta, JobLine, JobStarted,
    JobsPendingNotified, LoadedExtension, McpServerFailed, McpServerReady, OnFork, PendingReason,
    ReloadFailure, Reloaded, ReloadedServers, ServerFailure, ToolInfo, ToolSource, ToolState, Ui,
};
pub use offer::{
    OfferDecision, OfferedItem, OfferedKind, RepositoryCodeOffered, RepositoryCodeResolved,
};
pub use session::{
    Clients, ContextAdded, ContextFill, FiberExited, FiberStarted, FinalMessage, InputItem,
    NamedBy, Parent, QueuedMessage, Rewind, Rewound, SessionNamed, SessionStarted, SessionState,
    SessionStatus, ShellCommand, SteeringApplied, SteeringQueue, TurnCompleted, TurnOutcome,
    TurnStarted, Variables, VariablesSource, Waiting, WaitingKind,
};

use crate::Envelope;

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

kinds! {
    FiberStarted(FiberStarted) = "fiber_started", Durable;
    FiberExited(FiberExited) = "fiber_exited", Durable;
    SessionStarted(SessionStarted) = "session_started", Durable;
    Rewound(Rewound) = "rewound", Durable;
    TurnStarted(TurnStarted) = "turn_started", Durable;
    StepStarted(Empty) = "step_started", Durable;
    TurnCompleted(TurnCompleted) = "turn_completed", Durable;
    SteeringApplied(SteeringApplied) = "steering_applied", Durable;
    SteeringQueue(SteeringQueue) = "steering_queue", Ephemeral;
    ShellCommand(ShellCommand) = "shell_command", Durable;
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
