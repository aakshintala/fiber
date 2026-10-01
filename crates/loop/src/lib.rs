//! Runs turns and steps (`docs/loop.md`): the only thing that decides what
//! happens next, and the only writer of durable events
//! (`docs/architecture.md`, "The threads").
//!
//! A [`Loop`] runs on the session's loop thread. It blocks on its inbox while
//! idle, starts a turn from everything waiting there, and streams each model
//! reply on its own thread (`docs/architecture.md`, "One inbox" and
//! "Streaming").

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use contract::events::{
    AssistantMessageCompleted, CacheLifetime, CallStatus, Empty, Event, InputItem, MessageOutcome,
    SessionStarted, SteeringApplied, ToolCallCompleted, TurnCompleted, TurnOutcome, TurnStarted,
    UsageRecorded,
};
use contract::provider::{CallError, Delta, Input, ModelRequest, Provider, Reply, ReplyAction};
use contract::shapes::{ContentPart, Failure, Sender};
use contract::{ActionId, ErrorCode, TurnId};
use log::Log;

/// A message for the loop: a driver's, an extension's or another session's.
/// While the loop is idle it starts a turn; while a turn runs it steers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The message.
    pub content: Vec<ContentPart>,
    /// Where it came from.
    pub sender: Sender,
}

/// What stops the loop.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The session log refused a write, so the turn cannot be recorded.
    #[error(transparent)]
    Log(#[from] log::Error),
}

impl Error {
    /// The stable code a consumer switches on (`docs/errors.md`,
    /// "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Log(e) => e.code(),
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
}

impl Loop {
    /// Starts a new session's loop, writing `session_started`.
    pub fn start(
        log: Arc<Log>,
        provider: Arc<dyn Provider>,
        model: String,
        system_prompt: String,
        inbox: Receiver<Message>,
        workspace: String,
    ) -> Result<Self, Error> {
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
        self.conversation.extend(input.iter().map(user));
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

    /// One step (`docs/loop.md`, "One step"), steps 1 to 4 and 7.
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
            self.conversation.push(user(&message));
        }
        let request = ModelRequest {
            system_prompt: self.system_prompt.clone(),
            tools: Vec::new(),
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
    /// ephemeral event under `message`. Readable reasoning opens a reasoning
    /// action at its first fragment, which is returned with the reply.
    fn stream(
        &self,
        request: &ModelRequest,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<(Result<Reply, CallError>, Option<ActionId>), Error> {
        let mut reasoning: Option<ActionId> = None;
        let mut failed = None;
        let call = self.provider.call(request);
        let reply = call.run(&mut |delta| {
            let written = match delta {
                Delta::Text(text) => {
                    self.append(&Event::AssistantMessageDelta(text), turn, Some(message))
                }
                Delta::ToolCallArguments(arguments) => self.append(
                    &Event::ToolCallArgumentsDelta(arguments),
                    turn,
                    Some(message),
                ),
                Delta::Reasoning(text) => {
                    let opened = match &reasoning {
                        Some(id) => Ok(id.clone()),
                        None => {
                            let id = ActionId(mint("a_"));
                            self.append(&Event::ReasoningStarted(Empty {}), turn, Some(&id))
                                .map(|()| id)
                        }
                    };
                    opened.and_then(|id| {
                        let written = self.append(&Event::ReasoningDelta(text), turn, Some(&id));
                        reasoning = Some(id);
                        written
                    })
                }
            };
            if let Err(e) = written {
                failed.get_or_insert(e);
            }
        });
        match failed {
            Some(e) => Err(e),
            None => Ok((reply, reasoning)),
        }
    }

    /// Writes a reply's actions, its usage and its completion, and adds them
    /// to the conversation. A tool call ends the step with a next one.
    fn record(
        &mut self,
        reply: Reply,
        mut streamed: Option<ActionId>,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<Step, Error> {
        let mut calls = Vec::new();
        let mut reasoning = Vec::new();
        for action in reply.actions {
            match action {
                ReplyAction::Reasoning(completed) => {
                    // ponytail: a reasoning fragment does not say which item
                    // it belongs to, so every streamed fragment goes under the
                    // first reasoning action; an item index on the delta if a
                    // provider streams two readable items in one reply.
                    let id = match streamed.take() {
                        Some(id) => id,
                        None => {
                            let id = ActionId(mint("a_"));
                            self.append(&Event::ReasoningStarted(Empty {}), turn, Some(&id))?;
                            id
                        }
                    };
                    self.append(
                        &Event::ReasoningCompleted(completed.clone()),
                        turn,
                        Some(&id),
                    )?;
                    reasoning.push(Input::Reasoning {
                        model: self.model.clone(),
                        text: completed.text,
                        provider_item: completed.provider_item,
                    });
                }
                ReplyAction::ToolCall(call) => {
                    let id = ActionId(mint("a_"));
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
                text: reply.text.clone(),
                error: None,
                attempt: None,
            }),
            turn,
            Some(message),
        )?;
        self.conversation.extend(reasoning);
        self.conversation
            .push(Input::Assistant { text: reply.text });
        if calls.is_empty() {
            return Ok(Step::Replied);
        }
        // No tool is registered, so every call fails `unknown_tool`
        // (`docs/loop.md`, "Tool calls that do not run").
        for (id, call) in calls {
            let text = format!(
                "No tool is named `{}`. Call only the tools you were given.",
                call.name
            );
            self.append(
                &Event::ToolCallCompleted(ToolCallCompleted {
                    status: CallStatus::Failed,
                    reason: None,
                    error: Some(Failure {
                        code: ErrorCode::UnknownTool,
                        message: text.clone(),
                        retry_after: None,
                        provider: None,
                    }),
                    process: None,
                    content: vec![ContentPart::Text { text: text.clone() }],
                    details: None,
                    artifact: None,
                    changes: None,
                    control: None,
                    changed_by: None,
                }),
                turn,
                Some(&id),
            )?;
            self.conversation.push(Input::ToolCall {
                action_id: id.clone(),
                call,
            });
            self.conversation.push(Input::ToolResult {
                action_id: id,
                text,
            });
        }
        Ok(Step::Next)
    }

    fn append(&self, event: &Event, turn: &TurnId, action: Option<&ActionId>) -> Result<(), Error> {
        self.log
            .append(event, Some(turn.clone()), action.cloned())?;
        Ok(())
    }
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

/// A message as the model reads it. Only text reaches it yet.
fn user(message: &Message) -> Input {
    let text = message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Input::User { text }
}

/// A new id from random bytes (`docs/events.md`, "Identity and ordering").
/// `RandomState` seeds its keys from the operating system's randomness.
fn mint(prefix: &str) -> String {
    format!("{prefix}{:016x}", RandomState::new().hash_one(()))
}
