//! Runs turns and steps (`docs/loop.md`): the only thing that decides what
//! happens next, and the only writer of durable events
//! (`docs/architecture.md`, "The threads").
//!
//! A [`Loop`] runs on the session's loop thread. It blocks on its inbox while
//! idle, starts a turn from everything waiting there, and streams each model
//! reply on its own thread (`docs/architecture.md`, "One inbox" and
//! "Streaming").

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

pub(crate) use completion::Step;
use contract::ErrorCode;
use contract::events::{CacheLifetime, Event, Grant, PreambleReason, SessionStarted, ToolReplaced};
use contract::inbox::Delivery;
use contract::provider::{Cost, Input, ModelRequest, Provider, ToolDefinition};
use contract::shapes::Failure;
use contract::shapes::Worktree;
use contract::tool::Tool;
use log::Log;
use serde_json::{Map, Value};
pub(crate) use util::{ended, mint, variables};

mod answer;
mod asking;
mod calls;
mod cancel;
mod caps;
mod changes;
mod commands;
mod completion;
mod conversation;
mod diag;
mod error;
mod handoff;
mod history;
mod hooks;
mod hosted;
mod inbox;
mod interactions;
mod jobs;
mod late_cost;
mod offer;
mod opening;
mod permission;
mod process;
mod progress;
mod prompt;
mod questions;
mod resume;
mod retry;
mod reviewer;
mod rewind;
mod schema;
mod shutdown;
mod skill_header;
mod skills;
mod status;
mod step;
mod suspend;
mod switch;
mod turn;
mod usage;
mod util;
mod warm;

pub use cancel::TurnCancel;
pub use caps::{ResultCaps, capped};
pub use commands::commands;
pub use conversation::rebuild;
pub use error::Error;
pub use handoff::HandoffSettings;
pub use history::forked;
pub use permission::Permissions;
pub use process::{Exited, extensions_loaded, fiber_exited, fiber_started, mcp_servers_started};
pub use prompt::PromptInputs;
pub use resume::{Resumed, resumed};
pub use retry::Retry;
pub use reviewer::{BlockLimits, NO_MODEL_MESSAGE, Reviewer};
pub use rewind::{Rewound, rewind_note};
pub use switch::{Hosted, NO_SWITCH, Prepare, Prepared, Switchable};

/// The model a session's calls reach, and the prices those calls are logged
/// at (`docs/model-routing.md`, "Cost").
pub struct Model {
    /// The model reference, `provider/model`.
    pub reference: String,
    /// Declared prices. `None` leaves a call's `cost` null.
    pub cost: Option<Cost>,
    /// Whether a subscription login serves it. Its calls are logged with
    /// `subscription`, and `budget.usd` never counts them.
    pub subscription: bool,
}

