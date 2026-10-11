//! A turn cut short on a question that suspends (`docs/tools.md`, "When a
//! person can answer"): the process exited with the question pending, and
//! resuming runs the call that raised it again, with no second
//! `tool_call_started`, so it raises the question again under the same
//! `request_id` (`docs/events.md`, "Resume"). Only a request logged
//! `resumes: true` is raised again this way.

use std::sync::Arc;

use contract::events::{
    Event, InteractionRequested, PermissionRequested, ToolCallRequested, TurnOutcome,
};
use contract::{ActionId, Envelope, RequestId, TurnId};

use crate::calls::Decided;
use crate::cancel::Commit;
use crate::resume::Suspended;
use crate::{Error, Loop};

/// The request a suspended turn stopped on.
#[derive(Clone)]
pub(crate) enum Pending {
    /// An approval, raised again by the finishing turn.
    Approval(PermissionRequested),
    /// A question, raised again by running its call again.
    Interaction(InteractionRequested),
}

impl Pending {
    /// The request's id, which the finishing turn keeps.
    pub(crate) fn request_id(&self) -> &RequestId {
        match self {
            Self::Approval(request) => &request.request_id,
            Self::Interaction(asked) => &asked.request_id,
        }
    }
}

/// The `interaction_requested` named `pending` when a resume may raise it
/// again, with its turn and the call that raised it: it is logged
/// `resumes: true`, no extension raised it, it names exactly one call, that
/// call has a `tool_call_started`, and no `interaction_resolved` answers
/// it. Whether its turn and its call are still open is
/// [`crate::resume::suspended`]'s check, shared with an approval's.
pub(crate) fn interaction_pending(
    lines: &[Envelope],
    pending: &RequestId,
) -> Result<Option<(InteractionRequested, TurnId, ActionId)>, Error> {
    let mut found = None;
    let kinds = ["interaction_requested", "interaction_resolved"];
    for line in lines
        .iter()
        .filter(|line| line.is_durable() && kinds.contains(&line.kind.as_str()))
    {
        match Event::from_envelope(line).map_err(Error::Unreadable)? {
            // A request raised again is logged again: the latest stands.
            Some(Event::InteractionRequested(asked)) if asked.request_id == *pending => {
                found = line.turn_id.clone().map(|turn| (asked, turn));
            }
            Some(Event::InteractionResolved(resolved)) if resolved.request_id == *pending => {
                return Ok(None);
            }
            Some(_) | None => {}
        }
    }
    let Some((asked, turn)) = found else {
        return Ok(None);
    };
    if !asked.resumes || asked.extension.is_some() {
        return Ok(None);
    }
    let Some([action]) = asked.action_ids.as_deref() else {
        return Ok(None);
    };
    // Running the call again writes no second `tool_call_started`, so it
    // must have one (`docs/events.md`, "Resume").
    let started = lines.iter().any(|line| {
        line.is_durable()
            && line.kind == "tool_call_started"
            && line.action_id.as_ref() == Some(action)
    });
    if !started {
        return Ok(None);
    }
    let action = action.clone();
    Ok(Some((asked, turn, action)))
}

impl Loop {
    /// Writes the suspended request again, under its turn, as its re-raise
    /// does: an approval under its action, a question with no action
    /// (`docs/events.md`, "Interactions").
    pub(crate) fn keep_suspended(&mut self, suspended: &Suspended) -> Result<(), Error> {
        self.raise_again(&suspended.pending, &suspended.turn, &suspended.action)
    }

    /// Writes `pending` again under `turn`; an approval under `action`.
    fn raise_again(
        &mut self,
        pending: &Pending,
        turn: &TurnId,
        action: &ActionId,
    ) -> Result<(), Error> {
        match pending {
            Pending::Approval(request) => self.append(
                &Event::PermissionRequested(request.clone()),
                turn,
                Some(action),
            ),
            Pending::Interaction(asked) => {
                self.append(&Event::InteractionRequested(asked.clone()), turn, None)
            }
        }
    }

