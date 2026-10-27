//! The hook seam (`docs/architecture.md`, "Hook seam"): "here is what is
//! about to happen: allow it, change it, or refuse it". The loop asks
//! through [`Hooks`] and never learns which extension answered, except as
//! data on the answer. `contract` holds no behaviour beyond checking that a value fits the
//! vocabulary, and pure conversions of its own values.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::ErrorCode;
use crate::SessionId;
use crate::events::{
    CallStatus, Event, ExtensionStateSet, ExtensionStateUnset, HandoffTrigger, HookInfo, InputItem,
    Notice, OnFork,
};
use crate::provider::Input;
use crate::shapes::{ContentPart, DeclaredEffects, Process, Usage};
use crate::tool::{Cancel, SpillFile};

/// What an `after_tool` hook is given (`docs/extensions.md`, "The hook
/// points"): a call that ran and has returned, before its output is cut,
/// written as an artifact or logged.
#[derive(Debug, Clone, Copy)]
pub struct AfterToolCall<'a> {
    /// The tool's name, as the model called it; for a delivery, the tool
    /// that started the job, `""` when none did.
    pub tool: &'a str,
    /// The arguments it ran with.
    pub arguments: &'a Map<String, Value>,
    /// `completed`, `failed` or `cancelled`: the status the completion
    /// carries.
    pub status: CallStatus,
    /// The full text of the output's text parts, joined.
    pub content: &'a str,
    /// The tool's `details`.
    pub details: Option<&'a Value>,
    /// How the call's process ended, when it ran one.
    pub process: Option<&'a Process>,
    /// The call's artifact file, when the tool already wrote it
    /// (`docs/tools.md`, "Result and output"): the hook rewrites it
    /// through `host.fs` or returns artifact text that replaces it.
    pub spilled: Option<&'a SpillFile>,
    /// Which job delivery the hook is seeing, when this is one:
    /// `completion`, `monitor` or `delegate` for a delivery to the model,
    /// `output_file` for a finished job's whole output file. `None` for an
    /// ordinary call.
    pub delivery: Option<JobDelivery>,
    /// When the `output_file` pass found the path is not a regular file:
    /// the pass failed before any hook ran, and the answer synthesizes
    /// that failure from each hook's `on_failure`
    /// (`docs/extensions.md`, "When a hook fails"). `content` is then
    /// empty, and no hook runs. False for every other call.
    pub not_regular: bool,
}

/// Which job delivery an `after_tool` hook is seeing
/// (`docs/extensions.md`, "The hook points").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobDelivery {
    /// A job's completion notice, before it joins the conversation.
    Completion,
    /// One batch of a monitor's lines.
    Monitor,
    /// A delegate's final message.
    Delegate,
    /// A finished job's whole output file.
    OutputFile,
}

impl JobDelivery {
    /// The `delivery` wire word the hook is shown.
    pub fn as_str(self) -> &'static str {
        match self {
            JobDelivery::Completion => "completion",
            JobDelivery::Monitor => "monitor",
            JobDelivery::Delegate => "delegate",
            JobDelivery::OutputFile => "output_file",
        }
    }
}

/// What the chain of `after_tool` hooks decided.
#[derive(Debug, Clone, PartialEq)]
pub enum AfterToolOutcome {
    /// No hook changed anything: the tool's output is the output.
    Unchanged,
    /// The chain's final `content` and `details`, each `None` when no hook
    /// returned it, and the last artifact text a hook returned.
    Changed {
        /// Replacement text for the output's text parts.
        content: Option<String>,
        /// Replacement `details`.
        details: Option<Value>,
        /// What the artifact holds instead of the returned output.
        artifact: Option<String>,
    },
    /// A hook of the command's own extension could not run: the call
    /// fails with `hook_failed`, and its output is withheld.
    Failed(HookFailure),
    /// A `blocking` hook of `extension` failed: the output is withheld.
    Withheld {
        /// The extension whose hook failed.
        extension: String,
    },
}

