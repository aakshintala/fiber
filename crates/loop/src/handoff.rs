//! Handoff (`docs/handoff.md`): the context's restart from a note the
//! session's own model writes. This module holds the settings, the context
//! size and the automatic check, the note request, the recording, and the
//! render state that makes the live conversation and a resume's rebuild
//! agree.

use std::sync::Arc;

use contract::events::{
    ContextNudged, Event, HandoffCompleted, HandoffStarted, HandoffTrigger, Note, Outcome,
    ToolCallRequested, TurnCompleted, TurnOutcome,
};
use contract::provider::{Finish, Input, ModelRequest, Reply, ReplyAction};
use contract::shapes::{Failure, Tokens};
use contract::{ActionId, ErrorCode, TurnId};

use crate::prompt::{body, fill};
use crate::retry::Attempted;
use crate::{Error, Loop, Step};

/// How automatic handoff is set (`docs/configuration.md`, `handoff.*`).
#[derive(Debug, Clone, PartialEq)]
pub struct HandoffSettings {
    /// `handoff.enabled`: whether automatic handoff runs.
    pub enabled: bool,
    /// `handoff.tokens`: the token trigger.
    pub tokens: u64,
    /// `handoff.window_fraction`: the trigger as a fraction of the model's
    /// context window.
    pub window_fraction: f64,
    /// `handoff.nudge`: whether the nudge is given.
    pub nudge: bool,
}

impl Default for HandoffSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            tokens: 400_000,
            window_fraction: 0.7,
            nudge: true,
        }
    }
}

/// The context size, in tokens, at which an automatic handoff runs: the
/// token trigger, or the window fraction when that comes first. `None` when
/// automatic handoff is off; the token trigger alone when the window is
/// unknown, `0`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a window is far below 2^53 tokens, and the product is floored and non-negative"
)]
pub(crate) fn trigger_at(settings: &HandoffSettings, window: u64) -> Option<u64> {
    if !settings.enabled {
        return None;
    }
    if window == 0 {
        return Some(settings.tokens);
    }
    let by_window = (settings.window_fraction * window as f64).floor() as u64;
    Some(settings.tokens.min(by_window))
}

/// The tokens a request held and its reply added: the prompt (`input`, the
/// cache reads and every cache write) plus the output.
pub(crate) fn context_tokens(tokens: &Tokens) -> u64 {
    let written: u64 = tokens.cache_write.values().sum();
    tokens
        .input
        .saturating_add(tokens.cache_read)
        .saturating_add(written)
        .saturating_add(tokens.output)
}

/// An estimate of the tokens `input` adds: `ceil(bytes / 4)` of its text; a
/// tool call is its name plus its arguments' JSON.
pub(crate) fn estimate(input: &Input) -> u64 {
    let bytes = match input {
        Input::User { text }
        | Input::Assistant { text, .. }
        | Input::Reasoning { text, .. }
        | Input::ToolResult { text, .. } => text.len(),
        Input::ToolCall { call, .. } => call.name.len() + call.arguments.to_string().len(),
    };
    u64::try_from(bytes).unwrap_or(u64::MAX).div_ceil(4)
}

/// The size of the last session-model reply, measured, and the conversation
/// length it was measured at.
#[derive(Debug, Clone, Copy)]
struct Measured {
    tokens: u64,
    at: usize,
}

