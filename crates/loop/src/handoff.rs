//! Handoff (`docs/handoff.md`): the context's restart from a note the
//! session's own model writes. This module holds the settings, context size
//! and automatic check. Its child modules hold the note request, recording,
//! handoff run, and render state that makes the live conversation and a
//! resume's rebuild agree.

use std::sync::Arc;

use contract::events::{
    ContextNudged, Event, HandoffCompleted, HandoffTrigger, Note, Outcome, ToolCallRequested,
    TurnCompleted, TurnOutcome,
};
use contract::provider::{Input, ModelRequest};
use contract::shapes::{Failure, Tokens};
use contract::{ActionId, ErrorCode, TurnId};

use crate::{Error, Loop, Step};

mod carry;
mod note;

#[cfg(test)]
pub(crate) use note::note_request_text;
use note::{cancelled, failure};

pub(crate) use carry::Carry;

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
/// automatic handoff is off.
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
    let by_window = (settings.window_fraction * window as f64).floor() as u64;
    Some(settings.tokens.min(by_window))
}

/// The tokens a request held and its reply added: the prompt plus the
/// output.
pub(crate) fn context_tokens(tokens: &Tokens) -> u64 {
    prompt_tokens(tokens).saturating_add(tokens.output)
}

/// The tokens a request held: `input`, the cache reads and every cache
/// write. 0 for a call that reported no input, such as one that failed
/// before its provider named a generation.
pub(crate) fn prompt_tokens(tokens: &Tokens) -> u64 {
    let written: u64 = tokens.cache_write.values().sum();
    tokens
        .input
        .saturating_add(tokens.cache_read)
        .saturating_add(written)
}

/// An estimate of the tokens `input` adds: `ceil(bytes / 4)` of its text; a
/// tool call is its name plus its arguments' JSON.
pub(crate) fn estimate(input: &Input) -> u64 {
    let bytes = match input {
        Input::User { text, .. }
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
            // A rewound session's first request sends its parent's logged
            // build verbatim (`docs/events.md`, "Rewind").
            sent_tools: built.sent_tools.clone(),
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
        let window = self.prompt.context_window;
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
    /// call's result stays in the conversation as an ordinary result. Returns
    /// whether a handoff ran and restarted the context.
    pub(crate) fn handoff_from_tools(&mut self, turn: &TurnId) -> Result<bool, Error> {
        if self.handoff.step_ran {
            return Ok(false);
        }
        let note = self.handoff.carry.noted();
        if note.is_empty() {
            return Ok(false);
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
        self.restarted(turn)?;
        Ok(true)
    }

    /// The size of the context now: measured, or estimated when no reply has
    /// measured this context yet.
    fn context_estimate(&self) -> u64 {
        self.context_size()
            .unwrap_or_else(|| self.conversation.iter().map(estimate).sum())
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
            return Ok(Step::Ended(self.budget_end(completed)));
        }
        let window = self.prompt.context_window;
        if self.handoff.settings.enabled && self.context_estimate() > window {
            return self.overflowed(turn, None);
        }
        // A refused request leaves the previous request's end in place, so a
        // later request still marks the cache where that request ended.
        self.sent = Some(self.conversation.len());
        self.attempt(&request, turn)
    }

    /// Writes the opening message and its notices, and rebuilds the tracked
    /// instruction-file state from them. Refreshes the maintained skill
    /// set with what the message sent, so `/name` and the `skill` tool
    /// answer from the listing the model was given.
    pub(crate) fn write_opening(&mut self, turn: Option<&TurnId>) -> Result<(), Error> {
        // The loop's current inputs, with the maintained disabled list
        // applied: a model switch updates the window `collect` sizes its
        // notices by, and the set's snapshot would keep the old one
        // (`docs/system-prompt.md`, "Size").
        let inputs = self.skills.with_disabled(&self.prompt);
        let collected = crate::opening::collect(&inputs, &self.workspace);
        let crate::opening::Collected {
            message,
            notices,
            found,
        } = collected;
        self.skills.opened(found, inputs.skills_disabled.clone());
        self.changes = crate::changes::State::initial(&message, &self.workspace, &self.prompt);
        for event in std::iter::once(Event::OpeningMessage(message))
            .chain(notices.into_iter().map(Event::Notice))
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
mod tests;