/// The answer synthesizing a failed `output_file` pass over a path that is
/// not a regular file, from the chain's registered `on_failure` values, in
/// run order (`docs/extensions.md`, "When a hook fails"): a pure
/// conversion of the chain's own data, and no hook runs. `chain` holds each
/// registered `after_tool` hook's extension and whether it is `blocking`.
/// Empty: there is no pass to fail, so `Unchanged`. A `blocking` hook: the
/// first one in run order withholds, so the pass fails closed. All
/// `non-blocking`: `Unchanged` with a `notice` per extension, so the path
/// is kept and every failure is named.
pub fn not_regular_answer(chain: &[(String, bool)]) -> AfterToolAnswer {
    for (extension, blocking) in chain {
        if *blocking {
            return AfterToolAnswer {
                outcome: AfterToolOutcome::Withheld {
                    extension: extension.clone(),
                },
                changed_by: vec![extension.clone()],
                notices: Vec::new(),
            };
        }
    }
    AfterToolAnswer {
        outcome: AfterToolOutcome::Unchanged,
        changed_by: Vec::new(),
        notices: chain
            .iter()
            .map(|(extension, _)| Notice {
                code: ErrorCode::HookFailed,
                message: "The `after_tool` hook failed, so its change was dropped: the job's output file is not a regular file.".to_owned(),
                extension: Some(extension.clone()),
            })
            .collect(),
    }
}

/// The answer to an `after_tool` call.
#[derive(Debug, Clone, PartialEq)]
pub struct AfterToolAnswer {
    /// What the chain decided.
    pub outcome: AfterToolOutcome,
    /// The extensions that changed the result, in run order, each once.
    pub changed_by: Vec<String>,
    /// The notices the chain raised, such as a `non-blocking` hook's failure.
    pub notices: Vec<Notice>,
}

/// What a `before_tool` hook is given (`docs/extensions.md`, "The hook
/// points"): a call that passed its schema check and its effects function,
/// before permission is decided.
#[derive(Debug, Clone, Copy)]
pub struct BeforeToolCall<'a> {
    /// The tool's name, as the model called it.
    pub tool: &'a str,
    /// The arguments the model gave, or a hook before it left.
    pub arguments: &'a Map<String, Value>,
    /// The effects the call declared.
    pub declared: &'a DeclaredEffects,
}

/// A hook's refusal: the extension and the reason it gave.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    /// The extension whose hook refused.
    pub extension: String,
    /// The reason the hook gave.
    pub reason: String,
}

/// A hook's failure: the extension and what went wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct HookFailure {
    /// The extension whose hook failed.
    pub extension: String,
    /// What went wrong.
    pub message: String,
}

/// What the chain of `before_tool` hooks decided.
#[derive(Debug, Clone, PartialEq)]
pub enum BeforeToolOutcome {
    /// No hook changed anything: the call runs as asked.
    Unchanged,
    /// The chain's final arguments and the extensions that rewrote them,
    /// in run order, each once.
    Rewritten {
        /// The arguments the call runs with.
        arguments: Map<String, Value>,
        /// The extensions whose hook returned `arguments`.
        changed_by: Vec<String>,
    },
    /// A hook refused the call: it never starts.
    Refused(Refusal),
    /// A `blocking` hook failed: the call never starts.
    Failed(HookFailure),
}

/// The answer to a `before_tool` call.
#[derive(Debug, Clone, PartialEq)]
pub struct BeforeToolAnswer {
    /// What the chain decided.
    pub outcome: BeforeToolOutcome,
    /// The notices the chain raised, such as a `non-blocking` hook's failure.
    pub notices: Vec<Notice>,
}

/// What the chain of `check` hooks decided.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckOutcome {
    /// No hook refused: the guarded thing may happen.
    Passed,
    /// A hook refused it: it does not happen.
    Refused(Refusal),
    /// A `blocking` hook failed: it does not happen.
    Failed(HookFailure),
}

/// The answer to a `check` call.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckAnswer {
    /// What the chain decided.
    pub outcome: CheckOutcome,
    /// The notices the chain raised, such as a `non-blocking` hook's failure.
    pub notices: Vec<Notice>,
}

