//! Runs turns and steps (`docs/loop.md`): the only thing that decides what
//! happens next, and the only writer of durable events
//! (`docs/architecture.md`, "The threads").
//!
//! A [`Loop`] runs on the session's loop thread. It blocks on its inbox while
//! idle, starts a turn from everything waiting there, and reads each model
//! reply itself, on the loop thread (`docs/architecture.md`, "One inbox" and
//! "Streaming").

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use crate::shutdown::ReleaseDelegates;
pub(crate) use completion::Step;
use contract::ErrorCode;
use contract::events::{
    CacheLifetime, CommandInfo, Event, Grant, Parent, PreambleReason, SessionStarted, ToolInfo,
    ToolReplaced, ToolState,
};
use contract::inbox::Delivery;
use contract::provider::{Cost, Input, Provider, ToolDefinition};
use contract::shapes::Failure;
use contract::shapes::Worktree;
use contract::steering;
use contract::tool::Tool;
use log::Log;
use serde_json::{Map, Value};
use support::lock;
pub(crate) use util::{ended, mint, variables};
pub use util::{set_variables_source, variables_source};

mod answer;
mod asking;
mod before_message;
mod before_tool;
mod calls;
mod cancel;
mod caps;
mod changes;
mod commands;
mod completion;
mod conversation;
mod delegated;
mod deliveries;
mod diag;
mod error;
mod handoff;
mod history;
mod hooks;
mod hosted;
mod inbox;
mod inner_calls;
mod interactions;
mod jobs;
mod known_hosts;
mod late_cost;
mod lifecycle;
mod media;
mod naming;
mod notes;
mod offer;
mod opening;
mod permission;
mod process;
mod progress;
mod prompt;
mod questions;
mod quota_notice;
mod reload;
mod render_loaded;
mod resume;
mod retry;
mod reviewer;
mod rewind;
mod schema;
mod shutdown;
mod skill_header;
mod skill_reader;
mod skill_set;
mod skills;
mod slash;
mod status;
mod step;
mod suspend;
mod switch;
#[cfg(test)]
mod timing_tests;
#[cfg(test)]
mod tool_timing_tests;
mod turn;
mod usage;
mod util;
mod warm;

pub use before_message::OnRewrite;
pub use before_tool::Prepared as ReviewPrepared;
pub use calls::Decided as ReviewDecision;
pub use cancel::TurnCancel;
pub use caps::{ResultCaps, capped};
pub use commands::{Commands, RunnableCommands, commands, skills};
pub use error::Error;
pub use handoff::HandoffSettings;
pub use history::forked;
pub use media::resolve_media;
pub use offer::{Approved, LoadApproved, OfferKey, OfferOutcome};
pub use permission::Permissions;
pub use process::{
    Exited, Ran, Summary, extensions_loaded, fiber_exited, fiber_started, mcp_servers_started,
};
pub use prompt::{Deferrals, DisabledReader, DocsPlace, PromptInputs};
pub use quota_notice::QuotaRead;
pub use reload::{
    PrepareReload, PromptPatch, ReconnectServer, Reconnected, ReloadPrepared, ReloadRoute,
    ReloadSettings, ServerSync,
};
pub use resume::{Resumed, resumed};
pub use retry::{Decision, Retry};
pub use reviewer::{
    BlockLimits, NO_MODEL_MESSAGE, Reviewed, Reviewer, Shown, render_reviewed, summary_skips,
};
pub use rewind::{
    Continuation, Rewound, fork_task, latest_name, rewind_note, rewind_note_with_jobs, rewound_from,
};
pub use skill_reader::SkillReader;
pub use skill_set::SkillSet;
pub use slash::{FetchPrompt, ServerPrompts};
pub use switch::{Hosted, NO_SWITCH, Prepare, Prepared, Switchable};
pub use usage::extension_line;

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

/// Builds the `tool_search` tool over the deferred definitions as sent and
/// their sources (`docs/tools.md`, "Tool search"): the search tool by
/// role, whatever its definition names it. The loop never names the tool.
/// `main` sets one; without it the loop defers nothing.
pub type ToolSearchFactory =
    Arc<dyn Fn(&[ToolDefinition], &[String]) -> Arc<dyn Tool> + Send + Sync>;

