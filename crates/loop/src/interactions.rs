//! A running call's interactions (`docs/events.md`, "Interactions"): the
//! loop thread writes `interaction_requested` for each ask a call raised,
//! answers at once when nobody can (`docs/permissions.md`, "Headless"),
//! and, while one is pending, waits on the inbox as well as on its calls,
//! so the `reply` reaches the step (`docs/architecture.md`, "One inbox").
//! Fiber resolves a pending one on a cancel, a shutdown, `close`, the inbox
//! closing or its `until`, always before its call is released, so before
//! the call's `tool_call_completed` (`docs/architecture.md`,
//! "Cancellation"). A question that suspends is the exception: once its
//! call is the step's only call without a result, the idle delay or a
//! shutdown ends the step with it still pending (`docs/invocation.md`,
//! "Lifecycle" and "Shutdown").

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

/// The calls of a step without a written completion, in request order,
/// each with its stream while it runs.
pub(crate) type Calls<'a> = [(&'a ActionId, Option<&'a Stream>)];

/// The running calls among `calls`, each with its stream.
fn running<'a>(calls: &'a Calls<'a>) -> impl Iterator<Item = (&'a ActionId, &'a Stream)> {
    calls
        .iter()
        .filter_map(|(id, stream)| stream.map(|stream| (*id, stream)))
}