/// One session's loop.
pub struct Loop {
    log: Arc<Log>,
    diag: diag::SessionDiag,
    provider: Arc<dyn Provider>,
    /// The model `provider` reaches, and how its calls are priced.
    model: Model,
    /// What the one preamble build reads, once (`docs/prompt-cache.md`,
    /// "The preamble").
    prompt: prompt::PromptInputs,
    /// Why the one build writes `preamble_built`: `start` on a new
    /// session, `resume` on a resumed one.
    preamble_reason: PreambleReason,
    /// The built preamble: what every request sends and what
    /// `preamble_built` records. `None` until the first turn builds it
    /// (`docs/prompt-cache.md`, "The preamble").
    preamble: Option<Preamble>,
    inbox: Receiver<Delivery>,
    /// The signal that cancels the running turn, armed while one runs.
    pub(crate) cancel: Arc<TurnCancel>,
    /// The root session's id, for providers that route by key.
    cache_key: String,
    /// Steering messages and job notices taken and not yet written, in
    /// arrival order: held while the loop waited on an approval, taken by
    /// the end-of-turn check, or taken in the drain that is about to apply
    /// them.
    pub(crate) queued: VecDeque<jobs::Queued>,
    /// `close` has been taken. No further turn starts
    /// (`docs/invocation.md`, "Lifecycle").
    closing: bool,
    /// A turn cut short on a pending approval, folded at resume: the next
    /// `turn` finishes it before starting any new one.
    pub(crate) suspended: Option<resume::Suspended>,
    /// Deliveries held aside across the finishing turn, in arrival order:
    /// `wait_for_turn` takes them first, ahead of the channel.
    pub(crate) deferred: VecDeque<Delivery>,
    /// Job notices a resume logged behind a suspended turn's open batch:
    /// they join the conversation after that batch's results.
    pub(crate) held: Vec<Input>,
    /// The conversation, built from the durable events as they are written
    /// and never by re-reading the log (`docs/loop.md`, "What the model is
    /// sent").
    conversation: Vec<Input>,
    /// How much of `conversation` the previous request sent.
    sent: Option<usize>,
    /// The registered tools by name.
    tools: BTreeMap<String, calls::Registered>,
    /// Each tool registered under a name already taken, recorded on
    /// `preamble_built` with each tool's `registered_by`.
    replaced: Vec<ToolReplaced>,
    /// The workspace, symlinks resolved.
    workspace: PathBuf,
    /// Fiber home's `credentials/` directory, symlinks resolved.
    credentials: PathBuf,
    /// Each configured `file` credential source, resolved at start.
    credential_files: Vec<PathBuf>,
    /// The standing rules, re-read every time a call reaches step 2.
    rules: Arc<dyn contract::rules::Rules>,
    /// The session grants this loop's answers added, in order: the fold of
    /// the `permission_resolved` lines this loop wrote.
    grants: Vec<Grant>,
    /// What judges step 7's calls; `Err(no_model)` until `reviewer` sets
    /// one (`docs/permissions.md`, "How it runs").
    reviewer: Result<Reviewer, Failure>,
    /// The person's `reviewer.context` notes, rendered once when the loop
    /// is built: every reviewer request carries them after the shared
    /// instructions, byte-stable across a model switch and unchanged by a
    /// config file changing on disk (`docs/permissions.md`, "What the
    /// person tells it").
    reviewer_notes: String,
    /// When a reviewer block hands the call to a person.
    limits: BlockLimits,
    /// What the reviewer is shown, rendered from the same events as
    /// `conversation` (`docs/permissions.md`, "What it is shown").
    reviewed: Vec<reviewer::Reviewed>,
    /// The reviewer's cache key: the session's id plus `reviewer`
    /// (`docs/prompt-cache.md`, "Rules for other areas").
    reviewer_key: String,
    /// How much of `reviewed` the previous reviewer request sent.
    reviewer_sent: Option<usize>,
    /// Consecutive reviewer blocks; a reviewer allow or a person's answer
    /// to a review escalation resets it.
    consecutive: u64,
    /// Reviewer blocks this session; never reset.
    session_blocks: u64,
    /// Whether the `no_model` notice was written.
    no_model_noticed: bool,
    /// A headless block-budget exhaustion, ending the turn once this step's
    /// calls complete (`docs/permissions.md`, "Headless").
    turn_blocked: Option<Failure>,
    /// The workspace as `session_started` records it, for the reviewer's
    /// effects item.
    workspace_label: String,
    /// Whether a person can answer an approval; `false` for `fiber ask`.
    answerable: bool,
    /// The offer of the repository's code before the first request.
    repository: offer::State,
    /// Whether the opening message was already written: a resume over a
    /// log holding one writes none, the conversation rebuild renders it
    /// from the log.
    opened: bool,
    /// The instruction files the model was sent and the directories
    /// checked (`docs/system-prompt.md`, "When something changes").
    changes: changes::State,
    /// Whether this turn's previous reply was cut off by the output limit.
    cut_off: bool,
    /// Usage lines this loop has written, latest per generation.
    ledger: usage::Ledger,
    /// Costs looked up after their calls ended, to write as they settle.
    late_cost: late_cost::LateCost,
    /// `budget.usd` for this session. `None` is no limit.
    budget: Option<f64>,
    /// How a failed model call is retried (`docs/model-routing.md`, "When
    /// a model call fails").
    retry: Retry,
    /// How long an idle wait lasts. `None` never exits.
    idle_exit: Option<std::time::Duration>,
    /// Set when an idle deadline ended an approval wait.
    idle_left: bool,
    /// The session's hooks; `None` runs none.
    hooks: Option<Arc<dyn contract::hook::Hooks>>,
    /// Handoff: the settings, the context's measure and what its render
    /// carries (`docs/handoff.md`).
    handoff: handoff::State,
    /// The session's jobs, and how far the loop is in ending with them.
    ending: jobs::Ending,
    /// `cache.warm_cap` when `cache.warm_idle` is set: how many cache
    /// lifetimes after the last turn an idle wait keeps the cache warm.
    /// `None` never warms (`docs/prompt-cache.md`, "Warming while idle").
    warm: Option<u32>,
    /// The last step's request and when it was handed to the provider, or
    /// the last refresh's send: what a refresh resends and counts from.
    /// Kept only while warming is on.
    last_request: Option<(ModelRequest, std::time::Instant)>,
    /// How a `model` command is prepared, with what the session started with.
    switcher: Option<(Prepare, Switchable)>,
    /// Switches admitted during a turn, applied at the next turn boundary.
    pending: Vec<Prepared>,
    /// The session's own thinking choice.
    chosen: Option<contract::ThinkingLevel>,
    /// When a switch cleared the last request while warming.
    warm_stopped: Option<std::time::Instant>,
    /// Puts a wake in the inbox; `None` leaves tool interactions unanswerable.
    inbox_wake: Option<Arc<dyn contract::clock::Wake>>,
}

