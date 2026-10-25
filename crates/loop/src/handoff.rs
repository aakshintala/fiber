//! Handoff (`docs/handoff.md`): the context's restart from a note the
//! session's own model or a `before_handoff` hook writes. This module holds the settings, context size
//! and automatic check. Its child modules hold the note request, recording,
//! handoff run, and render state that makes the live conversation and a
//! resume's rebuild agree.

use std::path::Path;
use std::sync::Arc;

use contract::events::{
    ContextNudged, Event, HandoffTrigger, Notice, Outcome, SkillLoad, SkillSent, SkillsResent,
    ToolCallRequested, TurnCompleted, TurnOutcome,
};
use contract::provider::{Input, ModelRequest};
use contract::shapes::{DeclaredEffects, Effect, Failure, Tokens};
use contract::skills::{SkillRead, Skills};
use contract::tool::Effects;
use contract::{ActionId, ErrorCode, TurnId};

use crate::{Error, Loop, Step};

mod carry;
mod hook;
mod note;

#[cfg(test)]
mod hook_tests;

use note::cancelled;
#[cfg(test)]
pub(crate) use note::note_request_text;
pub(crate) use note::{failure, reply_note};

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
        Input::ToolSearchResult { loaded, .. } => contract::provider::loaded_text(loaded).len(),
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
    pub(crate) step_ran: bool,
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
    /// around `conversation`. `None` before the preamble is built. Each
    /// logged search renders against the preamble's deferred set
    /// (`docs/tools.md`, "Tool search"). The request's media is every image
    /// and PDF `conversation` names, read through `log` and encoded once per
    /// session: the provider does no file I/O.
    pub(crate) fn request_for(&self, conversation: Vec<Input>) -> Option<ModelRequest> {
        let built = self.preamble.as_ref()?;
        let conversation = crate::render_loaded::render(conversation, built.deferred.as_ref());
        let media = self.media.resolve(self.log.dir(), &conversation);
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
            conversation: conversation.into(),
            previous_end: self.sent,
            max_output_tokens: None,
            media,
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

    /// Runs one handoff and, when it completes, moves the loaded names the
    /// new context dropped back to `Deferred` (`docs/tools.md`,
    /// "Tool search").
    fn run_handoff_rows(
        &mut self,
        turn: &TurnId,
        source: note::Source,
        tokens_before: u64,
    ) -> Result<Outcome, Error> {
        let before = self.loaded_names();
        let outcome = self.run_handoff(turn, source, tokens_before)?;
        if outcome == Outcome::Completed {
            self.publish_handoff_rows(&before);
        }
        Ok(outcome)
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
            return Ok(cancelled(self.run_handoff_rows(
                turn,
                note::Source::Requested {
                    trigger: HandoffTrigger::Auto,
                    instructions: None,
                },
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
        let ended = cancelled(self.run_handoff_rows(
            turn,
            note::Source::Requested {
                trigger: HandoffTrigger::Person,
                instructions,
            },
            size,
        )?);
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
        let outcome = self.run_handoff_rows(turn, note::Source::Tools { note }, tokens_before)?;
        Ok(outcome == Outcome::Completed)
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
        match self.run_handoff_rows(
            turn,
            note::Source::Requested {
                trigger: HandoffTrigger::Overflow,
                instructions: None,
            },
            size,
        )? {
            Outcome::Completed => self.send(turn),
            Outcome::Failed => Ok(Step::Ended(crate::ended(TurnOutcome::Failed, Some(over)))),
            Outcome::Cancelled => Ok(Step::Ended(crate::ended(TurnOutcome::Interrupted, None))),
        }
    }

    /// Sends the step's request from the conversation as it stands, once the
    /// budget and the window allow it (`docs/loop.md`, "One step").
    pub(crate) fn send(&mut self, turn: &TurnId) -> Result<Step, Error> {
        // The scope covers assembling the history plus the request, never
        // the budget, window or hook checks after it. A missing preamble
        // only means no timer starts: assembly and the fail-closed path
        // run exactly as before.
        let has_preamble = self.preamble.is_some();
        let _timer = has_preamble
            .then(|| self.diag.timed(contract::diag::Op::TurnRequest))
            .flatten();
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
        #[cfg(test)]
        crate::timing_tests::request_send_exit(self.log.clock().as_ref());
        drop(_timer);
        if let Some(completed) = self.over_budget() {
            return Ok(Step::Ended(self.budget_end(completed)));
        }
        let window = self.prompt.context_window;
        if self.handoff.settings.enabled && self.context_estimate() > window {
            return self.overflowed(turn, None);
        }
        if let Some(completed) = self.model_call_allowed(turn)? {
            return Ok(Step::Ended(completed));
        }
        // A refused request leaves the previous request's end in place, so a
        // later request still marks the cache where that request ended.
        self.sent = Some(self.conversation.len());
        self.attempt(&request, turn)
    }

    /// Writes the opening message and its notices, and rebuilds the tracked
    /// instruction-file state from them. Refreshes the maintained skill
    /// set with what the message sent, so `/name` and the `skill` tool
    /// answer from the listing the model was given. Before the opening, a
    /// completed handoff's loads are sent again as `skills_resent`, read
    /// fresh from disk (`docs/handoff.md`, "What the model sees after a
    /// handoff").
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
        let (refresh_commands, commands_changed) = self.skills.opened(
            found.clone(),
            inputs.skills_disabled.clone(),
            &message.skills,
            &notices,
        );
        let command_notices = if refresh_commands {
            let notices = self.refresh_commands(&inputs, &found);
            if commands_changed {
                notices
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        // The previous context's loads, armed by a completed handoff: the
        // set is now the new listing, so what is sent and what is listed
        // follow one rule.
        let resend = std::mem::take(&mut self.handoff.carry.resend);
        let reader = self.skills.reader();
        let (sent, left_out) = resent_skills(&reader, &|target| self.refuse(target), &resend);
        self.changes = crate::changes::State::initial(&message, &self.workspace, &self.prompt);
        if !sent.is_empty() {
            self.write(
                &Event::SkillsResent(SkillsResent { skills: sent }),
                turn,
                None,
            )?;
        }
        for event in std::iter::once(Event::OpeningMessage(message))
            .chain(notices.into_iter().map(Event::Notice))
            .chain(command_notices.into_iter().map(Event::Notice))
        {
            self.write(&event, turn, None)?;
        }
        for notice in left_out {
            self.write(&Event::Notice(notice), turn, None)?;
        }
        Ok(())
    }

    /// Whether the deny steps refuse a resend read of `target`, with the
    /// verdict's reason: the credential deny and the fail-closed answer
    /// for unreadable standing rules apply to a resend exactly as to a
    /// call. The name is empty: a resend is not a call of any tool.
    fn refuse(&self, target: &Path) -> Option<String> {
        let effects = Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: Some(vec![target.display().to_string()]),
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        };
        match crate::permission::inner_verdict(
            "",
            &effects,
            &self.rules.read(),
            &self.workspace,
            &self.credentials,
            &self.credential_files,
        ) {
            crate::permission::Verdict::Deny { why, .. } => Some(why),
            crate::permission::Verdict::Ask(_)
            | crate::permission::Verdict::Allow(_)
            | crate::permission::Verdict::Review => None,
        }
    }
}

/// Reads each loaded skill again through the listing the new opening sent:
/// the sent entries, in first-load order, and one notice per skill left
/// out, in load order. A skill is sent only when the listing holds it now,
/// its target resolves to UTF-8, the deny steps allow the read and its
/// body reads (`docs/tools.md`, "Skills").
fn resent_skills(
    skills: &dyn Skills,
    refuse: &dyn Fn(&Path) -> Option<String>,
    loaded: &[SkillLoad],
) -> (Vec<SkillSent>, Vec<Notice>) {
    let mut sent = Vec::new();
    let mut left_out = Vec::new();
    for load in loaded {
        let Some(file) = skills.file(&load.name) else {
            left_out.push(resend_notice(
                &load.name,
                ErrorCode::InvalidArguments,
                "it is not in the skills listing now, or it is switched off.",
            ));
            continue;
        };
        let target = match std::fs::canonicalize(&file) {
            Ok(target) => target,
            Err(error) => {
                left_out.push(resend_notice(
                    &load.name,
                    ErrorCode::IoFailed,
                    &format!("Could not read skill {}: {error}.", file.display()),
                ));
                continue;
            }
        };
        if target.to_str().is_none() {
            left_out.push(resend_notice(
                &load.name,
                ErrorCode::InvalidArguments,
                "its path is not valid UTF-8.",
            ));
            continue;
        }
        if let Some(why) = refuse(&target) {
            left_out.push(resend_notice(&load.name, ErrorCode::InvalidArguments, &why));
            continue;
        }
        match skills.body(&load.name, &target) {
            Ok(content) => sent.push(SkillSent {
                name: load.name.clone(),
                path: file.to_string_lossy().into_owned(),
                content,
            }),
            Err(SkillRead::Io(message)) => {
                left_out.push(resend_notice(&load.name, ErrorCode::IoFailed, &message));
            }
            Err(SkillRead::Invalid) => {
                left_out.push(resend_notice(
                    &load.name,
                    ErrorCode::InvalidArguments,
                    "its header no longer parses, names another skill, or turns off model invocation.",
                ));
            }
        }
    }
    (sent, left_out)
}

/// One resend notice: the skill left out, and why it was not sent again.
fn resend_notice(name: &str, code: ErrorCode, why: &str) -> Notice {
    Notice {
        code,
        message: format!("Skill {name} was not sent again after the handoff: {why}"),
        extension: None,
    }
}

/// A reply's calls, as `write_reply` returns them.
pub(crate) type Calls = Vec<(ActionId, ToolCallRequested)>;

#[cfg(test)]
mod tests;
