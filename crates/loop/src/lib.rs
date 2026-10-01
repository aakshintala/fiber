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
    AssistantMessageCompleted, CacheLifetime, Class, Empty, Event, InputItem, MessageOutcome,
    SessionStarted, SteeringApplied, TurnCompleted, TurnOutcome, TurnStarted, UsageRecorded,
};
use contract::inbox::Message;
use contract::provider::{
    CallError, Delta, Finish, Input, ModelRequest, Provider, Reply, ReplyAction, ToolDefinition,
};
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{ActionId, ErrorCode, TurnId};
use log::Log;

mod calls;
mod conversation;
mod schema;

pub use conversation::rebuild;

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

/// One session's loop.
pub struct Loop {
    log: Arc<Log>,
    provider: Arc<dyn Provider>,
    /// The model reference, `provider/model`, that `provider` reaches.
    model: String,
    system_prompt: String,
    inbox: Receiver<Message>,
    /// The root session's id, for providers that route by key.
    cache_key: String,
    /// Taken from the inbox by the check before a turn completes, and
    /// applied by the next step.
    waiting: Option<Message>,
    /// The conversation, built from the durable events as they are written
    /// and never by re-reading the log (`docs/loop.md`, "What the model is
    /// sent").
    conversation: Vec<Input>,
    /// How much of `conversation` the previous request sent.
    sent: Option<usize>,
    /// The registered tools by name, each with its definition.
    tools: BTreeMap<String, (Arc<dyn Tool>, ToolDefinition)>,
    /// The workspace, symlinks resolved.
    workspace: PathBuf,
    /// The session directory, whose `artifacts/` holds cut results.
    session_dir: PathBuf,
    /// Whether this turn's previous reply was cut off by the output limit.
    cut_off: bool,
}

impl Loop {
    /// Starts a new session's loop, writing `session_started`. `tools` are
    /// registered by name; a later one replaces an earlier one of the same
    /// name (`docs/architecture.md`, "Tool seam"). `session_dir` is the
    /// directory `log` writes to.
    #[allow(
        clippy::too_many_arguments,
        reason = "each is a distinct part of the session; a struct of them is the same list"
    )]
    pub fn start(
        log: Arc<Log>,
        provider: Arc<dyn Provider>,
        model: String,
        system_prompt: String,
        inbox: Receiver<Message>,
        workspace: String,
        session_dir: PathBuf,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Result<Self, Error> {
        let resolved = PathBuf::from(&workspace);
        let resolved = resolved.canonicalize().unwrap_or(resolved);
        let started = log.append(
            &Event::SessionStarted(SessionStarted {
                workspace,
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
            waiting: None,
            conversation: Vec::new(),
            sent: None,
            tools: tools
                .into_iter()
                .map(|tool| {
                    let definition = tool.definition();
                    (definition.name.clone(), (tool, definition))
                })
                .collect(),
            workspace: resolved,
            session_dir,
            cut_off: false,
        })
    }

    /// Runs turns until every sender of the inbox is gone.
    pub fn run(mut self) -> Result<(), Error> {
        while self.turn()?.is_some() {}
        Ok(())
    }

    /// Blocks until a message arrives, then runs one turn from everything
    /// waiting in the inbox, in arrival order (`docs/loop.md`, "Starting a
    /// turn"). Returns how the turn ended, or `None` once every sender of the
    /// inbox is gone.
    pub fn turn(&mut self) -> Result<Option<TurnOutcome>, Error> {
        let Ok(first) = self.inbox.recv() else {
            return Ok(None);
        };
        let input: Vec<Message> = std::iter::once(first)
            .chain(self.inbox.try_iter())
            .collect();
        let turn = TurnId(mint("t_"));
        self.cut_off = false;
        self.append(
            &Event::TurnStarted(TurnStarted {
                input: input
                    .iter()
                    .map(|m| InputItem::Message {
                        content: m.content.clone(),
                        sender: m.sender.clone(),
                        changed_by: None,
                    })
                    .collect(),
            }),
            &turn,
            None,
        )?;
        let completed = loop {
            match self.step(&turn)? {
                Step::Next => {}
                // Anything that arrived while the model wrote its final reply
                // continues the turn (`docs/loop.md`, "Ending a turn").
                Step::Replied => match self.inbox.try_recv() {
                    Ok(message) => self.waiting = Some(message),
                    Err(_) => break ended(TurnOutcome::Completed, None),
                },
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
        let steering: Vec<Message> = self
            .waiting
            .take()
            .into_iter()
            .chain(self.inbox.try_iter())
            .collect();
        for message in steering {
            self.append(
                &Event::SteeringApplied(SteeringApplied {
                    content: message.content.clone(),
                    sender: message.sender.clone(),
                    changed_by: None,
                }),
                turn,
                None,
            )?;
        }
        let request = ModelRequest {
            system_prompt: self.system_prompt.clone(),
            tools: self.tools.values().map(|(_, d)| d.clone()).collect(),
            effort: None,
            // ponytail: fixed until the preamble is built and logged
            // (`docs/prompt-cache.md`, "The preamble").
            tool_choice: "auto".to_owned(),
            cache_lifetime: CacheLifetime::OneHour,
            cache_key: self.cache_key.clone(),
            conversation: self.conversation.clone(),
            previous_end: self.sent.replace(self.conversation.len()),
        };
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
                        text: String::new(),
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
            write(log, conversation, model, event, turn, Some(action))
        };
        // ponytail: a reasoning fragment does not say which reasoning item it
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
        self.append(
            &Event::UsageRecorded(UsageRecorded {
                generation_id: reply.generation_id,
                model: self.model.clone(),
                tokens: reply.tokens,
                web_searches: None,
                // ponytail: no prices yet; the model's declared prices come
                // with the spending budget (`docs/model-routing.md`, "Cost").
                cost: None,
                subscription: None,
                extension: None,
                origin_session_id: None,
            }),
            turn,
            Some(message),
        )?;
        self.append(
            &Event::AssistantMessageCompleted(AssistantMessageCompleted {
                outcome: MessageOutcome::Completed,
                text: reply.text,
                error: None,
                attempt: None,
            }),
            turn,
            Some(message),
        )?;
        if reply.finish == Finish::OutputLimit {
            // None of a cut-off reply's calls runs (`docs/loop.md`, "A reply
            // cut off by the output limit").
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
            return Ok(Step::Next);
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
            &self.model,
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
fn mint(prefix: &str) -> String {
    format!("{prefix}{:016x}", RandomState::new().hash_one(()))
}