/// Publishes `/tools` rows as the loop's tool states move (`docs/tools.md`,
/// "Seeing the tools"): `declare` after every preamble build, `state`
/// when a loaded set moves. `main` wires the door's rows; without either
/// closure the loop publishes nothing.
#[allow(
    clippy::type_complexity,
    reason = "the setter holds two small closures; names would only rename them"
)]
#[derive(Clone)]
pub struct ToolRows {
    /// Adds or replaces the row named `name`, or removes it on `None`.
    pub declare: Arc<dyn Fn(&str, Option<ToolInfo>) + Send + Sync>,
    /// Moves the row named `name` to `state`.
    pub state: Arc<dyn Fn(&str, ToolState) + Send + Sync>,
}

/// Refreshes the session's `commands` answer after the turn-start check
/// changed the runnable set (`docs/invocation.md`, "What each command
/// does"): `extensions` reads the session's current extension commands,
/// and `replace` stores the recomputed rows. `main` wires both at the
/// command construction sites; without them the loop still writes its
/// `skills_changed` line, but no session row moves.
#[derive(Clone)]
pub struct CommandsRefresh {
    /// The session's current extension commands, in answer order.
    pub extensions: Arc<dyn Fn() -> Vec<CommandInfo> + Send + Sync>,
    /// Replaces the session's stored `commands` rows.
    pub replace: Arc<dyn Fn(Vec<CommandInfo>) + Send + Sync>,
}

