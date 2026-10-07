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
use contract::events::{
    AssistantMessageCompleted, CacheLifetime, Empty, Event, Grant, MessageOutcome, PreambleReason,
    SessionStarted, ToolReplaced, TurnCompleted, TurnOutcome, TurnStarted, UsageRecorded,
};
use contract::inbox::Delivery;
use contract::provider::{
    CallError, Cost, Delta, Finish, Input, ModelRequest, Provider, Reply, ReplyAction,
    ToolDefinition,
};
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{ActionId, ErrorCode, TurnId};
use log::Log;
pub(crate) use util::{ended, mint, variables};

mod answer;
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
mod hooks;
mod hosted;
mod inbox;
mod jobs;
mod opening;
mod permission;
mod process;
mod progress;
mod prompt;
mod resume;
mod retry;
mod reviewer;
mod schema;
mod shutdown;
mod skill_header;
mod skills;
mod status;
mod switch;
mod usage;
mod util;
mod warm;

pub use cancel::TurnCancel;
pub use caps::{ResultCaps, capped};
pub use commands::commands;
pub use conversation::rebuild;
pub use error::Error;
pub use handoff::HandoffSettings;
pub use permission::Permissions;
pub use process::{Exited, extensions_loaded, fiber_exited, fiber_started, mcp_servers_started};
pub use prompt::PromptInputs;
pub use resume::{Resumed, resumed};
pub use retry::Retry;
pub use reviewer::{BlockLimits, NO_MODEL_MESSAGE, Reviewer};
pub use switch::{NO_SWITCH, Prepare, Prepared, Switchable};

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
}