/// The one built preamble: what every request sends and what
/// `preamble_built` records (`docs/prompt-cache.md`, "The preamble").
#[derive(Debug, Clone)]
struct Preamble {
    /// The system prompt text as sent.
    system_prompt: String,
    /// The tools as sent, in name order.
    tools: Vec<ToolDefinition>,
    /// The tools as the parent session sent them: a rewound session's
    /// first request sends these verbatim, so it matches its parent's
    /// bytes (`docs/events.md`, "Rewind"). `None` sends what `tools`
    /// wires; any later build clears it back to `None`.
    sent_tools: Option<Vec<Map<String, Value>>>,
    /// The tool choice as sent, and as `preamble_built` records it.
    tool_choice: String,
    /// The cache lifetime as sent, and as `preamble_built` records it.
    cache_lifetime: CacheLifetime,
    /// The session's one reasoning setting, as `preamble_built` records it.
    thinking: Option<contract::ThinkingLevel>,
}

impl Loop {
    /// Starts a new session's loop, writing `session_started`. `tools` are
    /// registered by name, each with who registered it: `builtin`, or the
    /// extension or MCP server. A later one replaces an earlier one of the
    /// same name (`docs/architecture.md`, "Tool seam"). `worktree` is the
    /// worktree the session runs in, when Fiber created one for it.
    #[allow(
        clippy::too_many_arguments,
        reason = "the session's whole start: its worktree rides last"
    )]
    pub fn start(
        log: Arc<Log>,
        provider: Arc<dyn Provider>,
        model: Model,
        prompt: prompt::PromptInputs,
        inbox: Receiver<Delivery>,
        tools: Vec<(String, Arc<dyn Tool>)>,
        permissions: Permissions,
        worktree: Option<Worktree>,
    ) -> Result<Self, Error> {
        let (tools, replaced) = calls::register(tools);
        let workspace = PathBuf::from(&permissions.workspace);
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        let credentials = permission::resolved(permissions.credentials);
        let started = log.append(
            &Event::SessionStarted(SessionStarted {
                workspace: permissions.workspace.clone(),
                variables: variables(),
                parent: None,
                forked_from: None,
                rewind: None,
                worktree,
            }),
            None,
            None,
        )?;
        // Nothing read yet: the first turn writes the opening message
        // and rebuilds the state from it.
        let changes = changes::State::empty(&prompt.home);
        // `session_started` renders nothing into the conversation.
        let diag = diag::SessionDiag::new(
            &prompt.home,
            started.session_id.clone(),
            Arc::clone(log.clock()),
        );
        Ok(Self {
            log,
            diag,
            provider,
            model,
            prompt,
            preamble_reason: PreambleReason::Start,
            preamble: None,
            inbox,
            cancel: Arc::new(TurnCancel::default()),
            // A new session is its own root (`docs/prompt-cache.md`, "Cache
            // markers and keys").
            cache_key: started.session_id.0.clone(),
            reviewer_key: format!("{}:reviewer", started.session_id.0),
            queued: VecDeque::new(),
            closing: false,
            suspended: None,
            deferred: VecDeque::new(),
            held: Vec::new(),
            conversation: Vec::new(),
            sent: None,
            tools,
            replaced,
            workspace,
            credentials,
            credential_files: permissions
                .credential_files
                .into_iter()
                .map(permission::resolved)
                .collect(),
            rules: permissions.rules,
            grants: Vec::new(),
            reviewer: Err(Failure {
                code: ErrorCode::NoModel,
                message: NO_MODEL_MESSAGE.to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
            limits: BlockLimits::default(),
            reviewer_notes: String::new(),
            reviewed: Vec::new(),
            reviewer_sent: None,
            consecutive: 0,
            session_blocks: 0,
            no_model_noticed: false,
            turn_blocked: None,
            workspace_label: permissions.workspace,
            answerable: true,
            repository: offer::State::default(),
            opened: false,
            changes,
            cut_off: false,
            ledger: usage::Ledger::default(),
            late_cost: late_cost::LateCost::default(),
            budget: None,
            retry: Retry::default(),
            idle_exit: None,
            idle_left: false,
            hooks: None,
            handoff: handoff::State::new(handoff::Carry::default()),
            ending: jobs::Ending::default(),
            warm: None,
            last_request: None,
            switcher: None,
            pending: Vec::new(),
            chosen: None,
            warm_stopped: None,
            inbox_wake: None,
        })
    }

    /// The signal that cancels this loop's running turn
    /// (`docs/architecture.md`, "Cancellation"). Without it the loop
    /// arms a signal nobody cancels, so an unwired loop still works.
    pub fn cancelled_by(mut self, cancel: Arc<TurnCancel>) -> Self {
        self.cancel = cancel;
        self
    }

    /// Caps billed spend at `usd` US dollars. `None` is no limit
    /// (`docs/loop.md`, "Spending budget").
    pub fn budget(mut self, usd: Option<f64>) -> Self {
        self.budget = usd;
        self
    }

    /// Retries a failed model call with `retry` (`docs/model-routing.md`,
    /// "When a model call fails"). Default [`Retry::default`].
    pub fn retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    /// Whether a person can answer an approval; `false` for `fiber ask`
    /// (`docs/permissions.md`, "Headless"). Default `true`.
    pub fn answerable(mut self, yes: bool) -> Self {
        self.answerable = yes;
        self
    }

    /// Who judges step 7's calls, and when a block hands the call to a
    /// person (`docs/permissions.md`, "The reviewer"). Without it every
    /// reviewed call escalates `no_model`, with the default limits.
    pub fn reviewer(mut self, reviewer: Result<Reviewer, Failure>, limits: BlockLimits) -> Self {
        self.reviewer = reviewer;
        self.limits = limits;
        self
    }

    /// The person's `reviewer.context` notes, rendered once when the loop
    /// is built (`docs/permissions.md`, "What the person tells it").
    /// Called once, before `run`: a model switch replaces the reviewer
    /// but not the notes, so the prefix stays byte-stable across a
    /// switch.
    pub fn reviewer_notes(mut self, notes: String) -> Self {
        self.reviewer_notes = notes;
        self
    }
}
