//! Owns the note request to the model, its recording, and the handoff run.

use contract::events::{
    Event, HandoffCompleted, HandoffStarted, HandoffTrigger, Note, Outcome, TurnCompleted,
    TurnOutcome,
};
use contract::provider::{Finish, Input, Reply, ReplyAction};
use contract::shapes::Failure;
use contract::{ActionId, ErrorCode, TurnId};

use crate::prompt::{fill, message};
use crate::retry::Attempted;
use crate::{Error, Loop};

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
enum Noted {
    /// The reply is the note; its message action carries it.
    Note(ActionId),
    /// The request or the reply failed.
    Failed(Failure),
    /// A person cancelled the turn.
    Cancelled,
}

pub(super) fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.to_owned(),
        retry_after_ms: None,
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
    /// What a completed handoff leaves to do: the next request starts a new
    /// context, unmeasured and missing the cache for everything after the
    /// preamble, under a new opening message; the files seen are forgotten.
    pub(super) fn restarted(&mut self, turn: &TurnId) -> Result<(), Error> {
        self.sent = None;
        self.handoff.measured = None;
        self.write_opening(Some(turn))?;
        if let Some(forget) = &self.handoff.forget {
            forget();
        }
        self.reviewer_handoff(turn)
    }

    /// One handoff: `handoff_started`, the note request, then
    /// `handoff_completed` with how it ended (`docs/handoff.md`,
    /// "Recording"). A completed one writes the new opening message.
    pub(super) fn run_handoff(
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
            images: Vec::new(),
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