/// The one built preamble: what every request sends and what
/// `preamble_built` records (`docs/prompt-cache.md`, "The preamble").
#[derive(Debug, Clone)]
struct Preamble {
    /// The system prompt text as sent.
    system_prompt: String,
    /// The tools as sent, in name order.
    tools: Vec<ToolDefinition>,
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
    /// same name (`docs/architecture.md`, "Tool seam").
    pub fn start(
        log: Arc<Log>,
        provider: Arc<dyn Provider>,
        model: Model,
        prompt: prompt::PromptInputs,
        inbox: Receiver<Delivery>,
        tools: Vec<(String, Arc<dyn Tool>)>,
        permissions: Permissions,
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
            reviewed: Vec::new(),
            reviewer_sent: None,
            consecutive: 0,
            session_blocks: 0,
            no_model_noticed: false,
            turn_blocked: None,
            workspace_label: permissions.workspace,
            answerable: true,
            opened: false,
            changes,
            cut_off: false,
            ledger: usage::Ledger::default(),
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

    /// Runs turns until `close` is taken or every sender of the inbox is
    /// gone, then, while jobs still run, the ending notice and their ends
    /// (`docs/invocation.md`, "Lifecycle"; `docs/tools.md`, "Background jobs").
    pub fn run(mut self) -> Result<(), Error> {
        let status = status::spawn(&self);
        let result = loop {
            match self.turn() {
                Ok(Some(_)) => {}
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        let result = self.settled(result);
        // `fiber_exited` is the last line a process writes: no status
        // follows it.
        if let Some(status) = status {
            status.stop();
        }
        result
    }

    /// Blocks until a prompt or a steer arrives, then runs one turn from
    /// everything waiting in the inbox, in arrival order (`docs/loop.md`,
    /// "Starting a turn"). Every prompt in that drain joins the turn.
    /// Returns how the turn ended, or `None` once `close` was taken while
    /// idle or every sender of the inbox is gone.
    pub fn turn(&mut self) -> Result<Option<TurnOutcome>, Error> {
        self.apply_switches()?;
        // A shutdown starts no turn and finishes none.
        if self.shutting_down() {
            return Ok(None);
        }
        if let Some(pending) = self.suspended.take() {
            return self.finish_suspended(pending);
        }
        let Some(started) = self.wait_for_turn()? else {
            return Ok(None);
        };
        // The one preamble build, before `turn_started`: `answerable` is
        // already what the builder set, so an unattended loop's prompt
        // carries its line. A loop that never takes a turn writes none.
        // The instruction files and the date are checked at each turn
        // start, before `turn_started` — never on the turn that wrote the
        // opening message, whose state was just built from it.
        if !self.ensure_preamble()? {
            self.check_changes()?;
        }
        let turn = TurnId(mint("t_"));
        self.cut_off = false;
        let input = self.turn_input(started.pieces);
        let person = input
            .iter()
            .any(|item| matches!(item, contract::events::InputItem::Handoff { .. }));
        self.handoff.new_turn(person);
        // Armed with `turn_started`: a shutdown lands before both or after.
        let cancel = Arc::clone(&self.cancel);
        let event = Event::TurnStarted(TurnStarted { input });
        let Some(written) = cancel.commit(cancel::Commit::Arm, || self.append(&event, &turn, None))
        else {
            return Ok(None);
        };
        written?;
        // Each prompt is accepted once its `turn_started` is written, in
        // arrival order. A log error or a shutdown above drops them uncalled.
        for ack in started.prompts {
            inbox::accept(ack);
        }
        self.run_steps(&turn)
    }

    /// Builds the one preamble and writes `preamble_built`, once per loop.
    /// Later turns reuse what the first turn built: between builds the
    /// preamble does not change (`docs/prompt-cache.md`, "The preamble").
    /// The opening message and its notices follow `preamble_built`, before
    /// `turn_started`.
    fn ensure_preamble(&mut self) -> Result<bool, Error> {
        if self.preamble.is_some() {
            return Ok(false);
        }
        let unattended = !self.answerable;
        self.set_trigger();
        let (system_prompt, tools, event) = prompt::build(
            &self.prompt,
            &self.model.reference,
            unattended,
            &self.tools,
            self.provider.as_ref(),
            self.preamble_reason,
            self.replaced.clone(),
            self.handoff.trigger_at,
        );
        self.log
            .append(&Event::PreambleBuilt(event.clone()), None, None)?;
        let large = prompt::definitions_notice(&event);
        self.preamble = Some(Preamble {
            system_prompt,
            tools,
            tool_choice: event.tool_choice,
            cache_lifetime: event.cache_lifetime,
            thinking: self.prompt.thinking,
        });
        let opened = self.ensure_opening()?;
        if let Some(notice) = large {
            self.append_early(&Event::Notice(notice))?;
        }
        Ok(opened)
    }

    /// Writes the opening message and its notices, once per session: after
    /// `preamble_built` and before `turn_started`
    /// (`docs/system-prompt.md`, "The opening message"). A resume over a
    /// log that already holds one writes none. The message goes through
    /// the same path as every durable event, so it renders into the
    /// conversation as its first `User` message, before the turn's input.
    fn ensure_opening(&mut self) -> Result<bool, Error> {
        if self.opened {
            return Ok(false);
        }
        self.opened = true;
        self.write_opening(None)?;
        Ok(true)
    }

    /// The turn-start instruction-file and date check
    /// (`docs/system-prompt.md`, "When something changes" and "The date"),
    /// before `turn_started`: one `instruction_file` per change in path
    /// order, then `date_changed`.
    fn check_changes(&mut self) -> Result<(), Error> {
        let out = self.changes.check(self.prompt.clock.as_ref());
        for event in out
            .files
            .into_iter()
            .map(Event::InstructionFile)
            .chain(out.notices.into_iter().map(Event::Notice))
        {
            self.append_early(&event)?;
        }
        if let Some(date) = out.date {
            self.append_early(&Event::DateChanged(date))?;
        }
        Ok(())
    }

    /// Writes `event` before its turn started, when there is no turn id yet.
    fn append_early(&mut self, event: &Event) -> Result<(), Error> {
        util::write(
            &self.log,
            &mut self.conversation,
            &mut self.reviewed,
            &self.model.reference,
            event,
            None,
            None,
            &mut self.changes.had,
            &mut self.handoff.carry,
        )
    }

    /// Writes the subdirectory lines the last step's calls queued: after
    /// the completed calls' results, before this step's request is built.
    fn flush_queued(&mut self, turn: &TurnId) -> Result<(), Error> {
        for event in self.changes.take_queued() {
            self.append(&event, turn, None)?;
        }
        Ok(())
    }

    /// One step (`docs/loop.md`, "One step").
    fn step(&mut self, turn: &TurnId) -> Result<Step, Error> {
        // A cancel that landed ends the turn before anything is sent: no
        // `step_started`, no request, and queued steers stay queued for
        // the next turn.
        let cancel = Arc::clone(&self.cancel);
        let step = cancel.commit(cancel::Commit::Step, || {
            self.append(&Event::StepStarted(Empty {}), turn, None)
        });
        let Some(written) = step else {
            return Ok(Step::Ended(ended(TurnOutcome::Interrupted, None)));
        };
        written?;
        // A jobs notice comes first, before the drain's `steering_queue`.
        self.write_pending(turn)?;
        // Queued first: a steer held during an approval, or taken by the
        // end-of-turn check, arrived before this drain.
        self.drain(turn)?;
        self.apply_steering(turn)?;
        self.flush_queued(turn)?;
        if let Some(completed) = self.check_context(turn)? {
            return Ok(Step::Ended(completed));
        }
        self.send(turn)
    }

    /// When billed spend has reached `budget.usd`, the turn fails and the
    /// request is not sent (`docs/loop.md`, "Spending budget").
    fn over_budget(&self) -> Option<TurnCompleted> {
        let limit = self.budget?;
        if self.ledger.spend() < limit {
            return None;
        }
        Some(ended(
            TurnOutcome::Failed,
            Some(Failure {
                code: ErrorCode::BudgetExceeded,
                message: format!(
                    "The session reached its spending budget of ${limit:.2} (budget.usd)."
                ),
                retry_after_ms: None,
                provider: None,
            }),
        ))
    }

    /// Sends `request` and streams the reply, emitting each fragment as an
    /// ephemeral event as it arrives: text and tool-call arguments under
    /// `message`, reasoning under its own action, opened with
    /// `reasoning_started` at its first fragment. Returns the reasoning
    /// actions opened, in order, with the reply.
    fn stream(
        &mut self,
        request: &ModelRequest,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<(Result<Reply, CallError>, VecDeque<ActionId>), Error> {
        let cancel = Arc::clone(&self.cancel);
        let Self {
            log,
            provider,
            model,
            conversation,
            reviewed,
            changes,
            handoff,
            ..
        } = self;
        let mut emit = |event: &Event, action: &ActionId| {
            util::write(
                log,
                conversation,
                reviewed,
                &model.reference,
                event,
                Some(turn),
                Some(action),
                &mut changes.had,
                &mut handoff.carry,
            )
        };
        // debt: a reasoning fragment does not say which reasoning item it
        // belongs to, so a run of reasoning fragments with nothing between is
        // taken as one action. Two readable items back to back would share an
        // id; exact once `Delta::Reasoning` carries the item's index, as
        // `tool_call_arguments_delta` does.
        let mut opened: VecDeque<ActionId> = VecDeque::new();
        let mut in_reasoning = false;
        let mut failed = None;
        let call = provider.call(request);
        let reply = cancel::run_cancellable(&cancel, call, &mut |delta| {
            let written = match delta {
                Delta::Text(text) => {
                    in_reasoning = false;
                    emit(&Event::AssistantMessageDelta(text), message)
                }
                Delta::ToolCallArguments(arguments) => {
                    in_reasoning = false;
                    emit(&Event::ToolCallArgumentsDelta(arguments), message)
                }
                Delta::Reasoning(text) => {
                    let id = match opened.back() {
                        Some(id) if in_reasoning => Ok(id.clone()),
                        _ => {
                            let id = ActionId(mint("a_"));
                            opened.push_back(id.clone());
                            emit(&Event::ReasoningStarted(Empty {}), &id).map(|()| id)
                        }
                    };
                    in_reasoning = true;
                    id.and_then(|id| emit(&Event::ReasoningDelta(text), &id))
                }
            };
            if let Err(e) = written {
                failed.get_or_insert(e);
            }
        });
        match failed {
            Some(e) => Err(e),
            None => Ok((reply, opened)),
        }
    }

    /// Writes a reply's actions, its usage and its completion, and returns
    /// its tool calls and how it finished. A reasoning action with readable
    /// text takes the next action `opened` while it streamed; any other opens
    /// now.
    pub(crate) fn write_reply(
        &mut self,
        reply: Reply,
        mut opened: VecDeque<ActionId>,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<(handoff::Calls, Finish), Error> {
        let mut calls = Vec::new();
        for action in reply.actions {
            match action {
                ReplyAction::Text(completed) => {
                    self.append(&Event::TextCompleted(completed), turn, Some(message))?;
                }
                ReplyAction::Reasoning(completed) => {
                    let streamed = if completed.text.is_empty() {
                        None
                    } else {
                        opened.pop_front()
                    };
                    let id = match streamed {
                        Some(id) => id,
                        None => {
                            let id = ActionId(mint("a_"));
                            self.append(&Event::ReasoningStarted(Empty {}), turn, Some(&id))?;
                            id
                        }
                    };
                    self.append(&Event::ReasoningCompleted(completed), turn, Some(&id))?;
                }
                ReplyAction::ToolCall(call) => {
                    let id = ActionId(mint("a_"));
                    let call = self.repaired(call);
                    self.append(&Event::ToolCallRequested(call.clone()), turn, Some(&id))?;
                    calls.push((id, call));
                }
                ReplyAction::Hosted(hosted) => self.write_hosted(&hosted, turn)?,
            }
        }
        let cost = usage::call_cost(reply.cost, self.model.cost.as_ref(), &reply.tokens);
        let recorded = UsageRecorded {
            generation_id: reply.generation_id,
            model: self.model.reference.clone(),
            tokens: reply.tokens,
            input_bytes: reply.input_size.bytes,
            input_media: reply.input_size.media.then_some(true),
            web_searches: reply.web_searches,
            cost,
            subscription: self.model.subscription.then_some(true),
            extension: None,
            origin_session_id: None,
        };
        self.append(&Event::UsageRecorded(recorded.clone()), turn, Some(message))?;
        self.ledger.record(&recorded);
        self.append(
            &Event::AssistantMessageCompleted(AssistantMessageCompleted {
                outcome: MessageOutcome::Completed,
                error: None,
            }),
            turn,
            Some(message),
        )?;
        Ok((calls, reply.finish))
    }

    /// Writes a reply and ends the step: a tool call ends it with a next
    /// one, once every call has completed.
    fn record(
        &mut self,
        reply: Reply,
        opened: VecDeque<ActionId>,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<Step, Error> {
        let tokens = reply.tokens.clone();
        let (calls, finish) = self.write_reply(reply, opened, turn, message)?;
        self.measure(&tokens);
        if finish == Finish::OutputLimit {
            // None of a cut-off reply's calls runs (`docs/loop.md`, "A reply
            // cut off by the output limit"). One with no call ends the step
            // as any reply with no call does ("Ending a turn").
            let called = !calls.is_empty();
            for (id, _) in calls {
                self.append(
                    &Event::ToolCallCompleted(completion::truncated()),
                    turn,
                    Some(&id),
                )?;
            }
            if std::mem::replace(&mut self.cut_off, true) {
                return Ok(Step::Ended(ended(
                    TurnOutcome::Failed,
                    Some(Failure {
                        code: ErrorCode::OutputTruncated,
                        message: "Two replies in a row reached the output limit.".to_owned(),
                        retry_after_ms: None,
                        provider: None,
                    }),
                )));
            }
            return Ok(if called { Step::Next } else { Step::Replied });
        }
        self.cut_off = false;
        if calls.is_empty() {
            return Ok(Step::Replied);
        }
        if self.run_calls(calls, turn)? || self.idle_left {
            // A cancel ended the step: the turn ends `interrupted` instead
            // of taking a next step. An idle approval writes nothing more.
            return Ok(Step::Ended(ended(TurnOutcome::Interrupted, None)));
        }
        if let Some(error) = self.turn_blocked.take() {
            // Headless, the block budget ran out: the step's calls
            // completed, and the turn ends `failed` with code `blocked`
            // (`docs/permissions.md`, "Headless").
            return Ok(Step::Ended(ended(TurnOutcome::Failed, Some(error))));
        }
        self.handoff_from_tools(turn)?;
        Ok(Step::Next)
    }
}