    /// Finishes a turn cut short on a question, once the preamble is
    /// written: the question is raised again under the same `request_id`,
    /// and its call, allowed before the exit, runs again without being
    /// judged again and with no second `tool_call_started`; its ask pends
    /// under that request. A reply or `close` held aside answers it as if it
    /// came now. With nobody to answer, Fiber declines the request raised
    /// again, and the call, with nobody to ask, hands its questions to the
    /// driver (`docs/invocation.md`, "Lifecycle"). Then the turn goes on as
    /// after any batch.
    pub(crate) fn finish_suspended_form(
        &mut self,
        suspended: Suspended,
    ) -> Result<Option<TurnOutcome>, Error> {
        let Suspended {
            turn,
            pending,
            action,
            batch,
        } = suspended;
        let request = pending.request_id().clone();
        // Armed with the re-raise: a shutdown before it leaves the request
        // pending, so the next resume raises it again (`docs/invocation.md`,
        // "Shutdown").
        let cancel = Arc::clone(&self.cancel);
        let raised = cancel.commit(Commit::Arm, || self.raise_again(&pending, &turn, &action));
        let Some(raised) = raised else {
            return Ok(None);
        };
        raised?;
        let calls = form_batch(
            batch,
            &action,
            || Err(self.cancelled_before_ran()),
            |call| {
                self.checked(call)
                    .map(|(tool, arguments, effects)| (tool, arguments, effects.declared))
            },
        );
        let cancelled = self.run_batch(calls, &turn, Some((action, request)))?;
        self.finish_batch(&turn, cancelled)
    }

    /// The finishing turn after its batch. The idle delay passing in it
    /// writes nothing more, as in any step: the turn resumes later, so a
    /// queued switch is dropped. Otherwise orphan notices the resume logged
    /// behind the open batch follow it; a cancel that ended the batch ends
    /// the turn `interrupted` at the next step's start, and a spent headless
    /// block budget ends it `failed` `blocked` before the batch's questions
    /// are processed, as a step's do.
    pub(crate) fn finish_batch(
        &mut self,
        turn: &TurnId,
        cancelled: bool,
    ) -> Result<Option<TurnOutcome>, Error> {
        if self.idle_left {
            self.pending.clear();
            self.cancel.disarm();
            return Ok(None);
        }
        self.conversation.append(&mut self.held);
        if !cancelled {
            if let Some(blocked) = self.take_blocked_end() {
                return self.end_turn(turn, blocked);
            }
            if let Some(completed) = self.after_calls(turn)? {
                return self.end_turn(turn, completed);
            }
        }
        self.run_steps(turn)
    }
}

/// The finishing turn's batch: the calls before `action` get `before`,
/// the action gets `decide` of its call, and the calls after it are judged
/// as in any step. `decide` runs once, at the first entry naming the
/// action: a later duplicate names an id no call holds decisions for, so
/// it gets `None` like any later call. With no `action` in the batch every
/// entry gets `before` and `decide` never runs.
pub(crate) fn form_batch(
    batch: Vec<(ActionId, ToolCallRequested)>,
    action: &ActionId,
    before: impl Fn() -> Decided,
    decide: impl FnOnce(&ToolCallRequested) -> Decided,
) -> Vec<(ActionId, ToolCallRequested, Option<Decided>)> {
    let mut decide = Some(decide);
    let mut at_action = false;
    let mut calls = Vec::with_capacity(batch.len());
    for (id, call) in batch {
        let already = if id == *action {
            at_action = true;
            decide.take().map(|decide| decide(&call))
        } else if at_action {
            None
        } else {
            Some(before())
        };
        calls.push((id, call, already));
    }
    calls
}

#[cfg(test)]
#[path = "suspend_tests.rs"]
mod tests;
