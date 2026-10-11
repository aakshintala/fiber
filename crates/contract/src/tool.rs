//! The tool seam (`docs/architecture.md`, "Tool seam"): "run this and give me
//! a result". A built-in tool and an extension's register through [`Tool`]
//! alike; the loop looks a call's name up among them and never learns whose
//! it is.

use std::sync::{Arc, Weak};
use std::time::Instant;

use serde_json::{Map, Value};

use crate::ActionId;
use crate::clock::Wake;
use crate::emit::Emit;
use crate::events::{Answer, Control, FileChange, Interaction, McpServerFailed, McpServerReady};
use crate::jobs::JobRecord;
use crate::provider::ToolDefinition;
use crate::shapes::{ContentPart, DeclaredEffects, Failure, Process};

/// Every name a built-in tool can register under, `web_search` included
/// whether or not the session's model hosts a search. An extension tool
/// of one of these names replaces the built-in only when its manifest
/// lists it in `replaces` (`docs/extensions.md`, "What a package holds").
pub const BUILT_IN_TOOLS: &[&str] = &[
    "ask_user",
    "delegate_spawn",
    "edit",
    "handoff",
    "jobs",
    "read",
    "session_search",
    "shell",
    "skill",
    "web_fetch",
    "web_search",
    "write",
];

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

/// What a running call may ask whoever drives the session
/// (`docs/events.md`, "Interactions"). The loop writes the
/// `interaction_requested` and `interaction_resolved` lines; the call only
/// waits for the answer.
pub trait Ask: Send + Sync {
    /// The call's own `action_id`.
    fn action(&self) -> ActionId;

    /// Whether a person can answer an interaction this call raises: false
    /// in a session a program drives, after `close`, and with no inbox to
    /// read the reply (`docs/tools.md`, "Asking the person"). When false,
    /// [`Ask::ask`] returns [`Answered::NoAnswer`] at once.
    fn answerable(&self) -> bool;

    /// Raises `asking` under this call and blocks until it is resolved: by a
    /// `reply`, or by Fiber. Returns [`Answered::NoAnswer`] at once when
    /// nobody can answer (`docs/permissions.md`, "Headless").
    fn ask(&self, asking: Asking) -> Answered;
}

/// A further check on an answer that fits the interaction's kind. A reply
/// it refuses is rejected `invalid_arguments`, as an unfit one is
/// (`docs/invocation.md`, "Replying"). A decline never reaches it.
pub type Check = Box<dyn Fn(&Answer) -> bool + Send + Sync>;

/// One interaction a running call raises.
pub struct Asking {
    /// What it asks.
    pub interaction: Interaction,
    /// Every call it belongs to, this one included, in the order the line
    /// lists them; empty means this call only.
    pub action_ids: Vec<ActionId>,
    /// When it ends unanswered, on the session's clock; `None` never.
    pub until: Option<Instant>,
    /// A check beyond the kind's, if any.
    pub check: Option<Check>,
    /// The call does nothing else while this waits, and running it again
    /// with the same arguments raises the same interaction. The session may
    /// exit on it as on a pending approval, and resuming runs the call
    /// again (`docs/tools.md`, "When a person can answer").
    pub suspends: bool,
}

/// How an [`Ask::ask`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// A `reply` that fits, which may be a person's decline.
    Reply(Answer),
    /// Nobody answered: none was possible, or Fiber resolved it.
    NoAnswer,
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
    /// that Fiber has stopped it. `emit` carries its `tool_call_delta` lines:
    /// only that event is accepted through it, and it is borrowed for the
    /// call's duration only, so nothing emits after `run` returns.
    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output;

    /// Runs the call as [`Tool::run`] does, with `ask` for raising an
    /// interaction while it runs (`docs/events.md`, "Interactions"). The
    /// loop calls only this; a tool that never asks keeps the default,
    /// which runs [`Tool::run`].
    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        _ask: &dyn Ask,
    ) -> Output {
        self.run(arguments, cancel, emit)
    }

    /// How a long result is cut (`docs/tools.md`, "Bounded results").
    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }

    /// This tool with its results capped at `cap` bytes of model-facing text,
    /// for a tool that cuts its own output (`docs/tools.md`, "Bounded
    /// results"). The returned tool keeps this tool's `definition().name`.
    /// `None`, the default, leaves the cap to the loop's cut.
    fn with_cap(&self, _cap: usize) -> Option<Arc<dyn Tool>> {
        None
    }

    /// Guideline lines for the system prompt, if any
    /// (`docs/system-prompt.md`, "Tool guidelines").
    fn guidelines(&self) -> Option<String> {
        None
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
    /// When true the call skips the fast path, session grants and standing
    /// allows, and still meets the credential deny and the standing deny
    /// and ask rules.
    pub always_reviewed: bool,
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
    /// Job lines the loop writes, in order, under this call's action, just
    /// before its `tool_call_completed`.
    pub jobs: Vec<JobRecord>,
    /// Server lines the loop writes, in order, under this call's action,
    /// before its job lines and `tool_call_completed`: what this call saw
    /// of its server's deaths and restarts. Set only for the call that
    /// observed each one, so each is written once.
    pub servers: Vec<ServerRecord>,
}

/// One durable server line a call returns (`docs/mcp.md`, "When a server
/// dies").
#[derive(Debug, Clone, PartialEq)]
pub enum ServerRecord {
    /// `mcp_server_failed`: the server's start failed, or it died.
    Failed(McpServerFailed),
    /// `mcp_server_ready`: a server that died runs again after its restart.
    Ready(McpServerReady),
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
