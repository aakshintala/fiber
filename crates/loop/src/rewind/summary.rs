//! The optional rewind summary: one turn on the old session, with no inbox
//! drain or continuation (`docs/events.md`, "Rewind").

use std::sync::Arc;

use contract::events::{Empty, Event, InputItem, TurnCompleted, TurnOutcome, TurnStarted};
use contract::provider::{Finish, Input};
use contract::shapes::Failure;
use contract::{CommandId, Envelope, ErrorCode, Seq, TurnId};

use crate::handoff::{failure, reply_note};
use crate::prompt::{fill, message};
use crate::retry::Attempted;
use crate::{Error, Loop, cancel};

use super::command::Target;

impl Loop {
    /// Makes the summary before closing admission or minting the new session.
    pub(crate) fn summarise(
        &mut self,
        id: CommandId,
        target: &Target,
        point: u64,
    ) -> Result<Result<String, Failure>, Error> {
        if !self.offer(None)? {
            return Ok(Err(failure(
                ErrorCode::SummaryFailed,
                "the session ended while the repository's offer waited.",
            )));
        }
        self.write_settled()?;
        self.ensure_preamble()?;
        let contexts = self.session_start_once()?;
        let turn = TurnId(crate::mint("t_"));
        let input = vec![InputItem::Rewind { command_id: id }];
        let cancel = Arc::clone(&self.cancel);
        let written = cancel.commit(cancel::Commit::Arm, || -> Result<(), Error> {
            for (added, state) in contexts {
                self.write_carried(state, None)?;
                self.write(&Event::ContextAdded(added), None, None)?;
            }
            self.append(
                &Event::TurnStarted(TurnStarted {
                    input: input.clone(),
                }),
                &turn,
                None,
            )
        });
        let Some(written) = written else {
            return Ok(Err(failure(
                ErrorCode::SummaryFailed,
                "the session is shutting down.",
            )));
        };
        written?;
        if let Some(completed) = self.turn_start(&turn, &input)? {
            return self.finish_summary(&turn, completed, None);
        }
        let written = cancel.commit(cancel::Commit::Step, || {
            self.append(&Event::StepStarted(Empty {}), &turn, None)
        });
        let Some(written) = written else {
            return self.finish_summary(&turn, crate::ended(TurnOutcome::Interrupted, None), None);
        };
        written?;
        let forked = crate::history::forked(&target.dir, Seq(point))?;
        let lines = crate::history::read_window(&self.log, forked.window, self.log.count())?;
        let cut = lines
            .iter()
            .position(|line| {
                line.session_id == target.session && line.seq.is_some_and(|seq| seq.0 > point)
            })
            .unwrap_or_else(|| {
                lines
                    .iter()
                    .rposition(|line| line.session_id == target.session)
                    .map_or(0, |at| at + 1)
            });
        let (mut conversation, sent) = crate::conversation::summary_conversation(
            &lines,
            cut,
            &self.model.reference,
            forked.seed,
        )?;
        let point = excerpt(lines.get(cut..).unwrap_or_default())
            .map(|excerpt| fill(message("rewind-summary-point"), &[("excerpt", &excerpt)]))
            .unwrap_or_default();
        conversation.push(Input::User {
            text: fill(message("rewind-summary"), &[("point", &point)]),
            images: vec![],
        });
        let Some(mut request) = self.request_for(conversation) else {
            return self.finish_summary(
                &turn,
                crate::ended(
                    TurnOutcome::Failed,
                    Some(failure(
                        ErrorCode::LogCorrupt,
                        "The preamble was not built.",
                    )),
                ),
                None,
            );
        };
        request.previous_end = sent;
        if let Some(completed) = self.over_budget() {
            let completed = self.budget_end(completed);
            return self.finish_summary(&turn, completed, None);
        }
        if let Some(completed) = self.model_call_allowed(&turn)? {
            return self.finish_summary(&turn, completed, None);
        }
        let (completed, summary) = match self.call_with_retries(&request, &turn)? {
            Attempted::Failed(failed) => (crate::ended(TurnOutcome::Failed, Some(failed)), None),
            Attempted::Interrupted => (crate::ended(TurnOutcome::Interrupted, None), None),
            Attempted::Replied {
                reply,
                reasoning,
                message,
            } => {
                let text = reply_note(&reply);
                let (calls, finish) = self.write_reply(reply, reasoning, &turn, &message)?;
                let called = !calls.is_empty();
                for (id, _) in calls {
                    let completed = self.cancelled_before_ran();
                    self.append(&Event::ToolCallCompleted(*completed), &turn, Some(&id))?;
                }
                let result = if called {
                    Err(failure(
                        ErrorCode::SummaryFailed,
                        "The summary reply made tool calls.",
                    ))
                } else if finish == Finish::OutputLimit {
                    Err(failure(
                        ErrorCode::SummaryFailed,
                        "The summary reached the output limit.",
                    ))
                } else if text.trim().is_empty() {
                    Err(failure(
                        ErrorCode::SummaryFailed,
                        "The summary reply held no text.",
                    ))
                } else {
                    Ok(text)
                };
                let outcome = self.end_turn(&turn, crate::ended(TurnOutcome::Completed, None))?;
                return Ok(if outcome == Some(TurnOutcome::Completed) {
                    result
                } else {
                    Err(failure(
                        ErrorCode::SummaryFailed,
                        "the summary was interrupted.",
                    ))
                });
            }
        };
        self.finish_summary(&turn, completed, summary)
    }

    fn finish_summary(
        &mut self,
        turn: &TurnId,
        completed: TurnCompleted,
        summary: Option<String>,
    ) -> Result<Result<String, Failure>, Error> {
        let reason = completed
            .error
            .clone()
            .unwrap_or_else(|| failure(ErrorCode::SummaryFailed, "the summary was interrupted."));
        let outcome = self.end_turn(turn, completed)?;
        Ok(match (outcome, summary) {
            (Some(TurnOutcome::Completed), Some(text)) => Ok(text),
            _ => Err(reason),
        })
    }
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;

/// The first message, reply text or tool call after the point.
fn excerpt(lines: &[Envelope]) -> Option<String> {
    let mut inside = false;
    for line in lines {
        let Some(event) = Event::from_envelope(line).ok().flatten() else {
            continue;
        };
        if let Event::TurnStarted(started) = &event {
            inside = started
                .input
                .iter()
                .any(|item| matches!(item, InputItem::Rewind { .. }));
        }
        if inside {
            if matches!(event, Event::TurnCompleted(_) | Event::FiberStarted(_)) {
                inside = false;
            }
            continue;
        }
        let text = match event {
            Event::TurnStarted(started) => started.input.iter().find_map(|item| match item {
                InputItem::Message { content, .. } => Some(crate::conversation::text(content)),
                _ => None,
            }),
            Event::TextCompleted(text) => Some(text.text),
            Event::ToolCallRequested(call) => Some(format!("A call to the `{}` tool.", call.name)),
            _ => None,
        };
        if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
            return Some(text);
        }
    }
    None
}
