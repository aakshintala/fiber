//! The tool seam (`docs/architecture.md`, "Tool seam"): "run this and give me
//! a result". A built-in tool and an extension's register through [`Tool`]
//! alike; the loop looks a call's name up among them and never learns whose
//! it is.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value};

use crate::ActionId;
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
    "delegate_fork",
    "delegate_message",
    "delegate_models",
    "delegate_spawn",
    "edit",
    "handoff",
    "jobs",
    "name_session",
    "read",
    "session_list",
    "session_message",
    "session_search",
    "shell",
    "skill",
    "tool_search",
    "web_fetch",
    "web_search",
    "write",
];

/// What a tool may ask of the signal that cancels its call
/// (`docs/tools.md`, "Cancellation"). The trait lives in
/// [`support::cancel`](support::cancel) and is re-exported here
/// (`docs/architecture.md`, "The modules").
pub use support::cancel::Cancel;

/// Runs the inner calls of the call it belongs to (`docs/extensions.md`,
/// "Running a tool").
pub trait InnerCalls: Send + Sync {
    /// Runs `name` with `arguments` as an inner call `extension` makes,
    /// blocking until its completion is written. A self-call or depth cut
    /// returns an error in the calling code and writes no line. A request
    /// the loop never takes returns a cancelled completion when closed.
    fn run(
        &self,
        extension: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Box<crate::events::ToolCallCompleted>, String>;
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

    /// The runner for this call's inner calls; `None` where none can be made.
    fn inner_calls(&self) -> Option<Arc<dyn InnerCalls>> {
        None
    }
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

    /// Runs the call as [`Tool::run_asking`] does, with the exact `effects`
    /// the loop prepared before `tool_call_started`: the declared decision
    /// a later reload must not change. The loop calls only this. A tool
    /// that needs nothing beyond its arguments keeps the default, which
    /// forwards to [`Tool::run_asking`] unchanged.
    fn run_with_effects(
        &self,
        arguments: &Map<String, Value>,
        effects: &Effects,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        let _ = effects;
        self.run_asking(arguments, cancel, emit, ask)
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

    /// A per-call note for the reviewer, if the call needs one beyond its
    /// declared effects (`docs/tools.md`, "Shell", "Effects"). It is
    /// part of the one review request, never the system prompt or a tool
    /// definition, so the session's cache is untouched (`docs/permissions.md`,
    /// "What it is shown"). The default is none; only the shell sets one
    /// today.
    fn review_note(&self, _arguments: &Map<String, Value>) -> Option<String> {
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
    /// When true confinement does not skip review for this call: a future
    /// confined fast path must not decide it. Session grants and standing
    /// allows still apply, as do the credential deny and the standing deny
    /// and ask rules. Any tool may set it; it only makes the call
    /// stricter. Today only the shell sets it, for the built-in
    /// destructive-command guard (`docs/tools.md`, "What a tool declares").
    pub review_when_confined: bool,
    /// Network hosts contacted, each named by an HTTP(S) URL
    /// (`docs/tools.md`, "What a tool declares"). `None` is unknown;
    /// an empty list contacts no host the model chose.
    pub hosts: Option<Vec<String>>,
}

/// Why an effects function could not classify a call. Either way the call
/// fails closed (`docs/tools.md`, "Before a call runs").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectsError {
    /// The arguments passed the schema but name something the tool cannot
    /// classify, such as a path it cannot resolve; the message says what.
    #[error("{0}")]
    Arguments(String),
    /// The tool's own code failed, such as an extension's Lua raising an
    /// error; the message is the failure.
    #[error("{0}")]
    Tool(String),
}

/// What a call returned (`docs/tools.md`, "What a result carries"). The loop
/// adds the status and, when it cuts the result, the artifact, unless the
/// tool already wrote it: when `spilled` is set the loop takes the output's
/// text from it alone and ignores any text parts in `content` (its other
/// parts, such as images, are kept).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    /// Hosts surfaced by this result, each named by an HTTP(S) URL
    /// (`docs/tools.md`, "What a tool declares").
    pub hosts_surfaced: Vec<String>,
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
    /// before its `tool_call_completed`: the records a call starts outside
    /// any queue, a shell job's start and a delegate's first. A resumed
    /// run's start is not among them: it leaves the queue only through
    /// `settle`, whichever taker writes it.
    pub jobs: Vec<JobRecord>,
    /// Takes this call's prefix of a job's queue, written before the
    /// call's completion (see [`Settle`]).
    pub settle: Option<Settle>,
    /// Server lines the loop writes, in order, under this call's action,
    /// before its job lines and `tool_call_completed`: what this call saw
    /// of its server's deaths and restarts. Set only for the call that
    /// observed each one, so each is written once.
    pub servers: Vec<ServerRecord>,
    /// Output the tool wrote to the call's artifact file as it ran, past
    /// what it holds in memory (`docs/tools.md`, "Result and output").
    /// Only the shell sets it.
    pub spilled: Option<Spilled>,
    /// The tools a search loaded, by name, best first, ties by name;
    /// `Some(vec![])` when it matched nothing; `None` on every other
    /// output. An output never sets both `loaded` and `spilled`: only the
    /// shell spills, and it never loads.
    pub loaded: Option<Vec<String>>,
}

/// Takes this call's prefix of a job's queue of unwritten records, which
/// the loop writes before the call's completion, under the queuing call's
/// action and turn. The loop calls it exactly once, on the loop thread,
/// in `finish`, after the `servers` lines and before `jobs`, for every
/// call whose output reaches `finish`, failed and cancelled included.
/// A tool that sets no `settle` behaves as today.
#[derive(Clone)]
pub struct Settle(pub Arc<dyn Fn() -> Vec<crate::jobs::Taken> + Send + Sync>);

impl std::fmt::Debug for Settle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Settle(..)")
    }
}

impl PartialEq for Settle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Output a tool wrote to its call's artifact file as it ran, past what it
/// holds in memory (`docs/tools.md`, "Result and output").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spilled {
    /// The first bytes, decoded.
    pub head: String,
    /// The last bytes, decoded.
    pub tail: String,
    /// Raw bytes decoded into `head`.
    pub head_bytes: u64,
    /// Raw bytes decoded into `tail`.
    pub tail_bytes: u64,
    /// Raw bytes neither holds.
    pub removed: u64,
    /// The kept file, or why it was not kept.
    pub file: Result<SpillFile, Unsaved>,
}

/// Why spilled output could not be kept as the call's artifact file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsaved {
    /// What went wrong, naming the path.
    pub message: String,
    /// A partial file the tool could not remove, for the loop to remove.
    pub left: Option<std::path::PathBuf>,
}

/// A spilled call's artifact file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillFile {
    /// The file's path, absolute.
    pub path: std::path::PathBuf,
    /// The file's length, closing lines included.
    pub size: u64,
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

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "tool_run_effects_tests.rs"]
mod tool_run_effects_tests;