/// What the loop keeps for handoff: the settings, the trigger the preamble
/// recorded, the context's measure and the per-turn block.
pub(crate) struct State {
    pub(crate) settings: HandoffSettings,
    /// `preamble_built.trigger_at`: what the check compares against.
    pub(crate) trigger_at: Option<u64>,
    /// Runs on every completed handoff (`docs/tools.md`, "File tools").
    forget: Option<Arc<dyn Fn() + Send + Sync>>,
    /// What the conversation's rendering carries across a handoff.
    pub(crate) carry: Carry,
    /// The last reply's size; `None` in a context no reply has measured.
    measured: Option<Measured>,
    /// A failed handoff blocks the automatic trigger for the rest of the
    /// turn.
    blocked: bool,
    /// The instructions of each person's `handoff` taken and not yet run, in
    /// arrival order: the next step boundary runs them as one handoff.
    pub(crate) held: Vec<Option<String>>,
    /// Whether this turn began as a person's `handoff` between turns.
    person_turn: bool,
    /// Whether a handoff of any trigger ran in this step: the overflow rule
    /// runs at most once per step, and a tool's handoff does not follow one.
    step_ran: bool,
}

impl State {
    /// A fresh state around what a rebuild carried.
    pub(crate) fn new(carry: Carry) -> Self {
        Self {
            settings: HandoffSettings::default(),
            trigger_at: None,
            forget: None,
            carry,
            measured: None,
            blocked: false,
            held: Vec::new(),
            person_turn: false,
            step_ran: false,
        }
    }

    /// A new turn lifts the block. `person` is whether the turn began as a
    /// person's `handoff` between turns.
    pub(crate) fn new_turn(&mut self, person: bool) {
        self.blocked = false;
        self.person_turn = person;
    }
}

/// The render state a handoff reads: the same fold in the live loop and in a
/// rebuild, so the two never differ (`docs/loop.md`, "What the model is
/// sent").
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Carry {
    /// The conversation's length at the open `handoff_started`.
    pub(crate) window: Option<usize>,
    /// This turn's input: its message items and the steering messages
    /// applied, in log order. A handoff carries it verbatim.
    pub(crate) input: Vec<Input>,
    /// The jobs started and not completed, in start order, as
    /// `(job id, description)`.
    pub(crate) jobs: Vec<(String, String)>,
    /// The session log's path, from the latest opening message.
    pub(crate) session_log: String,
    /// Whether this context was nudged.
    pub(crate) nudged: bool,
    /// The text parts the actions of a handoff window completed, by action:
    /// a completed handoff takes them; a window that ends otherwise leaves
    /// its few lines, which no later note names.
    pub(crate) texts: Vec<(ActionId, String)>,
    /// The calls of the last reply, in call order, with their results.
    pub(crate) step: Vec<StepCall>,
    /// The artifact of each tool result in this context that has one, by
    /// call, as a path relative to the session directory.
    pub(crate) artifacts: Vec<(ActionId, String)>,
}

/// One call of the last reply: the call, its result once written, and the
/// note its result set as `control.handoff`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StepCall {
    pub(crate) action: ActionId,
    pub(crate) call: Input,
    pub(crate) result: Option<Input>,
    pub(crate) note: Option<String>,
}

