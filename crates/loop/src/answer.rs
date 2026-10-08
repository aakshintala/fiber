//! Waiting for a person's answer to a raised approval (`docs/permissions.md`,
//! "What the log records"): one wait shared by a new request and one a
//! resume raised again, so the two cannot drift.

use contract::events::{AskStep, DecidedBy, Decision, PermissionRequested};
use contract::inbox::Delivery;
use contract::{ActionId, ErrorCode, RequestId, TurnId};

use crate::calls::Asked;
use crate::completion::{denied, resolved};
use crate::inbox::{self, InboxRecv, Waited};
use crate::{Error, Loop};

/// The denial's reason when the session closed while the request was
/// pending (`docs/events.md`, `permission_resolved`).
const CLOSED: &str = "The session closed while waiting for an answer.";
/// The denial's reason when the turn was cancelled while the request was
/// pending (`docs/events.md`, `permission_resolved`).
const CANCELLED: &str = "The turn was cancelled while waiting for an answer.";

impl Loop {
    /// Waits for the answer to `request`, already written for the call
    /// `id` of `tool`. Other deliveries are admitted as at any drain. A
    /// reply that does not fit is rejected and the wait goes on; one that
    /// names another request is rejected `stale_request`. `close` taken
    /// while waiting denies the request by `cancel` and returns
    /// [`Asked::Closed`]; the inbox closing denies it the same way while
    /// the session is not shutting down, and leaves it pending when it is
    /// (`docs/events.md`, `permission_resolved`; `docs/invocation.md`,
    /// "Shutdown"). A reply or `close` held aside before the request was
    /// raised again is taken first, as if it came now.
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
        // Waiting on an approval is idle (`docs/invocation.md`, "Lifecycle").
        // The deadline is this moment, and a rejected reply does not move it.
        let deadline = self.idle_deadline();
        loop {
            let delivery = match self.take_held_answer(&request_id) {
                Some(delivery) => delivery,
                None => match self.recv_until(deadline, false, None) {
                    InboxRecv::Delivery(delivery) => delivery,
                    // Every sender is gone, so no answer can come: the session
                    // closed while the request was pending, and the request
                    // is denied by `cancel` (`docs/events.md`,
                    // `permission_resolved`). Under a shutdown the request
                    // stays pending for the resume instead
                    // (`docs/invocation.md`, "Shutdown").
                    InboxRecv::Closed if !self.shutting_down() => {
                        self.closed(id, turn, request_id.clone())?;
                        return Ok(Asked::Gone(denied(
                            "no_person",
                            format!("{CLOSED} It did not run."),
                        )));
                    }
                    // Without `check` or a refresh instant the wait never
                    // ends unattended or to warm; the
                    // arm keeps the match total. A disconnect under a
                    // shutdown joins it: the wait ends as the idle delay
                    // ends it, and the request stays pending.
                    InboxRecv::Idle
                    | InboxRecv::Unattended
                    | InboxRecv::Warm
                    | InboxRecv::Closed => {
                        self.idle_left = true;
                        return Ok(Asked::Idle);
                    }
                },
            };
            match self.take_while_waiting(&request_id, delivery, turn)? {
                Waited::Again => {}
                // `close` was taken while the request was pending: the
                // request is denied by `cancel`, and the caller only
                // completes the call (`docs/events.md`,
                // `permission_resolved`).
                Waited::Closed => {
                    self.closed(id, turn, request_id.clone())?;
                    return Ok(Asked::Closed);
                }
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
                            Some(CANCELLED.to_owned()),
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

    /// Denies `request_id` by `cancel` with [`CLOSED`]: the session closed
    /// while it was pending.
    fn closed(&mut self, id: &ActionId, turn: &TurnId, request_id: RequestId) -> Result<(), Error> {
        self.decided(
            id,
            turn,
            resolved(
                Some(request_id),
                Decision::Deny,
                DecidedBy::Cancel,
                Some(CLOSED.to_owned()),
                None,
            ),
        )
    }

    /// The first delivery held aside before a finishing turn raised its
    /// request again that answers it: a `reply` naming `request_id`, or
    /// `close`. Everything else stays held, in order, for the next turn.
    pub(crate) fn take_held_answer(
        &mut self,
        request_id: &contract::RequestId,
    ) -> Option<Delivery> {
        let at = self.deferred.iter().position(|held| match held {
            Delivery::Reply(reply, _) => reply.request_id == *request_id,
            Delivery::Close(_) => true,
            Delivery::Prompt(..)
            | Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Handoff(..)
            | Delivery::Model(..)
            | Delivery::Rewind(..)
            | Delivery::Interaction(_)
            | Delivery::Resolved(..)
            | Delivery::Job(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::ExtensionLog(_)
            | Delivery::Cancelled => false,
        })?;
        self.deferred.remove(at)
    }
}
