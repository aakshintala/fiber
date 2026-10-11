//! The hook seam (`docs/architecture.md`, "Hook seam"): "here is what is
//! about to happen: allow it, change it, or refuse it". The loop asks
//! through [`Hooks`] and never learns which extension answered, except as
//! data on the answer. `contract` holds no behaviour beyond checking that a value fits the
//! vocabulary, and pure conversions of its own values.

use serde_json::{Map, Value};

use crate::events::{CallStatus, Notice};
use crate::shapes::Process;

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

/// The session's hooks. Calls come from the loop thread, one at a time.
pub trait Hooks: Send + Sync {
    /// Runs every `after_tool` hook on `call`, in order, and returns what
    /// they decided.
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer;
    /// Hands the session loop's inbox to the hooks, so a program an
    /// extension ran can be logged as `extension_exec`.
    fn deliver_to(&self, inbox: std::sync::mpsc::Sender<crate::inbox::Delivery>);
}
