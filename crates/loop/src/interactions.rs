//! A running call's interactions (`docs/events.md`, "Interactions"): the
//! loop thread writes `interaction_requested` for each ask a call raised,
//! answers at once when nobody can (`docs/permissions.md`, "Headless"),
//! and, while one is pending, waits on the inbox as well as on its calls,
//! so the `reply` reaches the step (`docs/architecture.md`, "One inbox").
//! Fiber resolves a pending one on a cancel, a shutdown, `close`, the inbox
//! closing or its `until`, always before its call is released, so before
//! the call's `tool_call_completed` (`docs/architecture.md`,
//! "Cancellation").

use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::time::Instant;

use contract::clock::Wake;
use contract::events::{Answer, Event, InteractionRequested, InteractionResolved, ResolvedBy};
use contract::inbox::Delivery;
use contract::shapes::True;
use contract::tool::Answered;
use contract::{ActionId, ErrorCode, RequestId, TurnId};

use crate::asking::Fitted;
use crate::cancel::SignalState;
use crate::inbox;
use crate::progress::{SharedWake, Stream};
use crate::{Error, Loop, mint};

/// The running calls of a step, each with its stream.
pub(crate) type Calls<'a> = [(&'a ActionId, &'a Stream)];

/// What the step's bounded receive took. It is its own wait, never the
/// idle wait, so no idle state moves.
#[allow(
    clippy::large_enum_variant,
    reason = "a short-lived return value; boxing would allocate on every delivery"
)]
enum Received {
    /// A delivery was waiting, or arrived before `until`.
    Delivery(Delivery),
    /// `until` passed, or the turn is cancelled: the next pass serves.
    Due,
    /// Every sender is gone.
    Closed,
}

impl Loop {
    /// The wake that puts `Delivery::Cancelled` in this loop's inbox.
    /// Without one, no interaction a tool call raises can be answered.
    pub fn inbox_wake(mut self, wake: Arc<dyn Wake>) -> Self {
        self.inbox_wake = Some(wake);
        self
    }

    /// Writes the lines for every ask raised since the last pass, answers
    /// at once the ones nobody can answer, and resolves each pending one
    /// whose `until` passed or that nobody can answer any more.
    pub(crate) fn serve_interactions(
        &mut self,
        calls: &Calls<'_>,
        turn: &TurnId,
    ) -> Result<(), Error> {
        let now = self.log.clock().now();
        // `fiber ask`, a delegate, after `close`, or a loop no inbox wake
        // reaches: no answer can come (`docs/permissions.md`, "Headless").
        // A cancelled turn, a shutdown included, resolves what it holds.
        let nobody = !self.answerable || self.inbox_wake.is_none() || self.turn_cancelled();
        for (id, stream) in calls {
            let slot = stream.asking();
            if let Some(asking) = slot.take_raised() {
                let request = RequestId(mint("r_"));
                if nobody {
                    // No `interaction_requested`: the resolved line names a
                    // request never raised (`docs/events.md`,
                    // "Interactions").
                    self.declined(request, turn)?;
                    slot.resolve(Answered::NoAnswer);
                    continue;
                }
                let action_ids = if asking.action_ids.is_empty() {
                    vec![(*id).clone()]
                } else {
                    asking.action_ids
                };
                let requested = InteractionRequested {
                    request_id: request.clone(),
                    interaction: asking.interaction.clone(),
                    action_ids: Some(action_ids),
                    extension: None,
                    // Only an ask that suspends may be raised again by
                    // running its call (`docs/events.md`, "Resume").
                    resumes: asking.suspends,
                };
                self.append(&Event::InteractionRequested(requested), turn, None)?;
                slot.pend(request, asking.interaction, asking.until, asking.check);
            }
            if let Some((request, until)) = slot.pending()
                && (nobody || until.is_some_and(|until| now >= until))
            {
                self.declined(request, turn)?;
                slot.resolve(Answered::NoAnswer);
            }
        }
        Ok(())
    }

    /// The earliest `until` among the pending interactions.
    pub(crate) fn interaction_deadline(calls: &Calls<'_>) -> Option<Instant> {
        calls
            .iter()
            .filter_map(|(_, stream)| stream.asking().pending().and_then(|(_, until)| until))
            .min()
    }