impl Carry {
    /// The conversation after a completed handoff: the carried input, the
    /// note, a line for the jobs still running and, when tools set the note,
    /// the other calls of that step with their results.
    pub(crate) fn restart(&mut self, done: &HandoffCompleted) -> Vec<Input> {
        let mut conversation = self.input.clone();
        let texts = std::mem::take(&mut self.texts);
        let step = std::mem::take(&mut self.step);
        let ids: &[ActionId] = match &done.note {
            Some(Note::Actions { note }) => note,
            Some(Note::Hook { .. }) | None => &[],
        };
        let note = match &done.note {
            Some(Note::Hook { note_text, .. }) => note_text.clone(),
            Some(Note::Actions { .. }) | None => ids
                .iter()
                .map(|id| {
                    let parts: Vec<&str> = texts
                        .iter()
                        .filter(|(action, _)| action == id)
                        .map(|(_, text)| text.as_str())
                        .collect();
                    if parts.is_empty() {
                        step.iter()
                            .find(|call| call.action == *id)
                            .and_then(|call| call.note.clone())
                            .unwrap_or_default()
                    } else {
                        parts.join("\n")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        };
        conversation.push(Input::User { text: note });
        if !self.jobs.is_empty() {
            let listed: Vec<String> = self
                .jobs
                .iter()
                .map(|(id, description)| format!("- {id}: {description}"))
                .collect();
            conversation.push(Input::User {
                text: fill(
                    &body(crate::conversation::MESSAGES_MD, "handoff-jobs"),
                    &[("jobs", listed.join("\n").as_str())],
                ),
            });
        }
        // The model had not seen the other calls of a step a tool ended: they
        // follow the note, the calls then their results, in call order. A
        // tool ended the step when a note action is one of its calls; a
        // note request's action is a reply, never a call.
        if step.iter().any(|call| ids.contains(&call.action)) {
            let others: Vec<&StepCall> = step
                .iter()
                .filter(|call| !ids.contains(&call.action))
                .collect();
            conversation.extend(others.iter().map(|call| call.call.clone()));
            conversation.extend(others.iter().filter_map(|call| call.result.clone()));
        }
        // Only a result still in this context can be moved out of it.
        self.artifacts.retain(|(id, _)| {
            conversation.iter().any(
                |input| matches!(input, Input::ToolResult { action_id, .. } if action_id == id),
            )
        });
        self.nudged = false;
        conversation
    }

    /// A call of the last reply was requested.
    pub(crate) fn call_requested(&mut self, action: &ActionId, call: &Input) {
        self.step.push(StepCall {
            action: action.clone(),
            call: call.clone(),
            result: None,
            note: None,
        });
    }

    /// A call of the last reply completed, with its result, its artifact and
    /// the note its `control.handoff` set, when it completed.
    pub(crate) fn call_completed(
        &mut self,
        action: &ActionId,
        result: &Input,
        artifact: Option<&str>,
        note: Option<&str>,
    ) {
        if let Some(path) = artifact {
            self.artifacts.push((action.clone(), path.to_owned()));
        }
        if let Some(call) = self.step.iter_mut().find(|call| call.action == *action) {
            call.result = Some(result.clone());
            call.note = note.map(str::to_owned);
        }
    }

    /// The calls of the last reply whose result set `control.handoff`, in
    /// call order.
    pub(crate) fn noted(&self) -> Vec<ActionId> {
        self.step
            .iter()
            .filter(|call| call.note.is_some())
            .map(|call| call.action.clone())
            .collect()
    }

    /// Closes a window left open: truncates `conversation` back to where
    /// the handoff began, so the note request's lines never stay.
    pub(crate) fn close_window(&mut self, conversation: &mut Vec<Input>) {
        if let Some(length) = self.window.take() {
            conversation.truncate(length);
        }
    }

    /// Tracks the jobs running: started adds, completed removes.
    pub(crate) fn fold_jobs(&mut self, event: &Event) {
        if let Event::JobStarted(job) = event {
            self.jobs
                .push((job.job_id.0.clone(), job.description.clone()));
        }
        if let Event::JobCompleted(job) = event {
            self.jobs.retain(|(id, _)| *id != job.job_id.0);
        }
    }

    /// The nudge's text, from its payload.
    pub(crate) fn nudge_text(&self, nudged: &ContextNudged) -> String {
        fill(
            &body(crate::conversation::MESSAGES_MD, "nudge"),
            &[
                ("tokens", nudged.tokens.to_string().as_str()),
                ("trigger_at", nudged.trigger_at.to_string().as_str()),
                ("session_log", self.session_log.as_str()),
            ],
        )
    }
}

/// The request for the note: `handoff-note`, then `handoff-focus` when there
/// are instructions.
pub(crate) fn note_request_text(session_log: &str, instructions: Option<&str>) -> String {
    let mut text = fill(
        &body(crate::conversation::MESSAGES_MD, "handoff-note"),
        &[("session_log", session_log)],
    );
    if let Some(instructions) = instructions {
        text.push_str("\n\n");
        text.push_str(&fill(
            &body(crate::conversation::MESSAGES_MD, "handoff-focus"),
            &[("instructions", instructions)],
        ));
    }
    text
}

/// The turn's end when a handoff was cancelled: a person cancelled the turn.
fn cancelled(outcome: Outcome) -> Option<TurnCompleted> {
    (outcome == Outcome::Cancelled).then(|| crate::ended(TurnOutcome::Interrupted, None))
}

/// What the note request came to.
enum Noted {
    /// The reply is the note; its message action carries it.
    Note(ActionId),
    /// The request or the reply failed.
    Failed(Failure),
    /// A person cancelled the turn.
    Cancelled,
}

fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.to_owned(),
        retry_after: None,
        provider: None,
    }
}

/// The note a reply holds: its text parts in order, joined with a newline.
fn reply_note(reply: &Reply) -> String {
    reply
        .actions
        .iter()
        .filter_map(|action| match action {
            ReplyAction::Text(part) => Some(part.text.as_str()),
            ReplyAction::Reasoning(_) | ReplyAction::ToolCall(_) | ReplyAction::Hosted(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Loop {
    /// Turns automatic handoff on or off and sets its triggers. Default
    /// [`HandoffSettings::default`].
    pub fn handoff(mut self, settings: HandoffSettings) -> Self {
        self.handoff.settings = settings;
        self
    }

    /// Runs `forget` on every completed handoff: what the model had seen of
    /// the files is no longer in its context.
    pub fn on_handoff(mut self, forget: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.handoff.forget = Some(forget);
        self
    }

    /// The request the preamble, the key and the previous request's end make
    /// around `conversation`. `None` before the preamble is built.
    pub(crate) fn request_for(&self, conversation: Vec<Input>) -> Option<ModelRequest> {
        let built = self.preamble.as_ref()?;
        Some(ModelRequest {
            system_prompt: built.system_prompt.clone(),
            tools: built.tools.clone(),
            thinking: built.thinking,
            tool_choice: built.tool_choice.clone(),
            cache_lifetime: built.cache_lifetime,
            cache_key: self.cache_key.clone(),
            conversation,
            previous_end: self.sent,
            max_output_tokens: None,
            session_dir: self.log.dir().to_path_buf(),
        })
    }

    /// Sets the trigger the preamble records and the check compares with.
    pub(crate) fn set_trigger(&mut self) {
        let window = self.prompt.context_window.unwrap_or(0);
        self.handoff.trigger_at = trigger_at(&self.handoff.settings, window);
    }

    /// Measures the context at a session-model reply: its tokens, and the
    /// conversation as it stands.
    pub(crate) fn measure(&mut self, tokens: &Tokens) {
        self.handoff.measured = Some(Measured {
            tokens: context_tokens(tokens),
            at: self.conversation.len(),
        });
    }

    /// The context's size: the last reply's tokens plus an estimate of what
    /// was rendered after it. `None` before any reply measured this context.
    fn context_size(&self) -> Option<u64> {
        let measured = self.handoff.measured?;
        let after: u64 = self
            .conversation
            .get(measured.at..)
            .unwrap_or_default()
            .iter()
            .map(estimate)
            .sum();
        Some(measured.tokens.saturating_add(after))
    }

    /// The check before each request (`docs/loop.md`, "One step"): hands off
    /// at the trigger, or gives the nudge once per context. Returns the
    /// turn's end when the handoff was cancelled.
    pub(crate) fn check_context(&mut self, turn: &TurnId) -> Result<Option<TurnCompleted>, Error> {
        self.handoff.step_ran = false;
        if !self.handoff.held.is_empty() {
            return self.run_person_handoff(turn);
        }
        let Some(trigger_at) = self.handoff.trigger_at else {
            return Ok(None);
        };
        let Some(size) = self.context_size() else {
            return Ok(None);
        };
        if size >= trigger_at && !self.handoff.blocked {
            return Ok(cancelled(self.run_handoff(
                turn,
                HandoffTrigger::Auto,
                None,
                size,
            )?));
        }
        if self.handoff.settings.nudge
            && !self.handoff.carry.nudged
            && size.saturating_mul(3) >= trigger_at.saturating_mul(2)
        {
            self.append(
                &Event::ContextNudged(ContextNudged {
                    tokens: size,
                    trigger_at,
                }),
                turn,
                None,
            )?;
        }
        Ok(None)
    }

    /// The handoff a person asked for, with the instructions of every
    /// command that arrived before this boundary, in arrival order. It takes
    /// the step's one handoff, so the automatic check does not also run. A
    /// turn that began as the command and holds no message from the person
    /// ends here, with no request after the note.
    fn run_person_handoff(&mut self, turn: &TurnId) -> Result<Option<TurnCompleted>, Error> {
        let parts: Vec<String> = std::mem::take(&mut self.handoff.held)
            .into_iter()
            .flatten()
            .filter(|instructions| !instructions.is_empty())
            .collect();
        let instructions = (!parts.is_empty()).then(|| parts.join("\n\n"));
        let size = self.context_estimate();
        let ended =
            cancelled(self.run_handoff(turn, HandoffTrigger::Person, instructions, size)?);
        if ended.is_none() && self.handoff.person_turn && self.handoff.carry.input.is_empty() {
            return Ok(Some(crate::ended(TurnOutcome::Completed, None)));
        }
        Ok(ended)
    }

    /// The handoff the last step's tools asked for, through
    /// `control.handoff`: no note request, because each call's argument is
    /// the note (`docs/handoff.md`, "A tool"). Runs once the step's calls
    /// have completed. At most one handoff runs per step: after an
    /// automatic or person's handoff in this step, it does nothing, and each
    /// call's result stays in the conversation as an ordinary result.
    pub(crate) fn handoff_from_tools(&mut self, turn: &TurnId) -> Result<(), Error> {
        if self.handoff.step_ran {
            return Ok(());
        }
        let note = self.handoff.carry.noted();
        if note.is_empty() {
            return Ok(());
        }
        let tokens_before = self.context_estimate();
        self.append(
            &Event::HandoffCompleted(HandoffCompleted {
                outcome: Outcome::Completed,
                error: None,
                note: Some(Note::Actions { note }),
                tokens_before,
                instructions: None,
            }),
            turn,
            None,
        )?;
        self.restarted(turn)
    }

    /// The size of the context now: measured, or estimated when no reply has
    /// measured this context yet.
    fn context_estimate(&self) -> u64 {
        self.context_size()
            .unwrap_or_else(|| self.conversation.iter().map(estimate).sum())
    }

    /// What a completed handoff leaves to do: the next request starts a new
    /// context, unmeasured and missing the cache for everything after the
    /// preamble, under a new opening message; the files seen are forgotten.
    fn restarted(&mut self, turn: &TurnId) -> Result<(), Error> {
        self.sent = None;
        self.handoff.measured = None;
        self.write_opening(Some(turn))?;
        if let Some(forget) = &self.handoff.forget {
            forget();
        }
        Ok(())
    }

    /// One handoff: `handoff_started`, the note request, then
    /// `handoff_completed` with how it ended (`docs/handoff.md`,
    /// "Recording"). A completed one writes the new opening message.
    fn run_handoff(
        &mut self,
        turn: &TurnId,
        trigger: HandoffTrigger,
        instructions: Option<String>,
        tokens_before: u64,
    ) -> Result<Outcome, Error> {
        self.handoff.step_ran = true;
        self.append(
            &Event::HandoffStarted(HandoffStarted { trigger }),
            turn,
            None,
        )?;
        let noted = self.request_note(turn, trigger, instructions.as_deref())?;
        let (outcome, error, note) = match noted {
            Noted::Note(id) => (
                Outcome::Completed,
                None,
                Some(Note::Actions { note: vec![id] }),
            ),
            Noted::Failed(failure) => (Outcome::Failed, Some(failure), None),
            Noted::Cancelled => (Outcome::Cancelled, None, None),
        };
        self.append(
            &Event::HandoffCompleted(HandoffCompleted {
                outcome,
                error,
                note,
                tokens_before,
                instructions,
            }),
            turn,
            None,
        )?;
        match outcome {
            Outcome::Completed => self.restarted(turn)?,
            Outcome::Failed => self.handoff.blocked = true,
            Outcome::Cancelled => {}
        }
        Ok(outcome)
    }

    /// Asks the model for its note: the conversation and one more user
    /// input, which is never logged. A cancel that landed before the call
    /// ends it at once, as it ends any call (`cancel::run_cancellable`).
    fn request_note(
        &mut self,
        turn: &TurnId,
        trigger: HandoffTrigger,
        instructions: Option<&str>,
    ) -> Result<Noted, Error> {
        let mut conversation = if trigger == HandoffTrigger::Overflow {
            self.moved_results()
        } else {
            self.conversation.clone()
        };
        conversation.push(Input::User {
            text: note_request_text(&self.handoff.carry.session_log, instructions),
        });
        let Some(request) = self.request_for(conversation) else {
            return Ok(Noted::Failed(failure(
                ErrorCode::LogCorrupt,
                "The preamble was not built.",
            )));
        };
        if let Some(over) = self.over_budget().and_then(|ended| ended.error) {
            return Ok(Noted::Failed(over));
        }
        self.sent = Some(self.conversation.len());
        match self.call_with_retries(&request, turn)? {
            Attempted::Replied {
                reply,
                reasoning,
                message,
            } => self.record_note(reply, reasoning, turn, message),
            Attempted::Failed(failed) => Ok(Noted::Failed(failed)),
            Attempted::Interrupted => Ok(Noted::Cancelled),
        }
    }

    /// Writes the note reply as any reply is written. A tool call in it
    /// never runs.
    fn record_note(
        &mut self,
        reply: Reply,
        reasoning: std::collections::VecDeque<ActionId>,
        turn: &TurnId,
        message: ActionId,
    ) -> Result<Noted, Error> {
        let note = reply_note(&reply);
        let (calls, finish) = self.write_reply(reply, reasoning, turn, &message)?;
        for (id, _) in calls {
            let completed = self.cancelled_before_ran();
            self.append(&Event::ToolCallCompleted(*completed), turn, Some(&id))?;
        }
        if finish == Finish::OutputLimit {
            return Ok(Noted::Failed(failure(
                ErrorCode::OutputTruncated,
                "The handoff note reached the output limit.",
            )));
        }
        if note.trim().is_empty() {
            return Ok(Noted::Failed(failure(
                ErrorCode::UnreadableReply,
                "The handoff note reply held no text.",
            )));
        }
        Ok(Noted::Note(message))
    }

    /// The conversation with the last step's tool results moved out: each
    /// result after the last reply input is replaced by a line naming its
    /// artifact, written here where the result has none. A result whose
    /// artifact cannot be written stays inline; the log holds it in full.
    fn moved_results(&self) -> Vec<Input> {
        let mut conversation = self.conversation.clone();
        // From the end back to the last reply input: what followed the reply.
        let after_reply = conversation.iter_mut().rev().take_while(|input| {
            !matches!(
                input,
                Input::Assistant { .. } | Input::Reasoning { .. } | Input::ToolCall { .. }
            )
        });
        for input in after_reply {
            if let Input::ToolResult {
                action_id, text, ..
            } = input
                && let Some(path) = self.artifact_of(action_id, text)
            {
                *text = fill(
                    &body(crate::conversation::MESSAGES_MD, "moved-result"),
                    &[("path", path.as_str())],
                );
            }
        }
        conversation
    }

    /// The path of the full text of a tool result: its artifact, or one
    /// written now, named after the call's action.
    fn artifact_of(&self, action: &ActionId, text: &str) -> Option<String> {
        let relative = match self
            .handoff
            .carry
            .artifacts
            .iter()
            .find(|(id, _)| id == action)
        {
            Some((_, path)) => path.clone(),
            None => {
                self.log
                    .write_artifact(&format!("{}.txt", action.0), text.as_bytes())
                    .ok()?
                    .0
            }
        };
        let session = std::path::Path::new(&self.handoff.carry.session_log).parent();
        Some(session.map_or(relative.clone(), |dir| {
            dir.join(&relative).display().to_string()
        }))
    }

    /// The overflow rule (`docs/handoff.md`, "Overflow"): a request that
    /// does not fit, or that the provider rejected for size as `failed`,
    /// hands off with the last step's tool results moved out, then sends the
    /// step's request from the new context. Once per step; with automatic
    /// handoff off, or when the handoff does not complete, the turn fails
    /// `context_overflow`.
    pub(crate) fn overflowed(
        &mut self,
        turn: &TurnId,
        failed: Option<Failure>,
    ) -> Result<Step, Error> {
        let over = failed.unwrap_or_else(|| {
            failure(
                ErrorCode::ContextOverflow,
                "The request does not fit the model's context window.",
            )
        });
        if !self.handoff.settings.enabled || self.handoff.step_ran {
            return Ok(Step::Ended(crate::ended(TurnOutcome::Failed, Some(over))));
        }
        let size = self.context_estimate();
        match self.run_handoff(turn, HandoffTrigger::Overflow, None, size)? {
            Outcome::Completed => self.send(turn),
            Outcome::Failed => Ok(Step::Ended(crate::ended(TurnOutcome::Failed, Some(over)))),
            Outcome::Cancelled => Ok(Step::Ended(crate::ended(TurnOutcome::Interrupted, None))),
        }
    }

    /// Sends the step's request from the conversation as it stands, once the
    /// budget and the window allow it (`docs/loop.md`, "One step").
    pub(crate) fn send(&mut self, turn: &TurnId) -> Result<Step, Error> {
        let Some(request) = self.request_for(self.conversation.clone()) else {
            // `turn` builds the preamble before its `turn_started`; reaching
            // a step without one is a bug, so the turn fails closed.
            return Ok(Step::Ended(crate::ended(
                TurnOutcome::Failed,
                Some(failure(
                    ErrorCode::LogCorrupt,
                    "The preamble was not built.",
                )),
            )));
        };
        if let Some(completed) = self.over_budget() {
            return Ok(Step::Ended(completed));
        }
        let window = self.prompt.context_window.unwrap_or(0);
        if self.handoff.settings.enabled && window != 0 && self.context_estimate() > window {
            return self.overflowed(turn, None);
        }
        // A refused request leaves the previous request's end in place, so a
        // later request still marks the cache where that request ended.
        self.sent = Some(self.conversation.len());
        self.attempt(&request, turn)
    }

    /// Writes the opening message and its notices, and rebuilds the tracked
    /// instruction-file state from them.
    pub(crate) fn write_opening(&mut self, turn: Option<&TurnId>) -> Result<(), Error> {
        let collected = crate::opening::collect(&self.prompt, &self.workspace);
        self.changes =
            crate::changes::State::initial(&collected.message, &self.workspace, &self.prompt);
        for event in std::iter::once(Event::OpeningMessage(collected.message))
            .chain(collected.notices.into_iter().map(Event::Notice))
        {
            crate::util::write(
                &self.log,
                &mut self.conversation,
                &mut self.reviewed,
                &self.model.reference,
                &event,
                turn,
                None,
                &mut self.changes.had,
                &mut self.handoff.carry,
            )?;
        }
        Ok(())
    }
}

/// A reply's calls, as `write_reply` returns them.
pub(crate) type Calls = Vec<(ActionId, ToolCallRequested)>;

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