/// One session's loop.
pub struct Loop {
    log: Arc<Log>,
    status_tap: Option<status::Tap>,
    diag: diag::SessionDiag,
    provider: Arc<dyn Provider>,
    /// The session's encoded images and PDFs, so each artifact is read and
    /// encoded once (`docs/model-routing.md`, "Image limits").
    media: media::Cache,
    /// The model `provider` reaches, and how its calls are priced.
    model: Model,
    /// Builds the session's observed model calls, one per attempt: `None`
    /// calls the provider directly, leaving `Provider::call` unchanged.
    observe: Option<contract::diag::Observe>,
    /// Builds the reviewer's observed calls at the first attempt: `None`
    /// calls the reviewer's provider directly.
    reviewer_observe: Option<contract::diag::Observe>,
    /// What the one preamble build reads, once (`docs/prompt-cache.md`,
    /// "The preamble").
    prompt: prompt::PromptInputs,
    /// The session's maintained skill set: what `/name` and the `skill`
    /// tool answer from (`docs/system-prompt.md`, "Skills").
    skills: skill_set::SkillSet,
    /// Why the latest build writes `preamble_built`: start, resume,
    /// switch or reload.
    preamble_reason: PreambleReason,
    /// The built preamble: what every request sends and what
    /// `preamble_built` records. `None` until the first turn builds it
    /// (`docs/prompt-cache.md`, "The preamble").
    preamble: Option<Preamble>,
    inbox: inbox::queue::Queue,
    /// The signal that cancels the running turn, armed while one runs.
    pub(crate) cancel: Arc<TurnCancel>,
    /// The root session's id, for providers that route by key.
    cache_key: String,
    /// Steering messages and job notices taken and not yet written, in
    /// arrival order: held while the loop waited on an approval, taken by
    /// the end-of-turn check, or taken in the drain that is about to apply
    /// them.
    pub(crate) queued: VecDeque<jobs::Queued>,
    /// The steering queue the door and this loop share
    /// (`docs/architecture.md`, "One inbox"): a reader admits into it
    /// at once, and this loop reconciles each delivery against it at the
    /// drain. A loop built without one shares nothing: its own queue
    /// admits direct-inbox senders exactly as before.
    pub(crate) steering: Arc<steering::Queue>,
    /// The turn of each call this loop started whose output has not
    /// reached `finish`, so a resumed run's start that the call queued is
    /// written under that call's turn, whichever taker writes it
    /// (`docs/events.md`, "`job_started`"). The loop thread alone reads
    /// and writes it.
    pub(crate) call_turns: BTreeMap<contract::ActionId, contract::TurnId>,
    /// `close` has been taken. No further turn starts
    /// (`docs/invocation.md`, "Lifecycle").
    closing: bool,
    /// `rewound` has been written: the session takes no more commands and
    /// the process ends once the drain is refused (`docs/events.md`,
    /// "Rewind").
    rewound: bool,
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
    /// The session's known network hosts, folded from trusted durable events.
    known_hosts: known_hosts::KnownHosts,
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
    /// `conversation` (`docs/permissions.md`, "What it is shown"). A
    /// completed handoff drops every tool call from it and keeps the
    /// person's messages, releasing the calls' allocation, so the resident
    /// state follows the context window, not the transcript
    /// (`docs/performance.md`, "What a budget covers").
    ///
    /// Public for the `review` jig's replay, which restores the exact
    /// reviewer input a source log's line had (`docs/testing.md`,
    /// "Jigs"); the loop itself renders it as its lines are written.
    pub reviewed: Vec<reviewer::Reviewed>,
    /// The reviewer's cache key: the session's id plus `reviewer`
    /// (`docs/prompt-cache.md`, "Rules for other areas").
    reviewer_key: String,
    /// The session's id plus `:notes`, isolating proposals from conversation and reviewer caches.
    // debt: missing docs/prompt-cache.md :notes row, Part 3 or 4's doc pass adds that row.
    notes_key: String,
    /// How much of `reviewed` the previous reviewer request sent. A
    /// completed handoff resets it, as a resume and a model switch do:
    /// `None` means no earlier prefix may be reused.
    reviewer_sent: Option<usize>,
    /// `permission_resolved` lines phase A of a concurrent batch buffered
    /// instead of appending: phase C drains each call's lines in request
    /// order at write-back. `None` outside phase A, when `decided` appends.
    decision_buffer: Option<Vec<contract::events::PermissionResolved>>,
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
    ///
    /// Public for the `review` jig's replay, which labels the effects
    /// item with the source session's workspace (`docs/testing.md`,
    /// "Jigs").
    pub workspace_label: String,
    /// Whether a person can answer an approval; `false` for `fiber ask`.
    answerable: bool,
    /// The session's MCP prompt rows and how to run one, if `main`
    /// started servers with prompts (`docs/mcp.md`, "Prompts and
    /// resources").
    server_prompts: Option<slash::ServerPrompts>,
    /// The offer of the repository's code before the first request.
    repository: offer::State,
    /// Whether the person's name pins the session against the model's
    /// `name_session` (`docs/tools.md`, "Naming the session").
    naming: naming::Naming,
    /// Whether the opening message was already written: a resume over a
    /// log holding one writes none, the conversation rebuild renders it
    /// from the log.
    opened: bool,
    /// The instruction files the model was sent and the directories
    /// checked (`docs/system-prompt.md`, "When something changes").
    changes: changes::State,
    /// Whether this turn's previous reply was cut off by the output limit.
    cut_off: bool,
    /// This process's lines, folded as they are written: what
    /// `fiber_exited` reports, without reading the log back
    /// (`docs/events.md`, `fiber_exited`).
    summary: Summary,
    /// Usage lines this loop has written, latest per generation.
    ledger: usage::Ledger,
    /// The spend the delegate-socket reader shares: per-generation billed
    /// costs, the accumulated sum, the budget and the one-way trip. The
    /// watcher applies each tapped line under its lock; the loop
    /// publishes each ledgered line under the same lock.
    pub(crate) spend_trip: std::sync::Arc<std::sync::Mutex<usage::Tripwire>>,
    /// Costs looked up after their calls ended, to write as they settle.
    late_cost: late_cost::LateCost,
    pub(crate) lifecycle: lifecycle::State,
    /// `budget.usd` for this session. `None` is no limit.
    budget: Option<f64>,
    /// True for a delegate's loop (`Loop::delegate`): its tree stop must
    /// preempt its failed-turn drain, so the session ends stopped.
    is_delegate: bool,
    /// How a failed model call is retried (`docs/model-routing.md`, "When
    /// a model call fails").
    retry: Retry,
    /// How long an idle wait lasts. `None` never exits.
    idle_exit: Option<std::time::Duration>,
    /// Set when an idle deadline ended an approval wait.
    idle_left: bool,
    /// The session's hooks; `None` runs none.
    hooks: Option<Arc<dyn contract::hook::Hooks>>,
    /// Forgets the normal end's extension-owned ids: `settled` calls it
    /// with the branch's running snapshot at the top of its normal-end branch,
    /// before the first drain, so a queued end for a released id is
    /// dropped and never runs `end_extension_job`. `None` releases
    /// nothing: every end still runs. Set by [`Loop::released`].
    release: Option<ReleaseDelegates>,
    /// Reports a replaced prompt's replacement to the door, before the
    /// prompt's acceptance (`docs/state.md`, "Prompt history").
    on_rewrite: Option<crate::before_message::OnRewrite>,
    /// Handoff: the settings, the context's measure and what its render
    /// carries (`docs/handoff.md`).
    handoff: handoff::State,
    /// The session's jobs, and how far the loop is in ending with them.
    ending: jobs::Ending,
    quota_notices: quota_notice::Notices,
    /// An idle wait's warming state (`docs/prompt-cache.md`, "Warming while idle").
    warming: warm::Warming,
    /// How a `model` command is prepared, with what the session started with.
    switcher: Option<(Prepare, Switchable)>,
    /// Prepares a replacement configuration without changing the route.
    reloader: Option<PrepareReload>,
    /// Restarts one failed MCP server; absent this, reconnect is unsupported.
    reconnector: Option<ReconnectServer>,
    /// A reload built before the process's post-offer load was due.
    load_after_reload: bool,
    /// Switches admitted during a turn, applied at the next turn boundary.
    pending: Vec<Prepared>,
    /// The session's own thinking choice.
    chosen: Option<contract::ThinkingLevel>,
    /// Puts a wake in the inbox; `None` leaves tool interactions unanswerable.
    inbox_wake: Option<Arc<dyn contract::clock::Wake>>,
    /// Builds the `tool_search` tool when the model defers; `None` defers
    /// nothing (`docs/tools.md`, "Tool search").
    tool_search: Option<ToolSearchFactory>,
    /// The factory-inserted search tool's name, while the registry holds
    /// it: the next build drops it before rebuilding, so a rebuild never
    /// takes its own insert for a name the general rule already holds
    /// (`docs/tools.md`, "Tool search").
    inserted_search: Option<String>,
    /// Publishes `/tools` rows; `None` publishes nothing.
    tool_rows: Option<ToolRows>,
    /// Refreshes the session's `commands` rows after the turn-start
    /// check; `None` refreshes nothing.
    commands_refresh: Option<CommandsRefresh>,
    /// The admission the door and the loop share: once it closes, no session
    /// message enters the inbox (`docs/tools.md`, "Messaging other sessions").
    admission: Option<Arc<support::admission::Admission>>,
    /// The turn's collected input holds a session message while the offer
    /// step resolves, so the session is not idle meanwhile.
    message_waiting: bool,
    /// The loop thread's hook around the last idle check, in tests only.
    #[cfg(test)]
    window: Option<Box<dyn FnMut(crate::inbox::session_message::Window) + Send>>,
}

