//! Runs turns and steps (`docs/loop.md`): the only thing that decides what
//! happens next, and the only writer of durable events
//! (`docs/architecture.md`, "The threads").
//!
//! A [`Loop`] runs on the session's loop thread. It blocks on its inbox while
//! idle, starts a turn from everything waiting there, and streams each model
//! reply on its own thread (`docs/architecture.md`, "One inbox" and
//! "Streaming").

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, VecDeque};
use std::hash::BuildHasher;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use contract::events::{
    AssistantMessageCompleted, CacheLifetime, Class, Empty, Event, Grant, InputItem,
    MessageOutcome, SessionStarted, ToolReplaced, TurnCompleted, TurnOutcome, TurnStarted,
    UsageRecorded,
};
use contract::inbox::{Delivery, Message};
use contract::provider::{
    CallError, Cost, Delta, Finish, Input, ModelRequest, Provider, Reply, ReplyAction,
};
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{ActionId, ErrorCode, TurnId};
use log::Log;

mod calls;
mod conversation;
mod inbox;
mod permission;
mod process;
mod schema;
mod usage;

pub use conversation::rebuild;
pub use process::{fiber_exited, fiber_started};

/// What stops the loop.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The session log refused a write, so the turn cannot be recorded.
    #[error(transparent)]
    Log(#[from] log::Error),
    /// A durable line's payload does not read as its kind.
    #[error("a log line does not read as its kind: {0}")]
    Unreadable(serde_json::Error),
}

impl Error {
    /// The stable code a consumer switches on (`docs/errors.md`,
    /// "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Log(e) => e.code(),
            Self::Unreadable(_) => ErrorCode::LogCorrupt,
        }
    }
}

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

/// What a session's calls are judged against (`docs/permissions.md`, "The
/// order a call is judged in").
pub struct Permissions {
    /// The workspace, as `session_started` records it; fast paths resolve
    /// against it.
    pub workspace: String,
    /// Fiber home's `credentials/` directory.
    pub credentials: PathBuf,
    /// The standing rules.
    pub rules: Arc<dyn contract::rules::Rules>,
}

