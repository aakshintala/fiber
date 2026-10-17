//! Owns the note request to the model, its recording, and the handoff run.

use std::sync::Arc;

use contract::events::{
    ContextAdded, Event, HandoffCompleted, HandoffStarted, HandoffTrigger, Note, Outcome,
    TurnCompleted, TurnOutcome,
};
use contract::provider::{Finish, Input, ModelRequest, Reply, ReplyAction};
use contract::shapes::Failure;
use contract::{ActionId, ErrorCode, TurnId};

use crate::lifecycle::Started;
use crate::prompt::{fill, message};
use crate::retry::Attempted;
use crate::{Error, Loop, cancel};

/// The request for the note: `handoff-note`, then `handoff-focus` when there
/// are instructions.
pub(crate) fn note_request_text(session_log: &str, instructions: Option<&str>) -> String {
    let mut text = fill(message("handoff-note"), &[("session_log", session_log)]);
    if let Some(instructions) = instructions {
        text.push_str("\n\n");
        text.push_str(&fill(
            message("handoff-focus"),
            &[("instructions", instructions)],
        ));
    }
    text
}

/// The turn's end when a handoff was cancelled: a person cancelled the turn.
pub(super) fn cancelled(outcome: Outcome) -> Option<TurnCompleted> {
    (outcome == Outcome::Cancelled).then(|| crate::ended(TurnOutcome::Interrupted, None))
}

/// What the note request came to.
pub(super) enum Noted {
    /// The reply is the note; its message action carries it.
    Note(ActionId),
    /// A `before_handoff` hook wrote the note instead; no note request
    /// was made.
    Hook {
        note_text: String,
        extension: String,
        state: Vec<contract::hook::StateLine>,
    },
    /// The request or the reply failed.
    Failed(Failure),
    /// A person cancelled the turn.
    Cancelled,
}

pub(crate) fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