/// The one built preamble: what every request sends and what
/// `preamble_built` records (`docs/prompt-cache.md`, "The preamble").
#[derive(Debug, Clone)]
struct Preamble {
    /// The system prompt text as sent.
    system_prompt: String,
    /// The tools as sent, in name order.
    tools: Vec<ToolDefinition>,
    /// The effective deferred names the preamble sends: `None` on a model
    /// without deferral, `Some(D)` otherwise, with D empty when nothing
    /// defers (`docs/tools.md`, "Tool search").
    deferred: Option<BTreeSet<String>>,
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

/// What every new loop runs on: the log, the provider and model, the
/// prompt inputs, the inbox, the tools and the permissions.
pub struct Session {
    /// The session's log.
    pub log: Arc<Log>,
    /// The provider answering the session's calls.
    pub provider: Arc<dyn Provider>,
    /// The model the session runs, and how its calls are priced.
    pub model: Model,
    /// What the one preamble build reads, once.
    pub prompt: PromptInputs,
    /// The session's inbox, which the loop blocks on while idle.
    pub inbox: Receiver<Delivery>,
    /// Registered by name, each with who registered it; a later one replaces
    /// an earlier one of the same name (`docs/architecture.md`, "Tool seam").
    pub tools: Vec<(String, Arc<dyn Tool>)>,
    /// The workspace, the credentials and the standing rules.
    pub permissions: Permissions,
}

/// What a new loop starts from that only a resume folds: the preamble
/// reason, the session and root ids, the folded state, and the registered
/// tools with the workspace and credentials the loop runs on.
struct Seed {
    reason: PreambleReason,
    session: String,
    root: String,
    suspended: Option<resume::Suspended>,
    held: Vec<Input>,
    conversation: Vec<Input>,
    sent: Option<usize>,
    grants: Vec<Grant>,
    known_hosts: known_hosts::KnownHosts,
    reviewed: Vec<reviewer::Reviewed>,
    session_blocks: u64,
    repository: offer::State,
    naming: naming::Naming,
    fired: quota_notice::Fired,
    context: Option<Vec<contract::Envelope>>,
    ledger: usage::Ledger,
    summary: Summary,
    carry: handoff::Carry,
    chosen: Option<contract::ThinkingLevel>,
    tools: BTreeMap<String, calls::Registered>,
    replaced: Vec<ToolReplaced>,
    workspace: PathBuf,
    credentials: PathBuf,
    mode: Mode,
}

/// Who drives a loop's session. A supervised session is always top-level,
/// so it is never also a delegate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A person's session, started or resumed.
    Plain,
    /// Driven by a program, so it never answers an approval.
    Supervised,
    /// A delegate's loop: its tree stop must preempt its failed-turn
    /// drain, so the session ends stopped.
    Delegate,
}