/// Who sent a session message: the sending session and its parent, if
/// the sender has one. `BeforeMessage.sender` holds this for a session
/// message and is absent for a person's or a driver's message, so a
/// parent without a sender cannot be written.
#[derive(Debug, Clone, Copy)]
pub struct MessageSender<'a> {
    /// The sending session.
    pub from_session_id: &'a SessionId,
    /// The sender's parent session, when the sender has one.
    pub parent_session_id: Option<&'a SessionId>,
}

/// What a `before_message` hook is given (`docs/extensions.md`, "The hook
/// points"): a person's, a driver's, or another session's message, before
/// it is logged. A session message carries its sender; the loop answers
/// the sender only after the hook passes, so a refusal or a blocking
/// failure rejects the sender and nothing is logged or sent.
#[derive(Debug, Clone, Copy)]
pub struct BeforeMessage<'a> {
    /// The message's content parts.
    pub content: &'a [ContentPart],
    /// Who sent it, for a session message; absent otherwise.
    pub sender: Option<MessageSender<'a>>,
}

/// A `before_message` replacement: the content and who changed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Replaced {
    /// The message's replacement content.
    pub content: Vec<ContentPart>,
    /// The extensions whose hook returned `content`, in run order, each once.
    pub changed_by: Vec<String>,
}

/// Context a hook added, and who added it. A `session_start`, `turn_start`
/// or `turn_end` hook returns it as context to append to the conversation;
/// at `turn_end` the texts are messages that continue the turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    /// State writes owned by this context, written immediately before it.
    pub state: Vec<StateLine>,
    /// The extension whose hook returned it.
    pub extension: String,
    /// The context to add.
    pub text: String,
}

/// What the chain of `before_message` hooks decided.
#[derive(Debug, Clone, PartialEq)]
pub enum BeforeMessageOutcome {
    /// The message may be logged and sent, replaced and with context.
    Passed {
        /// The replacement and who changed it, when a hook returned `content`.
        replaced: Option<Replaced>,
        /// The contexts the hooks added, in run order.
        context: Vec<Context>,
    },
    /// A hook refused the message: it is neither logged nor sent.
    Refused(Refusal),
    /// A `blocking` hook failed: the message is neither logged nor sent.
    Failed(HookFailure),
}

/// The answer to a `before_message` call.
#[derive(Debug, Clone, PartialEq)]
pub struct BeforeMessageAnswer {
    /// What the chain decided.
    pub outcome: BeforeMessageOutcome,
    /// The notices the chain raised, such as a `non-blocking` hook's failure.
    pub notices: Vec<Notice>,
}

/// What a `before_handoff` hook is given (`docs/extensions.md`, "The hook
/// points"): what started the handoff, the person's instructions, and the
/// conversation the note request would be built from, without the note
/// request.
#[derive(Clone, Copy)]
pub struct BeforeHandoff<'a> {
    /// What started the handoff.
    pub trigger: HandoffTrigger,
    /// The person's instructions, when there were any.
    pub instructions: Option<&'a str>,
    /// The conversation the note request would be built from, without
    /// the note request.
    pub conversation: &'a [Input],
    /// The loop's cancel signal: set for a cancel or a shutdown.
    pub cancel: &'a dyn Cancel,
}

impl std::fmt::Debug for BeforeHandoff<'_> {
    // The debug string is a developer aid no doc promises and no consumer observes.
    #[cfg_attr(false, mutants::skip)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BeforeHandoff")
            .field("trigger", &self.trigger)
            .field("instructions", &self.instructions)
            .field("conversation", &self.conversation)
            .finish_non_exhaustive()
    }
}

/// What the chain of `before_handoff` hooks decided.
#[derive(Debug, Clone, PartialEq)]
pub enum BeforeHandoffOutcome {
    /// No hook wrote a note: Fiber makes its own note request.
    NoNote,
    /// The last note a hook returned, and the extension that wrote it.
    /// The text is never empty after trimming.
    Note {
        /// The note's text.
        text: String,
        /// The extension whose note was last returned.
        extension: String,
    },
    /// A `blocking` hook failed: the handoff does not complete. It
    /// carries no note.
    Failed(HookFailure),
}