/// The note a reply holds: its text parts in order, joined with a newline.
pub(crate) fn reply_note(reply: &Reply) -> String {
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

/// One handoff's completion from its note: `Completed` keeps the note,
/// a `session_start` failure or cancel drops it.
fn finished(
    note: Option<Note>,
    tokens_before: u64,
    instructions: Option<String>,
    started: Started,
) -> (
    HandoffCompleted,
    Vec<(ContextAdded, Vec<contract::hook::StateLine>)>,
) {
    match started {
        Started::Contexts(contexts) => (
            HandoffCompleted {
                outcome: Outcome::Completed,
                error: None,
                note,
                tokens_before,
                instructions,
            },
            contexts,
        ),
        Started::Failed(failure) => (
            HandoffCompleted {
                outcome: Outcome::Failed,
                error: Some(failure),
                note: None,
                tokens_before,
                instructions,
            },
            Vec::new(),
        ),
        Started::Cancelled => (
            HandoffCompleted {
                outcome: Outcome::Cancelled,
                error: None,
                note: None,
                tokens_before,
                instructions,
            },
            Vec::new(),
        ),
    }
}

/// What one handoff runs from: a requested note, or the tool calls'
/// own arguments as the note (`docs/handoff.md`, "A tool").
pub(super) enum Source {
    /// A handoff with a note request: automatic, a person's or overflow.
    Requested {
        trigger: HandoffTrigger,
        instructions: Option<String>,
    },
    /// A tool-started handoff: the taken note, with no note request.
    Tools { note: Vec<ActionId> },
}

impl Loop {
    /// What a decided handoff leaves to do: its completion, and on a
    /// completed one a new context, unmeasured and missing the cache for
    /// everything after the preamble, under a new opening message with
    /// the `session_start` contexts right after it; the files seen are
    /// forgotten. One guard owns the completion, the opening message and
    /// the contexts, across every outcome the hook decided, so a cancel
    /// or a shutdown applies all or none: a refused commit records the
    /// handoff `cancelled`, with no note, no opening message and no
    /// context. The guard holds the cancellation lock, so only writes
    /// run under it; forgetting and the reviewer selection run after it
    /// is released. With no hooks there is no guard and no new write:
    /// the completion, the opening and the restart go as before.
    pub(super) fn restarted(
        &mut self,
        turn: &TurnId,
        done: HandoffCompleted,
        contexts: Vec<(ContextAdded, Vec<contract::hook::StateLine>)>,
        state: Vec<contract::hook::StateLine>,
    ) -> Result<Outcome, Error> {
        let completed = done.outcome == Outcome::Completed;
        if completed {
            self.sent = None;
            self.handoff.measured = None;
        }
        let outcome = done.outcome;
        let tokens_before = done.tokens_before;
        let instructions = done.instructions.clone();
        #[cfg(test)]
        crate::lifecycle::pause_after_handoff_start_check();
        if self.hooks.is_some() {
            // One flag at a time: no test arms both, so each is consumed
            // by its own check.
            #[cfg(test)]
            if crate::timing_tests::take_fail_handoff_completed() {
                return Err(crate::timing_tests::log_io_error());
            }
            #[cfg(test)]
            if crate::timing_tests::take_fail_restarted() {
                return Err(crate::timing_tests::log_io_error());
            }
            let cancel = Arc::clone(&self.cancel);
            let written = cancel.commit(cancel::Commit::Step, || -> Result<(), Error> {
                if completed {
                    self.write_carried(state, Some(turn))?;
                }
                self.append(&Event::HandoffCompleted(done), turn, None)?;
                if completed {
                    self.write_opening(Some(turn))?;
                    for (added, state) in &contexts {
                        self.write_carried(state.clone(), Some(turn))?;
                        self.append(&Event::ContextAdded(added.clone()), turn, None)?;
                    }
                }
                Ok(())
            });
            let Some(written) = written else {
                self.append(
                    &Event::HandoffCompleted(HandoffCompleted {
                        outcome: Outcome::Cancelled,
                        error: None,
                        note: None,
                        tokens_before,
                        instructions,
                    }),
                    turn,
                    None,
                )?;
                return Ok(Outcome::Cancelled);
            };
            written?;
        } else {
            debug_assert!(contexts.is_empty());
            #[cfg(test)]
            if crate::timing_tests::take_fail_handoff_completed() {
                return Err(crate::timing_tests::log_io_error());
            }
            self.append(&Event::HandoffCompleted(done), turn, None)?;
            #[cfg(test)]
            if crate::timing_tests::take_fail_restarted() {
                return Err(crate::timing_tests::log_io_error());
            }
            if completed {
                self.write_opening(Some(turn))?;
            }
        }
        if completed {
            if let Some(forget) = &self.handoff.forget {
                forget();
            }
            self.reviewer_handoff(turn)?;
        }
        Ok(outcome)
    }

    /// One handoff, end to end: `handoff_started`, the note request, then
    /// `handoff_completed` with how it ended (`docs/handoff.md`,
    /// "Recording"). A completed one writes the new opening message. A
    /// tool source makes no request for a note and writes no `started`:
    /// its argument is the note. One guard owns the whole run, across
    /// both sources and their shared recording and restart.
    pub(super) fn run_handoff(
        &mut self,
        turn: &TurnId,
        source: Source,
        tokens_before: u64,
    ) -> Result<Outcome, Error> {
        // The end-to-end scope begins before the first mutation and ends
        // after recording and the restart, or any error.
        let _timer = self.diag.timed(contract::diag::Op::TurnHandoff);
        match source {
            Source::Requested {
                trigger,
                instructions,
            } => self.run_requested_handoff(turn, trigger, instructions, tokens_before),
            Source::Tools { note } => self.run_tool_handoff(turn, note, tokens_before),
        }
    }

    /// A requested handoff: `handoff_started`, the note request, then
    /// `handoff_completed` with how it ended.
    fn run_requested_handoff(
        &mut self,
        turn: &TurnId,
        trigger: HandoffTrigger,
        instructions: Option<String>,
        tokens_before: u64,
    ) -> Result<Outcome, Error> {
        self.handoff.step_ran = true;
        #[cfg(test)]
        if crate::timing_tests::take_fail_handoff_started() {
            return Err(crate::timing_tests::log_io_error());
        }
        self.append(
            &Event::HandoffStarted(HandoffStarted { trigger }),
            turn,
            None,
        )?;
        // The conversation the note request uses, built once: the
        // hook chain sees it without the note request, which belongs
        // to Fiber's own request, and the note request, when one is made,
        // is built from the same conversation, so the overflow hook and
        // the request see the same moved results and no artifact is written
        // twice.
        let has_preamble = self.preamble.is_some();
        let mut request_timer = has_preamble
            .then(|| self.diag.timed(contract::diag::Op::TurnRequest))
            .flatten();
        let mut full = if trigger == HandoffTrigger::Overflow {
            self.moved_results()
        } else {
            self.conversation.clone()
        };
        full.push(Input::User {
            text: note_request_text(&self.handoff.carry.session_log, instructions.as_deref()),
            images: Vec::new(),
        });
        let request = self.request_for(full);
        #[cfg(test)]
        crate::timing_tests::request_note_exit(self.log.clock().as_ref());
        if let Some(timer) = request_timer.as_mut() {
            timer.stop();
        }
        // The request is built before the hook even when a hook note
        // leaves it unused, so a missing preamble still fails here and
        // the hook never sees a corrupt log.
        let noted = match request {
            Some(request) => {
                // The hook runs before the budget check: a hook's note needs
                // no model call, so the budget never stops it. It sees the
                // conversation without the note request, borrowed from the
                // built request, so no second copy is made.
                let hooked = {
                    let conversation: &[Input] = request
                        .conversation
                        .split_last()
                        .map_or(&[], |(_, init)| init);
                    self.hook_note(turn, trigger, instructions.as_deref(), conversation)?
                };
                match hooked {
                    Some(noted) => {
                        // A hook note, a hook failure and a cancel write no
                        // `turn.request` line, because no request is made.
                        if let Some(timer) = request_timer {
                            timer.discard();
                        }
                        noted
                    }
                    // The request path drops the stamped span: one line, with the
                    // assembly's milliseconds and none of the hook's time.
                    None => {
                        drop(request_timer);
                        self.request_note(turn, request)?
                    }
                }
            }
            None => {
                // No request is made on this path, so the held span writes
                // no line.
                if let Some(timer) = request_timer {
                    timer.discard();
                }
                Noted::Failed(failure(
                    ErrorCode::LogCorrupt,
                    "The preamble was not built.",
                ))
            }
        };
        // The hook runs once the note is ready, with the reason and
        // not the note; its contexts follow the new opening message.
        // A `blocking` failure ends the handoff `failed` with
        // `hook_failed`, and the session goes on. The automatic
        // trigger does not fire again in that turn. A hook's note
        // completes like the model's: `handoff_start` maps it the same
        // way, so a `session_start` failure or cancel after it ends the
        // handoff `failed` or `cancelled` with no note, as after the
        // model's.
        let (note, state) = match noted {
            Noted::Note(id) => (Note::Actions { note: vec![id] }, Vec::new()),
            Noted::Hook {
                note_text,
                extension,
                state,
            } => (
                Note::Hook {
                    note_text,
                    extension,
                },
                state,
            ),
            Noted::Failed(failure) => {
                #[cfg(test)]
                if crate::timing_tests::take_fail_handoff_completed() {
                    return Err(crate::timing_tests::log_io_error());
                }
                self.append(
                    &Event::HandoffCompleted(HandoffCompleted {
                        outcome: Outcome::Failed,
                        error: Some(failure),
                        note: None,
                        tokens_before,
                        instructions,
                    }),
                    turn,
                    None,
                )?;
                self.handoff.blocked = true;
                return Ok(Outcome::Failed);
            }
            Noted::Cancelled => {
                #[cfg(test)]
                if crate::timing_tests::take_fail_handoff_completed() {
                    return Err(crate::timing_tests::log_io_error());
                }
                self.append(
                    &Event::HandoffCompleted(HandoffCompleted {
                        outcome: Outcome::Cancelled,
                        error: None,
                        note: None,
                        tokens_before,
                        instructions,
                    }),
                    turn,
                    None,
                )?;
                return Ok(Outcome::Cancelled);
            }
        };
        let (done, contexts) = finished(
            Some(note),
            tokens_before,
            instructions,
            self.handoff_start(turn)?,
        );
        let outcome = self.restarted(turn, done, contexts, state)?;
        if outcome == Outcome::Failed {
            self.handoff.blocked = true;
        }
        Ok(outcome)
    }

    /// A tool-started handoff: no `started` and no note request, with the
    /// same completion and restart as a requested one. The state flags
    /// stay the tool path's own: no block is set. A `blocking` failure
    /// writes no opening message and takes no further handoff this step,
    /// so the call results stay ordinary results.
    fn run_tool_handoff(
        &mut self,
        turn: &TurnId,
        note: Vec<ActionId>,
        tokens_before: u64,
    ) -> Result<Outcome, Error> {
        let taken = Some(Note::Actions { note });
        let (done, contexts) = finished(taken, tokens_before, None, self.handoff_start(turn)?);
        let outcome = self.restarted(turn, done, contexts, Vec::new())?;
        if outcome != Outcome::Completed {
            self.handoff.step_ran = true;
        }
        Ok(outcome)
    }

    /// Sends the built note request. A cancel that landed before the call
    /// ends it at once, as it ends any call (`cancel::run_cancellable`).
    fn request_note(&mut self, turn: &TurnId, request: ModelRequest) -> Result<Noted, Error> {
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
                *text = fill(message("moved-result"), &[("path", path.as_str())]);
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
}
