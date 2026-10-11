//! The hook seam (`docs/architecture.md`, "Hook seam"): "here is what is
//! about to happen: allow it, change it, or refuse it". The loop asks
//! through [`Hooks`] and never learns which extension answered, except as
//! data on the answer. `contract` holds no behaviour beyond checking that a value fits the
//! vocabulary, and pure conversions of its own values.

use serde_json::{Map, Value};

use crate::events::{CallStatus, Notice};
use crate::shapes::{ContentPart, DeclaredEffects, Process};

/// What an `after_tool` hook is given (`docs/extensions.md`, "The hook
/// points"): a call that ran and has returned, before its output is cut,
/// written as an artifact or logged.
#[derive(Debug, Clone, Copy)]
pub struct AfterToolCall<'a> {
    /// The tool's name, as the model called it.
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
    /// A `blocking` hook of `extension` failed: the output is withheld.
    Withheld {
        /// The extension whose hook failed.
        extension: String,
    },
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

/// What a `before_message` hook is given (`docs/extensions.md`, "The hook
/// points"): a person's or a driver's message, before it is logged.
#[derive(Debug, Clone, Copy)]
pub struct BeforeMessage<'a> {
    /// The message's content parts.
    pub content: &'a [ContentPart],
}

/// A `before_message` replacement: the content and who changed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Replaced {
    /// The message's replacement content.
    pub content: Vec<ContentPart>,
    /// The extensions whose hook returned `content`, in run order, each once.
    pub changed_by: Vec<String>,
}

/// Context a `before_message` hook added, and who added it.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
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

/// The session's hooks. Calls come from the loop thread, one at a time.
pub trait Hooks: Send + Sync {
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
    /// Hands the session loop's inbox to the hooks, so a program an
    /// extension ran can be logged as `extension_exec`.
    fn deliver_to(&self, inbox: std::sync::mpsc::Sender<crate::inbox::Delivery>);
}