/// The answer to a `before_handoff` call.
#[derive(Debug, Clone, PartialEq)]
pub struct BeforeHandoffAnswer {
    /// What the chain decided.
    pub outcome: BeforeHandoffOutcome,
    /// The notices the chain raised, such as a `non-blocking` hook's failure.
    pub notices: Vec<Notice>,
}

/// Folded extension state: extension, key, whole value and its last fork rule.
pub type StateSeed = BTreeMap<String, BTreeMap<String, (Value, OnFork)>>;

/// A state write carried beside the change its hook returned.
#[derive(Debug, Clone, PartialEq)]
pub struct StateLine {
    /// The extension that owns the key.
    pub extension: String,
    /// The key written.
    pub key: String,
    /// The set value and fork rule, or none for an unset.
    pub value: Option<(Value, OnFork)>,
    /// The write's position in the session's state view.
    pub landing: u64,
}

impl StateLine {
    /// Builds the state event to write before the changed event.
    pub fn into_event(self) -> Event {
        match self.value {
            Some((value, on_fork)) => Event::ExtensionStateSet(ExtensionStateSet {
                extension: self.extension,
                key: self.key,
                value,
                on_fork,
            }),
            None => Event::ExtensionStateUnset(ExtensionStateUnset {
                extension: self.extension,
                key: self.key,
            }),
        }
    }
}

/// The session's hooks. Calls come from the loop thread, one at a time.
pub trait Hooks: Send + Sync {
    /// Drains admitted non-hook and unchanged-hook writes in landing order.
    // The default is an empty list either way: `vec![]` changes no behaviour a consumer sees.
    #[cfg_attr(false, mutants::skip)]
    fn take_state_lines(&self) -> Vec<Event> {
        Vec::new()
    }

    /// Admits carried writes, after draining any queued writes first.
    fn admit_state_lines(&self, lines: Vec<StateLine>) -> Vec<Event> {
        lines.into_iter().map(StateLine::into_event).collect()
    }

    /// Takes the last hook answer's state lines that belong to no context.
    // The default is an empty list either way: `vec![]` changes no behaviour a consumer sees.
    #[cfg_attr(false, mutants::skip)]
    fn take_change_state(&self) -> Vec<StateLine> {
        Vec::new()
    }