/// One session's loop.
pub struct Loop {
    log: Arc<Log>,
    provider: Arc<dyn Provider>,
    /// The model `provider` reaches, and how its calls are priced.
    model: Model,
    system_prompt: String,
    inbox: Receiver<Delivery>,
    /// The root session's id, for providers that route by key.
    cache_key: String,
    /// Steering messages taken and not yet applied, in arrival order: held
    /// while the loop waited on an approval, taken by the end-of-turn
    /// check, or taken in the drain that is about to apply them.
    queued: VecDeque<Message>,
    /// `close` has been taken. No further turn starts
    /// (`docs/invocation.md`, "Lifecycle").
    closing: bool,
    /// The conversation, built from the durable events as they are written
    /// and never by re-reading the log (`docs/loop.md`, "What the model is
    /// sent").
    conversation: Vec<Input>,
    /// How much of `conversation` the previous request sent.
    sent: Option<usize>,
    /// The registered tools by name.
    tools: BTreeMap<String, calls::Registered>,
    /// Each tool registered under a name already taken. #304 records it on
    /// `preamble_built`, with each tool's `registered_by`.
    #[expect(dead_code, reason = "read when #304 writes preamble_built")]
    replaced: Vec<ToolReplaced>,
    /// The workspace, symlinks resolved.
    workspace: PathBuf,
    /// Fiber home's `credentials/` directory, symlinks resolved.
    credentials: PathBuf,
    /// The standing rules, re-read every time a call reaches step 2.
    rules: Arc<dyn contract::rules::Rules>,
    /// The session grants this loop's answers added, in order: the fold of
    /// the `permission_resolved` lines this loop wrote.
    grants: Vec<Grant>,
    /// Whether a person can answer an approval; `false` for `fiber ask`.
    answerable: bool,
    /// Whether this turn's previous reply was cut off by the output limit.
    cut_off: bool,
    /// Usage lines this loop has written, latest per generation.
    ledger: usage::Ledger,
    /// `budget.usd` for this session. `None` is no limit.
    budget: Option<f64>,
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
        system_prompt: String,
        inbox: Receiver<Delivery>,
        tools: Vec<(String, Arc<dyn Tool>)>,
        permissions: Permissions,
    ) -> Result<Self, Error> {
        let (tools, replaced) = calls::register(tools);
        let workspace = PathBuf::from(&permissions.workspace);
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        let credentials =
            calls::resolve(&permissions.credentials).unwrap_or(permissions.credentials);
        let started = log.append(
            &Event::SessionStarted(SessionStarted {
                workspace: permissions.workspace,
                parent: None,
                forked_from: None,
                rewind: None,
            }),
            None,
            None,
        )?;
        // `session_started` renders nothing into the conversation.
        Ok(Self {
            log,
            provider,
            model,
            system_prompt,
            inbox,
            // A new session is its own root (`docs/prompt-cache.md`, "Cache
            // markers and keys").
            cache_key: started.session_id.0,
            queued: VecDeque::new(),
            closing: false,
            conversation: Vec::new(),
            sent: None,
            tools,
            replaced,
            workspace,
            credentials,
            rules: permissions.rules,
            grants: Vec::new(),
            answerable: true,
            cut_off: false,
            ledger: usage::Ledger::default(),
            budget: None,
        })
    }

    /// Caps billed spend at `usd` US dollars. `None` is no limit
    /// (`docs/loop.md`, "Spending budget").
    pub fn budget(mut self, usd: Option<f64>) -> Self {
        self.budget = usd;
        self
    }

    /// Whether a person can answer an approval; `false` for `fiber ask`
    /// (`docs/permissions.md`, "Headless"). Default `true`.
    pub fn answerable(mut self, yes: bool) -> Self {
        self.answerable = yes;
        self
    }

    /// Runs turns until `close` is taken or every sender of the inbox is
    /// gone (`docs/invocation.md`, "Lifecycle").
    pub fn run(mut self) -> Result<(), Error> {
        while self.turn()?.is_some() {}
        Ok(())
    }

    /// Blocks until a prompt or a steer arrives, then runs one turn from
    /// everything waiting in the inbox, in arrival order (`docs/loop.md`,
    /// "Starting a turn"). A later prompt in that drain is rejected `busy`.
    /// Returns how the turn ended, or `None` once `close` was taken while
    /// idle or every sender of the inbox is gone.
    pub fn turn(&mut self) -> Result<Option<TurnOutcome>, Error> {
        let Some(started) = self.wait_for_turn() else {
            return Ok(None);
        };
        let turn = TurnId(mint("t_"));
        self.cut_off = false;
        self.append(
            &Event::TurnStarted(TurnStarted {
                input: started
                    .messages
                    .into_iter()
                    .map(|message| InputItem::Message {
                        content: message.content,
                        sender: message.sender,
                        changed_by: None,
                    })
                    .collect(),
            }),
            &turn,
            None,
        )?;
        // A prompt is accepted once its `turn_started` is written. A log
        // error above drops it uncalled.
        if let Some(ack) = started.prompt {
            inbox::accept(ack);
        }
        let completed = loop {
            match self.step(&turn)? {
                Step::Next => {}
                // Anything waiting continues the turn (`docs/loop.md`,
                // "Ending a turn"). A steer taken here waits for the next
                // step; a prompt is rejected.
                Step::Replied => {
                    self.drain();
                    if self.queued.is_empty() {
                        break ended(TurnOutcome::Completed, None);
                    }
                }
                Step::Ended(completed) => break completed,
            }
        };
        let outcome = completed.outcome;
        self.append(&Event::TurnCompleted(completed), &turn, None)?;
        Ok(Some(outcome))
    }

    /// One step (`docs/loop.md`, "One step").
    fn step(&mut self, turn: &TurnId) -> Result<Step, Error> {
        self.append(&Event::StepStarted(Empty {}), turn, None)?;
        // Queued first: a steer held during an approval, or taken by the
        // end-of-turn check, arrived before this drain.
        self.drain();
        self.apply_steering(turn)?;
        let request = ModelRequest {
            system_prompt: self.system_prompt.clone(),
            tools: self.tools.values().map(|(_, _, d)| d.clone()).collect(),
            effort: None,
            // debt: weakens docs/prompt-cache.md, "The preamble"; fixed by
            // #304. tool_choice is fixed at "auto" and no preamble_built is
            // logged.
            tool_choice: "auto".to_owned(),
            cache_lifetime: CacheLifetime::OneHour,
            cache_key: self.cache_key.clone(),
            conversation: self.conversation.clone(),
            previous_end: self.sent,
            max_output_tokens: None,
        };
        if let Some(completed) = self.over_budget() {
            return Ok(Step::Ended(completed));
        }
        // A refused request leaves the previous request's end in place, so a
        // later request still marks the cache where that request ended.
        self.sent = Some(self.conversation.len());
        let message = ActionId(mint("a_"));
        self.append(
            &Event::AssistantMessageStarted(Empty {}),
            turn,
            Some(&message),
        )?;
        let (reply, reasoning) = self.stream(&request, turn, &message)?;
        match reply {
            Ok(reply) => self.record(reply, reasoning, turn, &message),
            Err(CallError::Failed { failure, .. }) => {
                self.append(
                    &Event::AssistantMessageCompleted(AssistantMessageCompleted {
                        outcome: MessageOutcome::Failed,
                        error: Some(failure.clone()),
                        attempt: Some(1),
                    }),
                    turn,
                    Some(&message),
                )?;
                Ok(Step::Ended(ended(TurnOutcome::Failed, Some(failure))))
            }
            // An interrupted reply has no `assistant_message_completed`
            // (`docs/architecture.md`, "Cancellation").
            Err(CallError::Cancelled) => Ok(Step::Ended(ended(TurnOutcome::Interrupted, None))),
        }
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
                retry_after: None,
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
        let Self {
            log,
            provider,
            model,
            conversation,
            ..
        } = self;
        let mut emit = |event: &Event, action: &ActionId| {
            write(
                log,
                conversation,
                &model.reference,
                event,
                turn,
                Some(action),
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
        let reply = call.run(&mut |delta| {
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

    /// Writes a reply's actions, its usage and its completion. A reasoning
    /// action with readable text takes the next action `opened` while it
    /// streamed; any other opens now. A tool call ends the step with a next
    /// one, once every call has completed.
    fn record(
        &mut self,
        reply: Reply,
        mut opened: VecDeque<ActionId>,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<Step, Error> {
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
            }
        }
        let cost = self
            .model
            .cost
            .as_ref()
            .map(|prices| usage::price(prices, &reply.tokens));
        let recorded = UsageRecorded {
            generation_id: reply.generation_id,
            model: self.model.reference.clone(),
            tokens: reply.tokens,
            web_searches: None,
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
                attempt: None,
            }),
            turn,
            Some(message),
        )?;
        if reply.finish == Finish::OutputLimit {
            // None of a cut-off reply's calls runs (`docs/loop.md`, "A reply
            // cut off by the output limit"). One with no call ends the step
            // as any reply with no call does ("Ending a turn").
            let called = !calls.is_empty();
            for (id, _) in calls {
                self.append(
                    &Event::ToolCallCompleted(calls::truncated()),
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
                        retry_after: None,
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
        self.run_calls(calls, turn)?;
        Ok(Step::Next)
    }

    /// Writes `event` and renders it into the conversation.
    fn append(
        &mut self,
        event: &Event,
        turn: &TurnId,
        action: Option<&ActionId>,
    ) -> Result<(), Error> {
        write(
            &self.log,
            &mut self.conversation,
            &self.model.reference,
            event,
            turn,
            action,
        )
    }
}

/// Writes `event` to `log` and renders it into `conversation`: the one path
/// every event the loop emits takes, so the conversation is the log's
/// rendering (`docs/loop.md`, "What the model is sent").
fn write(
    log: &Log,
    conversation: &mut Vec<Input>,
    model: &str,
    event: &Event,
    turn: &TurnId,
    action: Option<&ActionId>,
) -> Result<(), Error> {
    log.append(event, Some(turn.clone()), action.cloned())?;
    if event.class() == Class::Durable {
        conversation::render(conversation, event, action, model);
    }
    Ok(())
}

/// How a step ended.
enum Step {
    /// The reply called tools: take the next step.
    Next,
    /// The reply called no tool: the turn completes unless something is
    /// waiting.
    Replied,
    /// The turn ended.
    Ended(TurnCompleted),
}

/// `turn_completed` with `outcome`, and `error` on `failed`.
fn ended(outcome: TurnOutcome, error: Option<Failure>) -> TurnCompleted {
    TurnCompleted {
        outcome,
        error,
        questions: None,
    }
}

/// A new id from random bytes (`docs/events.md`, "Identity and ordering").
/// `RandomState` seeds its keys from the operating system's randomness.
pub(crate) fn mint(prefix: &str) -> String {
    format!("{prefix}{:016x}", RandomState::new().hash_one(()))
}
