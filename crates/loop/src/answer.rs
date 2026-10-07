//! Waiting for a person's answer to a raised approval (`docs/permissions.md`,
//! "What the log records"): one wait shared by a new request and one a
//! resume raised again, so the two cannot drift.

use contract::events::{AskStep, DecidedBy, Decision, PermissionRequested};
use contract::inbox::Delivery;
use contract::{ActionId, ErrorCode, TurnId};

use crate::calls::Asked;
use crate::completion::{denied, resolved};
use crate::inbox::{self, InboxRecv, Waited};
use crate::{Error, Loop};

impl Loop {
    /// Waits for the answer to `request`, already written for the call
    /// `id` of `tool`. Other deliveries are admitted as at any drain. A
    /// reply that does not fit is rejected and the wait goes on; one that
    /// names another request is rejected `stale_request`. `close` taken
    /// while waiting returns [`Asked::Closed`]; the inbox closing denies
    /// with no person to answer. A reply or `close` held aside before the
    /// request was raised again is taken first, as if it came now.
    pub(crate) fn await_answer(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        tool: &str,
        request: &PermissionRequested,
    ) -> Result<Asked, Error> {
        let request_id = request.request_id.clone();
        let offer = match &request.step {
            AskStep::StandingAsk { .. } => None,
            AskStep::Review { rule, .. } => rule.clone(),
        };
        // Who denies when the inbox closes with no answer follows from
        // the step, as the request carries it.
        let closed_by = match &request.step {
            AskStep::StandingAsk { .. } => DecidedBy::StandingRule,
            AskStep::Review { .. } => DecidedBy::Reviewer,
        };
        // Waiting on an approval is idle (`docs/invocation.md`, "Lifecycle").
        // The deadline is this moment, and a rejected reply does not move it.
        let deadline = self.idle_deadline();
        loop {
            let delivery = match self.take_held_answer(&request_id) {
                Some(delivery) => delivery,
                None => match self.recv_until(deadline, false) {
                    InboxRecv::Delivery(delivery) => delivery,
                    // Without `check` the wait never ends unattended; the
                    // arm keeps the match total.
                    InboxRecv::Idle | InboxRecv::Unattended => {
                        self.idle_left = true;
                        return Ok(Asked::Idle);
                    }
                    // Every sender is gone, so no answer can come.
                    InboxRecv::Closed => {
                        let reason = "The session ended while waiting for an answer.";
                        self.decided(
                            id,
                            turn,
                            resolved(
                                Some(request_id),
                                Decision::Deny,
                                closed_by,
                                Some(reason.to_owned()),
                                None,
                            ),
                        )?;
                        return Ok(Asked::Gone(denied(
                            "no_person",
                            format!("{reason} It did not run."),
                        )));
                    }
                },
            };
            match self.take_while_waiting(&request_id, delivery, turn)? {
                Waited::Again => {}
                Waited::Closed => return Ok(Asked::Closed(request_id)),
                // The wake the door delivers after an accepted cancel:
                // the request ends denied by the cancel, and the call
                // completes `cancelled`. A stale wake from an earlier
                // turn finds the signal not cancelled and keeps waiting.
                Waited::Cancelled => {
                    self.decided(
                        id,
                        turn,
                        resolved(
                            Some(request_id),
                            Decision::Deny,
                            DecidedBy::Cancel,
                            None,
                            None,
                        ),
                    )?;
                    return Ok(Asked::Cancelled);
                }
                Waited::Reply(reply, ack) => {
                    let Some(answered) = self.answered(tool, offer.as_ref(), &reply.answer) else {
                        inbox::reject(ack, ErrorCode::InvalidArguments, inbox::UNFIT_REPLY);
                        continue;
                    };
                    inbox::accept(ack);
                    match answered.decision {
                        Decision::Deny => {
                            let text = match &answered.feedback {
                                Some(feedback) => format!(
                                    "A person refused this call: {feedback}. It did not run."
                                ),
                                None => "A person refused this call. It did not run.".to_owned(),
                            };
                            self.decided(
                                id,
                                turn,
                                resolved(
                                    Some(request_id),
                                    Decision::Deny,
                                    DecidedBy::Person,
                                    None,
                                    answered.feedback,
                                ),
                            )?;
                            return Ok(Asked::Deny(denied("person", text)));
                        }
                        Decision::Allow => {
                            self.decided(id, turn, answered.allow(request_id))?;
                            return Ok(Asked::Allow);
                        }
                    }
                }
            }
        }
    }

    /// The first delivery held aside before a finishing turn raised its
    /// request again that answers it: a `reply` naming `request_id`, or
    /// `close`. Everything else stays held, in order, for the next turn.
    fn take_held_answer(&mut self, request_id: &contract::RequestId) -> Option<Delivery> {
        let at = self.deferred.iter().position(|held| match held {
            Delivery::Reply(reply, _) => reply.request_id == *request_id,
            Delivery::Close(_) => true,
            Delivery::Prompt(..)
            | Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Handoff(..)
            | Delivery::Job(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::Cancelled => false,
        })?;
        self.deferred.remove(at)
    }
}
