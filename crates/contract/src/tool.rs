//! The tool seam (`docs/architecture.md`, "Tool seam"): "run this and give me
//! a result". A built-in tool and an extension's register through [`Tool`]
//! alike; the loop looks a call's name up among them and never learns whose
//! it is.

use std::sync::Weak;

use serde_json::{Map, Value};

use crate::clock::Wake;
use crate::events::{Control, FileChange};
use crate::provider::ToolDefinition;
use crate::shapes::{ContentPart, DeclaredEffects, Failure, Process};

/// What a tool may ask of the signal that cancels its call
/// (`docs/tools.md`, "Cancellation"). The signal itself lives outside
/// `contract`, which holds no behaviour.
pub trait Cancel: Send + Sync {
    /// Whether the call has been cancelled.
    fn is_cancelled(&self) -> bool;

    /// Wakes `waker` when the call is cancelled from then on. A cancel
    /// before the subscription is not replayed: the caller checks
    /// [`Cancel::is_cancelled`] after subscribing.
    fn subscribe(&self, waker: Weak<dyn Wake>);
}

/// One tool (`docs/tools.md`, "What a tool declares"). Calls in a step run
/// concurrently, one thread each.
pub trait Tool: Send + Sync {
    /// Its name, description and input schema, as the model sees them.
    fn definition(&self) -> ToolDefinition;

    /// The call's effects, from arguments that passed the schema check. Fiber
    /// calls it before permission is decided. An error fails the call
    /// `tool_error`, and it never runs.
    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError>;

    /// Runs the call, blocking until it ends. `cancel` is how the call sees
    /// that Fiber has stopped it.
    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel) -> Output;

    /// How a long result is cut (`docs/tools.md`, "Bounded results").
    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// What a call declares before it runs (`docs/permissions.md`, "Effects" and
/// "What a rule matches").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effects {
    /// Its effects, whether it is reversible, and the paths it touches.
    pub declared: DeclaredEffects,
    /// Its primary argument; `Some("")` for a tool with none, and `None` for a
    /// call no rule can safely match.
    pub subject: Option<String>,
    /// The widening a rule would offer, such as `npm test`.
    pub prefix: Option<String>,
}

/// Why an effects function could not classify a call. Either way the call
/// fails closed (`docs/tools.md`, "Before a call runs").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectsError {
    /// The arguments passed the schema but name something the tool cannot
    /// classify, such as a path it cannot resolve; the message says what.
    Arguments(String),
    /// The tool's own code failed, such as an extension's Lua raising an
    /// error; the message is the failure.
    Tool(String),
}

impl std::fmt::Display for EffectsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Arguments(message) | Self::Tool(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for EffectsError {}

/// What a call returned (`docs/tools.md`, "What a result carries"). The loop
/// adds the status and, when it cuts the result, the artifact.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    /// Text and image parts for the model, before any cut.
    pub content: Vec<ContentPart>,
    /// Set when the call failed.
    pub error: Option<Failure>,
    /// On a call that ran a process.
    pub process: Option<Process>,
    /// Data for clients; never sent to the model.
    pub details: Option<Value>,
    /// On a call that changed files, one entry per file.
    pub changes: Option<Vec<FileChange>>,
    /// Instructions to the loop.
    pub control: Option<Control>,
}

/// How many bytes of a result's text the model is sent: the first `start`
/// and the last `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound {
    /// Bytes kept from the start.
    pub start: usize,
    /// Bytes kept from the end.
    pub end: usize,
}

impl Bound {
    /// A tool's bound unless it declares its own: the first 16 KiB.
    pub const DEFAULT: Self = Self {
        start: 16_384,
        end: 0,
    };
}

#[cfg(test)]
mod tests {
    use super::EffectsError;

    #[test]
    fn an_effects_error_reads_as_its_message() {
        assert_eq!(
            EffectsError::Arguments("no such path".into()).to_string(),
            "no such path"
        );
        assert_eq!(
            EffectsError::Tool("lua: boom".into()).to_string(),
            "lua: boom"
        );
    }
}