    /// Parks on `wake` when no interaction is pending. Otherwise forwards
    /// `wake` to the inbox wake, so an emit, a call returning, an ask, the
    /// cancel and a clock move each reach the inbox, then takes one
    /// delivery, or returns at `until`.
    pub(crate) fn wait_step(
        &mut self,
        wake: &SharedWake,
        calls: &Calls<'_>,
        until: Option<Instant>,
        turn: &TurnId,
    ) -> Result<(), Error> {
        let pending = calls
            .iter()
            .any(|(_, stream)| stream.asking().pending().is_some());
        let Some(target) = self.inbox_wake.clone().filter(|_| pending) else {
            wake.park(self.log.clock().as_ref(), until);
            return Ok(());
        };
        wake.forward(Some(target));
        let received = self.receive(until);
        wake.forward(None);
        match received {
            Received::Due => Ok(()),
            Received::Closed => {
                for (_, stream) in calls {
                    if let Some((request, _)) = stream.asking().pending() {
                        self.declined(request, turn)?;
                        stream.asking().resolve(Answered::NoAnswer);
                    }
                }
                Ok(())
            }
            Received::Delivery(delivery) => self.take_while_asked(delivery, calls, turn),
        }
    }

    /// The step's bounded receive: at once when the turn is cancelled,
    /// then anything already queued, then a wait on the loop's clock until
    /// `until`. Unlike the idle wait it honours `until` while a job runs
    /// and moves no idle state.
    fn receive(&self, until: Option<Instant>) -> Received {
        if self.cancel.state() != SignalState::Live {
            return Received::Due;
        }
        match self.inbox.try_recv() {
            Ok(delivery) => return Received::Delivery(delivery),
            Err(TryRecvError::Disconnected) => return Received::Closed,
            Err(TryRecvError::Empty) => {}
        }
        let clock = Arc::clone(self.log.clock());
        let mut slot = None;
        clock.wait_until(until, &mut |bound| {
            // `None` blocks until a delivery or the channel closes.
            slot = Some(match bound {
                None => self
                    .inbox
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
                Some(limit) => self.inbox.recv_timeout(limit),
            });
        });
        match slot {
            Some(Ok(delivery)) => Received::Delivery(delivery),
            Some(Err(RecvTimeoutError::Disconnected)) => Received::Closed,
            Some(Err(RecvTimeoutError::Timeout)) | None => Received::Due,
        }
    }

    /// Admits `delivery` while a call's interaction is pending. A `reply`
    /// naming a pending request is fitted to it: an unfit one is rejected
    /// `invalid_arguments` and the request stays pending, and a fitting one
    /// is written, then accepted, then handed to the call. Everything else
    /// is admitted as at a step boundary, a `reply` naming nothing pending
    /// rejected `stale_request` (`docs/invocation.md`, "Replying").
    fn take_while_asked(
        &mut self,
        delivery: Delivery,
        calls: &Calls<'_>,
        turn: &TurnId,
    ) -> Result<(), Error> {
        // One read of the signal, first, as an approval wait reads it: with
        // a cancel or a shutdown the delivery is admitted as at any drain,
        // and the next pass resolves what is pending.
        if self.cancel.state() != SignalState::Live {
            return self.admit_running(delivery, turn);
        }
        let Delivery::Reply(reply, ack) = delivery else {
            return self.admit_running(delivery, turn);
        };
        for (_, stream) in calls {
            match stream.asking().fit(&reply.request_id, &reply.answer) {
                Fitted::NotThis => {}
                Fitted::Unfit => {
                    inbox::reject(ack, ErrorCode::InvalidArguments, inbox::UNFIT_REPLY);
                    return Ok(());
                }
                Fitted::Fits(answer) => {
                    let resolved = InteractionResolved {
                        request_id: reply.request_id.clone(),
                        by: ResolvedBy::Person,
                        answer: answer.clone(),
                    };
                    self.append(&Event::InteractionResolved(resolved), turn, None)?;
                    inbox::accept(ack);
                    stream.asking().resolve(Answered::Reply(answer));
                    return Ok(());
                }
            }
        }
        self.admit_running(Delivery::Reply(reply, ack), turn)
    }

    /// Writes Fiber's decline of `request`: `by` `fiber`, `declined: true`.
    fn declined(&mut self, request: RequestId, turn: &TurnId) -> Result<(), Error> {
        let resolved = InteractionResolved {
            request_id: request,
            by: ResolvedBy::Fiber,
            answer: Answer::Declined { declined: True },
        };
        self.append(&Event::InteractionResolved(resolved), turn, None)
    }
}