#[cfg(test)]
thread_local! {
    /// A cancel the test runs after the take, before the decision read:
    /// the order a cancel landing between the two would force
    /// (`docs/testing.md`, "What a change ships with" allows the hook
    /// where no outside seam reaches the race).
    static AFTER_TAKE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

/// Runs the hook [`AFTER_TAKE`] holds, if any, then clears it.
#[cfg(test)]
fn pause_after_take() {
    AFTER_TAKE.with(|hook| {
        if let Some(paused) = hook.borrow_mut().take() {
            paused();
        }
    });
}

/// The request the step may suspend on: its only call without a written
/// completion waits on a pending interaction raised with `suspends`. While
/// another call has no result, its result would be written after this
/// one's, so the step waits for the answer (`docs/architecture.md`, "Tool
/// calls in a step").
fn suspendable(calls: &Calls<'_>) -> Option<RequestId> {
    let [(_, Some(stream))] = calls else {
        return None;
    };
    let slot = stream.asking();
    if !slot.pending_suspends() {
        return None;
    }
    slot.pending().map(|(request, _)| request)
}

/// The request a step may suspend on, and the idle deadline counted from
/// the moment it could: a rejected reply does not move it
/// (`docs/invocation.md`, "Lifecycle").
#[derive(Default)]
pub(crate) struct Suspend(Option<(RequestId, Option<Instant>)>);

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

    /// Whether no answer can come for an ask taken now: `fiber ask`, a
    /// delegate, after `close`, or a loop no inbox wake reaches
    /// (`docs/permissions.md`, "Headless"). A cancelled turn, a shutdown
    /// included, resolves what it holds.
    fn interactions_unanswerable(&self) -> bool {
        !self.answerable || self.inbox_wake.is_none() || self.turn_cancelled()
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
        let last = calls.last().map(|(id, _)| *id);
        for (id, stream) in running(calls) {
            let slot = stream.asking();
            let raised = slot.take_raised();
            #[cfg(test)]
            pause_after_take();
            // Read after the take, per slot: a cancel landing before it
            // declines the ask.
            let nobody = self.interactions_unanswerable();
            // A shutdown leaves a question that suspends pending when no later
            // call of the step is without its result, so resuming raises it
            // again (`docs/invocation.md`, "Shutdown").
            let kept = self.answerable && self.inbox_wake.is_some() && self.shutting_down();
            if let Some(asking) = raised {
                // A request raised again on resume is already written: the
                // ask pends under it (`docs/events.md`, "Resume").
                let again = slot.take_reraise();
                let request = again.clone().unwrap_or_else(|| RequestId(mint("r_")));
                if nobody {
                    // Unless raised again, no `interaction_requested`: the
                    // resolved line names a request never raised
                    // (`docs/events.md`, "Interactions").
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
                if again.is_none() {
                    self.append(&Event::InteractionRequested(requested), turn, None)?;
                }
                slot.pend(
                    request,
                    asking.interaction,
                    asking.until,
                    asking.check,
                    asking.suspends,
                );
            }
            let stays = kept && slot.pending_suspends() && last == Some(id);
            if let Some((request, until)) = slot.pending()
                && ((nobody && !stays) || until.is_some_and(|until| now >= until))
            {
                self.declined(request, turn)?;
                slot.resolve(Answered::NoAnswer);
            }
        }
        Ok(())
    }

    /// The earliest `until` among the pending interactions.
    pub(crate) fn interaction_deadline(calls: &Calls<'_>) -> Option<Instant> {
        running(calls)
            .filter_map(|(_, stream)| stream.asking().pending().and_then(|(_, until)| until))
            .min()
    }

    /// Parks on `wake` when no interaction is pending. Otherwise forwards
    /// `wake` to the inbox wake, so an emit, a call returning, an ask, the
    /// cancel and a clock move each reach the inbox, then takes one
    /// delivery, or returns at `until`. True when the step ends suspended
    /// instead: it can suspend on a question, and the idle delay passed or
    /// a shutdown started. `idle_left` is then set and nothing more is
    /// written (`docs/invocation.md`, "Lifecycle").
    pub(crate) fn wait_step(
        &mut self,
        wake: &SharedWake,
        calls: &Calls<'_>,
        mut until: Option<Instant>,
        suspend: &mut Suspend,
        turn: &TurnId,
    ) -> Result<bool, Error> {
        let clock = Arc::clone(self.log.clock());
        suspend.0 = match (suspendable(calls), suspend.0.take()) {
            (Some(request), Some((held, deadline))) if request == held => Some((held, deadline)),
            (Some(request), _) => Some((request, self.idle_deadline())),
            (None, _) => None,
        };
        if let Some(deadline) = suspend.0.as_ref().map(|(_, deadline)| *deadline) {
            // A shutdown ends the wait as the idle delay does
            // (`docs/invocation.md`, "Shutdown").
            if self.shutting_down() {
                self.idle_left = true;
                return Ok(true);
            }
            // Waiting on a question is idle, and a running job is not
            // (`docs/invocation.md`, "Lifecycle").
            let idle = self.idle_until(deadline);
            if idle.is_some_and(|at| clock.now() >= at) {
                // Anything already queued is taken first, as the idle wait
                // takes it.
                match self.inbox.try_recv() {
                    Ok(delivery) => {
                        return self.take_while_asked(delivery, calls, turn).map(|()| false);
                    }
                    Err(TryRecvError::Disconnected) => {
                        return self.inbox_closed(calls, turn).map(|()| false);
                    }
                    Err(TryRecvError::Empty) => {
                        self.idle_left = true;
                        return Ok(true);
                    }
                }
            }
            until = until.into_iter().chain(idle).min();
        }
        // What a shutdown left pending waits for the step's other calls to
        // stop: nothing answers it any more.
        let pending = !self.shutting_down()
            && running(calls).any(|(_, stream)| stream.asking().pending().is_some());
        let Some(target) = self.inbox_wake.clone().filter(|_| pending) else {
            wake.park(clock.as_ref(), until);
            return Ok(false);
        };
        // A reply or `close` held aside before a finishing turn raised its
        // request again is taken first, as if it came now
        // (`docs/invocation.md`, "Lifecycle").
        let requests: Vec<RequestId> = running(calls)
            .filter_map(|(_, stream)| stream.asking().pending().map(|(request, _)| request))
            .collect();
        if let Some(held) = requests
            .iter()
            .find_map(|request| self.take_held_answer(request))
        {
            return self.take_while_asked(held, calls, turn).map(|()| false);
        }
        wake.forward(Some(target));
        let received = self.receive(until);
        wake.forward(None);
        match received {
            Received::Due => Ok(false),
            Received::Closed => self.inbox_closed(calls, turn).map(|()| false),
            Received::Delivery(delivery) => {
                self.take_while_asked(delivery, calls, turn).map(|()| false)
            }
        }
    }

    /// Every sender is gone, so no answer can come: Fiber declines each
    /// pending interaction.
    fn inbox_closed(&mut self, calls: &Calls<'_>, turn: &TurnId) -> Result<(), Error> {
        for (_, stream) in running(calls) {
            if let Some((request, _)) = stream.asking().pending() {
                self.declined(request, turn)?;
                stream.asking().resolve(Answered::NoAnswer);
            }
        }
        Ok(())
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
        match self.recv_bounded(until) {
            Ok(delivery) => Received::Delivery(delivery),
            Err(RecvTimeoutError::Disconnected) => Received::Closed,
            Err(RecvTimeoutError::Timeout) => Received::Due,
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
        for (_, stream) in running(calls) {
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
    pub(crate) fn declined(&mut self, request: RequestId, turn: &TurnId) -> Result<(), Error> {
        let resolved = InteractionResolved {
            request_id: request,
            by: ResolvedBy::Fiber,
            answer: Answer::Declined { declined: True },
        };
        self.append(&Event::InteractionResolved(resolved), turn, None)
    }
}

#[cfg(test)]
#[path = "interactions_tests.rs"]
mod tests;