    /// Runs every `after_tool` hook on `call`, in order, and returns what
    /// they decided.
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer;
    /// Runs the `sanitize` and `transform` `before_tool` hooks on `call`,
    /// in order, and returns what they decided.
    fn before_tool(&self, call: &BeforeToolCall<'_>) -> BeforeToolAnswer;
    /// Runs the `check` `before_tool` hooks on `call`, in order, and
    /// returns whether it may run.
    fn check_tool(&self, call: &BeforeToolCall<'_>) -> CheckAnswer;
    /// Runs every `before_message` hook on `message`, in order, and returns
    /// what they decided.
    fn before_message(&self, message: &BeforeMessage<'_>) -> BeforeMessageAnswer;
    /// Runs every `session_start` hook, in order, and returns the contexts
    /// they added.
    fn session_start(&self, start: &SessionStart<'_>) -> ContextAnswer;
    /// Runs every `turn_start` hook on the turn's input, in order, and
    /// returns the contexts they added.
    fn turn_start(&self, turn: &TurnStart<'_>) -> ContextAnswer;
    /// Runs every `before_model_call` hook on the built request, in order,
    /// and returns whether it may be sent.
    fn before_model_call(&self, call: &BeforeModelCall<'_>) -> CheckAnswer;
    /// Runs every `turn_end` hook on the model's final reply, in order, and
    /// returns the messages they continued the turn with.
    fn turn_end(&self, end: &TurnEnd<'_>) -> ContextAnswer;
    /// Runs every `before_handoff` hook on the handoff, in order, and
    /// returns the note they wrote, if any.
    fn before_handoff(&self, handoff: &BeforeHandoff<'_>) -> BeforeHandoffAnswer;
    /// Hands the session loop's inbox to the hooks, so a program an
    /// extension ran can be logged as `extension_exec`.
    fn deliver_to(&self, inbox: std::sync::mpsc::Sender<crate::inbox::Delivery>);

    /// Returns once every `host.exec` run started by an extension that delivers
    /// to the session inbox has ended and its `extension_exec` line is queued.
    /// Every VM that delivers to the session inbox is waited on.
    fn wait_for_runs(&self) {}

    /// The hooks and watchers the session registered, hooks first in
    /// hook-point table order and run order, then watchers in registration
    /// order (`docs/invocation.md`, "What each command does", `hooks`).
    // The default is an empty list either way: `vec![]` changes no behaviour a consumer sees.
    #[cfg_attr(false, mutants::skip)]
    fn registrations(&self) -> Vec<HookInfo> {
        Vec::new()
    }
}

/// Why a session started, as a `session_start` hook is told
/// (`docs/extensions.md`, "The hook points").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartReason {
    /// A new session.
    Start,
    /// A resumed session.
    Resume,
    /// A fork of another session's conversation.
    Fork,
    /// A rewound session.
    Rewind,
    /// A handoff, once the note is ready.
    Handoff,
}

/// What a `session_start` hook is given (`docs/extensions.md`, "The hook
/// points"): why the session started, its id and its workspace root.
#[derive(Clone, Copy)]
pub struct SessionStart<'a> {
    /// Why the session started.
    pub reason: StartReason,
    /// The session's id.
    pub session_id: &'a SessionId,
    /// The session's workspace root.
    pub workspace: &'a str,
    /// The loop's cancel signal: set for a cancel or a shutdown.
    pub cancel: &'a dyn Cancel,
}

impl std::fmt::Debug for SessionStart<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStart")
            .field("reason", &self.reason)
            .field("session_id", &self.session_id)
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// What a `turn_start` hook is given (`docs/extensions.md`, "The hook
/// points"): the turn's input and what started it.
#[derive(Clone, Copy)]
pub struct TurnStart<'a> {
    /// Everything that started the turn, in arrival order.
    pub input: &'a [InputItem],
    /// The loop's cancel signal: set for a cancel or a shutdown.
    pub cancel: &'a dyn Cancel,
}

impl std::fmt::Debug for TurnStart<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnStart")
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

/// What a `before_model_call` hook is given (`docs/extensions.md`, "The
/// hook points"): the model reference, and the session's `usage` so far,
/// its delegates included.
#[derive(Clone, Copy)]
pub struct BeforeModelCall<'a> {
    /// The model reference the request is built for.
    pub model: &'a str,
    /// The session's usage so far, its delegates included.
    pub usage: &'a Usage,
    /// The loop's cancel signal: set for a cancel or a shutdown.
    pub cancel: &'a dyn Cancel,
}

impl std::fmt::Debug for BeforeModelCall<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BeforeModelCall")
            .field("model", &self.model)
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}

/// What a `turn_end` hook is given (`docs/extensions.md`, "The hook
/// points"): the model's final reply.
#[derive(Clone, Copy)]
pub struct TurnEnd<'a> {
    /// The text parts of the reply that ended the step, joined.
    pub reply: &'a str,
    /// The loop's cancel signal: set for a cancel or a shutdown.
    pub cancel: &'a dyn Cancel,
}

impl std::fmt::Debug for TurnEnd<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnEnd")
            .field("reply", &self.reply)
            .finish_non_exhaustive()
    }
}

/// What the chain of `session_start`, `turn_start` or `turn_end` hooks
/// decided. At `turn_end` the added texts are messages that continue the
/// turn, and an empty `Added` means the turn ends.
#[derive(Debug, Clone, PartialEq)]
pub enum ContextOutcome {
    /// The contexts the hooks added, in run order.
    Added(Vec<Context>),
    /// A `blocking` hook failed: none of the chain's contexts is kept.
    Failed(HookFailure),
}

/// The answer to a `session_start`, `turn_start` or `turn_end` call.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextAnswer {
    /// What the chain decided.
    pub outcome: ContextOutcome,
    /// The notices the chain raised, such as a `non-blocking` hook's failure.
    pub notices: Vec<Notice>,
}

#[cfg(test)]
#[path = "hook_tests.rs"]
mod tests;