impl Mode {
    /// The mode a session's `session_started` describes: a parent makes
    /// a delegate, whatever `supervised` says.
    fn of(is_delegate: bool, supervised: bool) -> Self {
        if is_delegate {
            Self::Delegate
        } else if supervised {
            Self::Supervised
        } else {
            Self::Plain
        }
    }
}

/// The failure step 7 escalates without a reviewer, with the default limits.
fn no_model() -> Failure {
    Failure {
        code: ErrorCode::NoModel,
        message: NO_MODEL_MESSAGE.to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

impl Loop {
    /// Starts a new session's loop, writing `session_started` with no
    /// parent. `worktree` is the worktree the session runs in, when Fiber
    /// created one for it.
    pub fn start(session: Session, worktree: Option<Worktree>) -> Result<Self, Error> {
        Self::open(session, None, worktree, false)
    }

    /// A supervised session's loop: as [`Loop::start`], but the session
    /// is driven by a program, so `session_started` carries `supervised`
    /// and the loop never answers an approval (`docs/permissions.md`,
    /// "Supervised sessions").
    pub fn supervised(session: Session, worktree: Option<Worktree>) -> Result<Self, Error> {
        Self::open(session, None, worktree, true)
    }

    /// A Fiber delegate's loop: as [`Loop::start`], but the session's
    /// `session_started` names its parent (`docs/delegates.md`, "Events").
    /// A delegate never runs in a worktree of its own.
    pub fn delegate(session: Session, parent: Parent) -> Result<Self, Error> {
        Self::open(session, Some(parent), None, false)
    }

    /// [`Loop::start`], [`Loop::supervised`] and [`Loop::delegate`]
    /// through one writer: the session's `session_started` carries
    /// `parent` (`None` for a plain start), `supervised` and `worktree`
    /// (`None` for a delegate).
    fn open(
        mut session: Session,
        parent: Option<Parent>,
        worktree: Option<Worktree>,
        supervised: bool,
    ) -> Result<Self, Error> {
        // Registered before `session_started`: a `Tool::definition()` may
        // read the log.
        let (tools, replaced) = calls::register(std::mem::take(&mut session.tools));
        let workspace = PathBuf::from(&session.permissions.workspace);
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        let credentials = permission::resolved(session.permissions.credentials.clone());
        let mode = Mode::of(parent.is_some(), supervised);
        let started = session.log.append(
            &Event::SessionStarted(SessionStarted {
                workspace: session.permissions.workspace.clone(),
                variables: variables(),
                parent,
                forked_from: None,
                rewind: None,
                worktree,
                supervised: mode == Mode::Supervised,
            }),
            None,
            None,
        )?;
        Self::new(
            session,
            Seed {
                reason: PreambleReason::Start,
                // A new session is its own root (`docs/prompt-cache.md`,
                // "Cache markers and keys").
                session: started.session_id.0.clone(),
                root: started.session_id.0.clone(),
                suspended: None,
                held: Vec::new(),
                conversation: Vec::new(),
                sent: None,
                grants: Vec::new(),
                known_hosts: known_hosts::KnownHosts::default(),
                reviewed: Vec::new(),
                session_blocks: 0,
                repository: offer::State::default(),
                naming: naming::Naming::default(),
                fired: quota_notice::Fired::default(),
                context: None,
                ledger: usage::Ledger::default(),
                summary: Summary::default(),
                carry: handoff::Carry::default(),
                chosen: None,
                tools,
                replaced,
                workspace,
                credentials,
                mode,
            },
        )
    }

    /// The one constructor: every entry builds its [`Session`] and `Seed`
    /// and builds through here, so the struct literal is written once.
    /// Writes nothing to the session log.
    fn new(session: Session, seed: Seed) -> Result<Self, Error> {
        let Session {
            log,
            provider,
            model,
            prompt,
            inbox,
            permissions,
            ..
        } = session;
        let Seed {
            reason,
            session: session_id,
            root,
            suspended,
            held,
            conversation,
            sent,
            grants,
            known_hosts,
            reviewed,
            session_blocks,
            repository,
            naming,
            fired,
            context,
            ledger,
            summary,
            carry,
            chosen,
            tools,
            replaced,
            workspace,
            credentials,
            mode,
        } = seed;
        // The tracked state the log's lines describe, when the log holds
        // an opening message; without one the first turn writes it fresh
        // and rebuilds the state from it. Only the current context's
        // lines: a completed handoff starts the tracked state afresh, as
        // it does live.
        let skills = skill_set::SkillSet::new(prompt.clone(), &workspace);
        let changes = match &context {
            Some(context) => {
                skills.resumed(context)?;
                changes::State::resumed(context, &workspace, &prompt)?
            }
            None => changes::State::empty(&prompt.home),
        };
        // `session_started` renders nothing into the conversation.
        let diag = diag::SessionDiag::new(
            &prompt.home,
            contract::SessionId(session_id.clone()),
            Arc::clone(log.clock()),
        );
        let spend_trip = std::sync::Arc::new(std::sync::Mutex::new(usage::Tripwire::from_ledger(
            &ledger, None,
        )));
        Ok(Self {
            status_tap: None,
            log,
            diag,
            provider,
            media: media::Cache::default(),
            model,
            observe: None,
            reviewer_observe: None,
            prompt,
            skills,
            preamble_reason: reason,
            preamble: None,
            inbox: inbox::queue::Queue::new(inbox),
            cancel: Arc::new(TurnCancel::default()),
            cache_key: root,
            reviewer_key: format!("{session_id}:reviewer"),
            notes_key: format!("{session_id}:notes"),
            queued: VecDeque::new(),
            steering: Arc::new(steering::Queue::new()),
            call_turns: BTreeMap::new(),
            closing: false,
            rewound: false,
            suspended,
            deferred: VecDeque::new(),
            held,
            conversation,
            sent,
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
            grants,
            known_hosts,
            reviewer: Err(no_model()),
            limits: BlockLimits::default(),
            reviewer_notes: String::new(),
            reviewed,
            reviewer_sent: None,
            decision_buffer: None,
            consecutive: 0,
            session_blocks,
            no_model_noticed: false,
            turn_blocked: None,
            workspace_label: permissions.workspace,
            // A supervised session is driven by a program: nothing in
            // its life re-enables answering (`docs/permissions.md`,
            // "Supervised sessions").
            answerable: mode != Mode::Supervised,
            server_prompts: None,
            repository,
            naming,
            opened: context.is_some(),
            changes,
            cut_off: false,
            summary,
            ledger,
            spend_trip,
            late_cost: late_cost::LateCost::default(),
            lifecycle: lifecycle::State::new(reason, &contract::SessionId(session_id.clone())),
            budget: None,
            is_delegate: mode == Mode::Delegate,
            retry: Retry::default(),
            idle_exit: None,
            idle_left: false,
            hooks: None,
            release: None,
            on_rewrite: None,
            handoff: handoff::State::new(carry),
            ending: jobs::Ending::default(),
            quota_notices: quota_notice::Notices::new(fired),
            warming: warm::Warming::default(),
            switcher: None,
            reloader: None,
            reconnector: None,
            load_after_reload: false,
            pending: Vec::new(),
            chosen,
            inbox_wake: None,
            tool_search: None,
            inserted_search: None,
            tool_rows: None,
            commands_refresh: None,
            admission: None,
            message_waiting: false,
            #[cfg(test)]
            window: None,
        })
    }

    /// Shares the door's steering queue: a reader admits into it at once,
    /// and this loop reconciles each delivery against it at the drain
    /// (`docs/architecture.md`, "One inbox"). Without it the loop
    /// shares nothing, exactly as unwired admission does today.
    pub fn steering(mut self, queue: Arc<steering::Queue>) -> Self {
        self.steering = queue;
        self
    }

    /// The signal that cancels this loop's running turn
    /// (`docs/architecture.md`, "Cancellation"). Without it the loop
    /// arms a signal nobody cancels, so an unwired loop still works.
    pub fn cancelled_by(mut self, cancel: Arc<TurnCancel>) -> Self {
        self.cancel = cancel;
        self
    }

    /// Caps billed spend at `usd` US dollars. `None` is no limit
    /// (`docs/loop.md`, "Spending budget"). Publishes the budget to the
    /// spend tripwire under its lock; the trip itself fires on spend
    /// arrivals and at the request-time budget end, never here.
    pub fn budget(mut self, usd: Option<f64>) -> Self {
        self.budget = usd;
        lock(&self.spend_trip).set_budget(usd);
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

    /// Observes the session's model calls: the turn's call with its retry
    /// attempt, and a warming refresh with the first attempt. `None`
    /// calls the provider directly.
    pub fn observed(mut self, observe: Option<contract::diag::Observe>) -> Self {
        self.observe = observe;
        self
    }

    /// Observes the reviewer's calls at the first attempt. `None` calls
    /// the reviewer's provider directly.
    pub fn reviewer_observed(mut self, observe: Option<contract::diag::Observe>) -> Self {
        self.reviewer_observe = observe;
        self
    }

    /// Builds the `tool_search` tool the preamble declares when the model
    /// defers (`docs/tools.md`, "Tool search"). Default `None`, which
    /// defers nothing.
    pub fn tool_search(mut self, factory: ToolSearchFactory) -> Self {
        self.tool_search = Some(factory);
        self
    }

    /// Publishes `/tools` rows as tool states move (`docs/tools.md`,
    /// "Seeing the tools"). Default `None`, which publishes nothing.
    pub fn tool_rows(mut self, rows: ToolRows) -> Self {
        self.tool_rows = Some(rows);
        self
    }

    /// Refreshes the session's `commands` rows after the turn-start check
    /// changed the runnable set. Default `None`, which refreshes nothing.
    pub fn commands_refresh(mut self, refresh: CommandsRefresh) -> Self {
        self.commands_refresh = Some(refresh);
        self
    }

    /// Shares the session's attached diagnostic writer: `extension_log`,
    /// request and memory lines go to one file. The caller attaches after
    /// its durable log exists and installs the factories first, so every
    /// line shares the writer's identity.
    pub fn diagnostics(mut self, diag: Arc<log::diag::Diag>) -> Self {
        self.diag = diag::SessionDiag::shared(diag);
        self
    }
}
