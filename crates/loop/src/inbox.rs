//! One drain for every delivery (`docs/architecture.md`, "One inbox").
//! The idle wait, a step boundary, the end-of-turn check and an approval
//! wait all admit a delivery through the same rules.

use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use contract::commands::Reply;
use contract::events::{Event, ExtensionLog, QueuedMessage, SteeringQueue};
use contract::inbox::{Ack, Delivery, Message, Rejection};
use contract::shapes::ContentPart;
use contract::{CommandId, ErrorCode, RequestId, TurnId};

use crate::cancel::SignalState;
use crate::jobs::Queued;
use crate::{Error, Loop};

mod asked;

/// A turn is running.
const BUSY: &str = "A turn is running; send `steer` to add to it.";

/// `close` has been taken.
pub(crate) const CLOSING: &str = "The session is closing and takes no new turn.";

/// A `steer_drop` names nothing still queued.
pub(crate) const STALE_STEER: &str =
    "That steering message was already applied, or was never queued.";

/// A `reply` names nothing pending.
pub(crate) const STALE_REPLY: &str = "That request is no longer pending.";

/// A `reply`'s keys do not fit the pending request.
pub(crate) const UNFIT_REPLY: &str = "That answer does not fit the pending request.";

/// What the idle drain collected. Every prompt waiting joins the turn as a
/// message, in arrival order (`docs/loop.md`, "Starting a turn").
pub(crate) struct TurnInput {
    /// The turn's input, in arrival order: messages and job notices.
    pub(crate) pieces: Vec<Queued>,
    /// Each prompt's acknowledgement, in arrival order, called once
    /// `turn_started` is written. A steer was already accepted when taken.
    pub(crate) prompts: Vec<Ack>,
    /// The index in `pieces` of each prompt's message, so a `steer_drop`
    /// never removes one.
    pub(crate) prompt_at: Vec<usize>,
}

impl TurnInput {
    /// An input that holds `pieces` and no prompt.
    pub(crate) fn of(pieces: Vec<Queued>) -> Self {
        Self {
            pieces,
            prompts: Vec::new(),
            prompt_at: Vec::new(),
        }
    }
}

/// What an approval wait should do with a delivery it took.
pub(crate) enum Waited {
    /// The delivery was answered. Keep waiting.
    Again,
    /// A reply that names the pending request. Not answered yet.
    Reply(Reply, Ack),
    /// `close` was taken. The pending approval is now unanswerable.
    Closed,
    /// The wake after an accepted cancel, with the signal set. The pending
    /// approval ends denied by the cancel. A clock move uses the same
    /// delivery and does not end the wait unless the signal is set.
    Cancelled,
}

/// What [`Loop::recv_until`] took from the inbox.
#[allow(
    clippy::large_enum_variant,
    reason = "a short-lived return value; boxing would allocate on every delivery"
)]
pub(crate) enum InboxRecv {
    /// A delivery was waiting, or arrived before the deadline.
    Delivery(Delivery),
    /// Every sender is gone.
    Closed,
    /// The idle deadline passed on an empty inbox.
    Idle,
    /// The jobs check came due on an empty inbox; only a wait that checks
    /// the jobs ends this way.
    Unattended,
    /// A cache refresh came due on an empty inbox with no job running; only
    /// a wait given a refresh instant ends this way.
    Warm,
}

impl Loop {
    /// `run` returns once the loop has been idle for `after`. `None` never
    /// expires, which is the default (`docs/invocation.md`, "Lifecycle").
    pub fn idle_exit(mut self, after: Option<Duration>) -> Self {
        self.idle_exit = after;
        self
    }

    /// `clock.now()` when this idle wait began, plus the idle delay. `None`
    /// when the loop does not expire, or the instant cannot be represented.
    pub(crate) fn idle_deadline(&self) -> Option<Instant> {
        self.idle_from(self.log.clock().now())
    }

    /// The idle deadline for a wait whose idle clock starts at `start`:
    /// the instant warming stopped, or now.
    fn idle_from(&self, start: Instant) -> Option<Instant> {
        let after = self.idle_exit?;
        start.checked_add(after)
    }

    /// The next delivery, or why the wait ended. With `warm`, the wait
    /// also ends when that refresh instant comes and no job runs: a job
    /// running means the session is not idle (`docs/invocation.md`,
    /// "Lifecycle"). Anything already queued is
    /// taken before the deadline is checked, so a prompt queued as the
    /// deadline passes still starts its turn. The caller keeps the deadline
    /// from the start of the idle wait; while a job runs there is none, and
    /// a wait that sees the last job end counts the delay from then.
    /// With `check`, the wait also ends when the jobs check comes due
    /// (`docs/invocation.md`, "Lifecycle"). Once a shutdown started, every
    /// wait ends as the idle delay ends it, taking nothing
    /// (`docs/invocation.md`, "Shutdown").
    pub(crate) fn recv_until(
        &mut self,
        deadline: Option<Instant>,
        check: bool,
        warm: Option<Instant>,
    ) -> InboxRecv {
        loop {
            if self.shutting_down() {
                return InboxRecv::Idle;
            }
            match self.inbox.try_recv() {
                Ok(delivery) => return InboxRecv::Delivery(delivery),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return InboxRecv::Closed,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            let clock = Arc::clone(self.log.clock());
            let idle = self.idle_until(deadline);
            let due = if check { self.check_deadline() } else { None };
            if due.is_some_and(|until| clock.now() >= until) {
                return InboxRecv::Unattended;
            }
            let warm = warm.filter(|_| self.running().is_empty());
            if warm.is_some_and(|until| clock.now() >= until) {
                return InboxRecv::Warm;
            }
            // Idle and a refresh need no job running and the check needs
            // one; a warming wait has no idle deadline. So at most one of
            // the three is set.
            let deadline = idle.or(due).or(warm);
            if idle.is_some_and(|until| clock.now() >= until) {
                match self.inbox.try_recv() {
                    Ok(delivery) => return InboxRecv::Delivery(delivery),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return InboxRecv::Closed,
                    Err(std::sync::mpsc::TryRecvError::Empty) => return InboxRecv::Idle,
                }
            }
            let mut slot = None;
            clock.wait_until(deadline, &mut |bound| {
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
                Some(Ok(delivery)) => return InboxRecv::Delivery(delivery),
                Some(Err(RecvTimeoutError::Disconnected)) => return InboxRecv::Closed,
                Some(Err(RecvTimeoutError::Timeout)) | None => {}
            }
        }
    }

    /// Blocks until a prompt or a steer is waiting to start a turn, and
    /// drains everything else already queued. When the session has been
    /// unattended for the idle delay with jobs running, the jobs check
    /// starts the turn instead (`docs/invocation.md`, "Lifecycle"). `None`
    /// once `close` was taken with nothing to start, the idle delay has
    /// passed on an empty inbox, or every sender is gone (`docs/loop.md`,
    /// "Starting a turn").
    pub(crate) fn wait_for_turn(&mut self) -> Result<Option<TurnInput>, Error> {
        // `rewound` is the last line: the process ends, starting no turn.
        if self.rewound {
            return Ok(None);
        }
        if self.closing {
            return self.ending();
        }
        if !self.reraise_offer()? {
            return Ok(None);
        }
        // Idle starts as the wait begins. A rejected command, a dropped
        // steer and a wake do not move it (`docs/invocation.md`, "Lifecycle").
        // While the cache is kept warm the session is not idle: the idle
        // clock starts once warming stops (`docs/prompt-cache.md`,
        // "Warming while idle").
        let start = self.log.clock().now();
        let mut warming = self.warm_stop(start);
        let mut deadline = match warming {
            Some(_) => None,
            None => self.idle_deadline(),
        };
        self.start_unattended();
        // A switch applied at the last turn's top cleared the last
        // request: warming is over and the idle clock starts there.
        if let Some(at) = self.warm_stopped.take() {
            warming = None;
            deadline = self.idle_from(at);
        }
        loop {
            let mut input = TurnInput::of(Vec::new());
            // Deliveries held aside across the finishing turn go first, in
            // arrival order, ahead of the channel: the caller's prompt waits
            // behind that turn instead of being rejected `busy`. When they
            // are here the wait does not block: what is already waiting is
            // drained as when steers were kept.
            let held = !self.deferred.is_empty();
            for delivery in std::mem::take(&mut self.deferred) {
                self.admit_idle(delivery, &mut input)?;
            }
            // Steers the previous turn kept start this one, in order,
            // ahead of whatever is already waiting. This holds after any
            // outcome, not only `interrupted`. While `closing` nothing
            // starts, so steers still queued are not run.
            let kept = self.queued.len();
            let kept_steer = self
                .queued
                .iter()
                .any(|piece| matches!(piece, Queued::Steer(_)));
            input.pieces.extend(self.queued.drain(..));
            if kept_steer {
                // The queue moved into `turn_started`. Only steers are
                // listed on `steering_queue`.
                self.emit_queue(None)?;
            }
            if kept > 0 || held {
                // Kept steers and notices moved into `turn_started`, and
                // held deliveries are already admitted above: what arrived
                // since is drained without blocking or re-announcing the
                // queue, which did not move again.
                for delivery in self.inbox.try_iter().collect::<Vec<_>>() {
                    self.admit_idle(delivery, &mut input)?;
                }
                if let Some(at) = self.warm_stopped.take() {
                    warming = None;
                    deadline = self.idle_from(at);
                }
            } else {
                let due = warming.and_then(|stop| self.warm_due(stop));
                if let (Some(stop), None) = (warming, due) {
                    warming = None;
                    deadline = self.idle_from(stop);
                }
                match self.recv_until(deadline, true, due) {
                    InboxRecv::Delivery(first) => {
                        let mut batch = vec![first];
                        batch.extend(self.inbox.try_iter());
                        for delivery in batch {
                            self.admit_idle(delivery, &mut input)?;
                        }
                        if let Some(at) = self.warm_stopped.take() {
                            warming = None;
                            deadline = self.idle_from(at);
                        }
                    }
                    InboxRecv::Unattended => self.check_jobs(&mut input.pieces),
                    InboxRecv::Warm => {
                        if let Some(stop) = warming
                            && let crate::warm::Refreshed::Stopped(at) = self.refresh(stop)?
                        {
                            warming = None;
                            deadline = self.idle_from(at);
                        }
                    }
                    InboxRecv::Closed | InboxRecv::Idle => return Ok(None),
                }
            }
            if !input.pieces.is_empty() {
                // The repository's offer resolves before the first request;
                // what arrived while it waited joins this turn.
                if !self.offer(None)? {
                    return Ok(None);
                }
                for delivery in std::mem::take(&mut self.deferred) {
                    self.admit_idle(delivery, &mut input)?;
                }
                return Ok(Some(input));
            }
            // `rewound` was written in the drain above: its deliveries
            // are refused, and no turn starts.
            if self.rewound {
                return Ok(None);
            }
            if self.closing {
                return self.ending();
            }
        }
    }

    /// What starts a turn once `close` was taken (`docs/tools.md`,
    /// "Background jobs"): job notices already taken, then, while jobs
    /// still run, the ending notice once, then each job's end as it
    /// arrives. `None` once no job runs and nothing is waiting, or every
    /// sender is gone. Without jobs, `None` at once. Steers still queued
    /// are not run, and no prompt or steer starts a turn.
    fn ending(&mut self) -> Result<Option<TurnInput>, Error> {
        if !self.has_jobs() {
            return Ok(None);
        }
        self.queued
            .retain(|piece| !matches!(piece, Queued::Steer(_)));
        let mut input = TurnInput::of(self.queued.drain(..).collect());
        for delivery in std::mem::take(&mut self.deferred) {
            self.admit_idle(delivery, &mut input)?;
        }
        // Every turn from here on starts after `close`.
        self.ending.after_close = true;
        loop {
            if !input.pieces.is_empty() {
                return Ok(Some(input));
            }
            let running = self.running();
            if running.is_empty() {
                // A job's end is sent before `running` stops listing it, so
                // what is waiting now holds the last ends.
                for delivery in self.inbox.try_iter().collect::<Vec<_>>() {
                    self.admit_idle(delivery, &mut input)?;
                }
                return Ok((!input.pieces.is_empty()).then_some(input));
            }
            if self.notify_pending(running, &mut input.pieces) {
                return Ok(Some(input));
            }
            match self.recv_until(None, false, None) {
                InboxRecv::Delivery(first) => {
                    let mut batch = vec![first];
                    batch.extend(self.inbox.try_iter());
                    for delivery in batch {
                        self.admit_idle(delivery, &mut input)?;
                    }
                }
                InboxRecv::Closed | InboxRecv::Idle | InboxRecv::Unattended | InboxRecv::Warm => {
                    return Ok(None);
                }
            }
        }
    }

    /// Drains the inbox at a step boundary or the end-of-turn check. A steer
    /// taken here is left in `queued` until [`Self::apply_steering`].
    pub(crate) fn drain(&mut self, turn: &TurnId) -> Result<(), Error> {
        let waiting: Vec<Delivery> = self.inbox.try_iter().collect();
        for delivery in waiting {
            self.admit_running(delivery, turn)?;
        }
        Ok(())
    }

    /// Logs everything queued, in arrival order: each steer as
    /// `steering_applied`, each job notice as `job_completed`. Clears the
    /// queue (`docs/loop.md`, "One step").
    pub(crate) fn apply_steering(&mut self, turn: &TurnId) -> Result<(), Error> {
        if self.write_queued(turn)? {
            self.emit_queue(Some(turn))?;
        }
        Ok(())
    }

    /// Writes the queue as it stands: every steering message still queued,
    /// oldest first, empty when the queue is (`docs/events.md`,
    /// `steering_queue`).
    fn emit_queue(&mut self, turn: Option<&TurnId>) -> Result<(), Error> {
        let event = contract::events::Event::SteeringQueue(SteeringQueue {
            messages: self
                .queued
                .iter()
                .filter_map(|piece| match piece {
                    Queued::Steer(message) => Some(QueuedMessage {
                        content: message.content.clone(),
                        sender: message.sender.clone(),
                    }),
                    Queued::Job(_)
                    | Queued::Line(_)
                    | Queued::Handoff(..)
                    | Queued::Pending(..) => None,
                })
                .collect(),
        });
        match turn {
            Some(turn) => self.append(&event, turn, None),
            // Between turns there is no turn to name. The line is
            // ephemeral, so nothing renders it into the conversation.
            None => self
                .log
                .append(&event, None, None)
                .map(|_| ())
                .map_err(crate::Error::Log),
        }
    }

    /// Admits `delivery` while the loop waits for a reply to `pending`.
    pub(crate) fn take_while_waiting(
        &mut self,
        pending: &RequestId,
        delivery: Delivery,
        turn: &TurnId,
    ) -> Result<Waited, Error> {
        // One locked read of the signal decides whether it or the delivery
        // came first; whatever lands after the read (a shutdown an ack
        // starts, say) does not change what it decided. A shutdown leaves
        // the request pending: the delivery is answered as at any drain, a
        // reply `stale_request`, and the wait ends as the idle delay ends it
        // (`docs/invocation.md`, "Shutdown"). A cancel wins the same way and
        // the approval ends denied by it. With the signal live, a reply to
        // the request answers it, and its answer stands whatever lands next.
        match self.cancel.state() {
            SignalState::Shutdown => {
                self.admit_running(delivery, turn)?;
                return Ok(Waited::Again);
            }
            SignalState::Cancelled => {
                self.admit_running(delivery, turn)?;
                return Ok(Waited::Cancelled);
            }
            SignalState::Live => {}
        }
        match delivery {
            Delivery::Reply(reply, ack) if reply.request_id == *pending => {
                Ok(Waited::Reply(reply, ack))
            }
            Delivery::Close(ack) => {
                self.take_close(ack);
                Ok(Waited::Closed)
            }
            // A stale wake from an earlier turn's cancel, or the wake
            // after an accepted cancel read with the signal not set: it
            // carries no meaning and is discarded. A set signal returned
            // above, so this never ends a pending approval.
            other @ (Delivery::Prompt(..)
            | Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Handoff(..)
            | Delivery::Model(..)
            | Delivery::Reply(..)
            | Delivery::Rewind(..)
            | Delivery::Interaction(_)
            | Delivery::Resolved(..)
            | Delivery::Job(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::ExtensionLog(_)
            | Delivery::Cancelled) => {
                self.admit_running(other, turn)?;
                Ok(Waited::Again)
            }
        }
    }

    /// `delivery` while the loop is still collecting a turn's input.
    pub(crate) fn admit_idle(
        &mut self,
        delivery: Delivery,
        input: &mut TurnInput,
    ) -> Result<(), Error> {
        // Once `rewound` is set every later delivery is refused, and
        // nothing is written (`docs/events.md`, "Rewind").
        if self.rewound {
            crate::rewind::command::refuse_after_rewound(delivery);
            return Ok(());
        }
        match delivery {
            Delivery::Prompt(message, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    self.attended();
                    input.prompt_at.push(input.pieces.len());
                    input.pieces.push(Queued::Steer(expanded(self, message)));
                    input.prompts.push(ack);
                }
            }
            Delivery::Steer(message, ack) => {
                // After `close` a steer joins only a turn a message starts.
                if self.closing && !has_message(input) {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    self.attended();
                    accept(ack);
                    input.pieces.push(Queued::Steer(message));
                }
            }
            Delivery::SteerDrop(id, ack) => {
                if drop_piece(input, &id) {
                    accept(ack);
                } else {
                    reject(ack, ErrorCode::StaleRequest, STALE_STEER);
                }
            }
            // Like a steer, a handoff starts a turn while idle: one
            // `handoff` item per command, run at the turn's first step
            // boundary.
            Delivery::Handoff(id, args, ack) => {
                if self.closing && input.pieces.is_empty() {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    accept(ack);
                    input.pieces.push(Queued::Handoff(id, args.instructions));
                }
            }
            // A switch admitted while idle applies at once, before the
            // next `turn_started`.
            Delivery::Model(args, ack) => {
                self.take_switch(args, ack, true)?;
            }
            // Starts a new session that continues this one from an
            // earlier point, once, while idle (`docs/events.md`,
            // "Rewind").
            Delivery::Rewind(args, ack) => {
                self.take_rewind(args, ack, input)?;
            }
            Delivery::Reply(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_REPLY),
            Delivery::Close(ack) => self.take_close(ack),
            // A stale wake from an earlier turn's cancel: it carries no
            // meaning while idle.
            Delivery::Cancelled => {}
            // After `close`, news starts a turn only while the loop waits
            // for the session's jobs; without them the claim is left
            // untaken.
            Delivery::Job(notice) => {
                if !(self.closing && input.pieces.is_empty() && !self.has_jobs())
                    && let Some(completed) = crate::jobs::claimed(notice)
                {
                    input.pieces.push(Queued::Job(completed));
                }
            }
            Delivery::JobLine(line) => {
                if !(self.closing && input.pieces.is_empty() && !self.has_jobs()) {
                    input.pieces.push(Queued::Line(line));
                }
            }
            // Written at the drain that takes it, idle or not: it never
            // starts a turn and never joins the model's input.
            Delivery::ExtensionExec(exec) => {
                self.log.append(&Event::ExtensionExec(exec), None, None)?;
            }
            Delivery::Interaction(requested) => self.record_interaction(requested)?,
            Delivery::Resolved(resolved, ack) => self.record_resolved(resolved, ack)?,
            Delivery::ExtensionLog(entry) => self.record_extension_log(entry)?,
        }
        Ok(())
    }

    /// Writes an `extension_log` delivery live and to the diagnostic log.
    /// The bound event goes to the log first, as the drains did before,
    /// and lends the diagnostic line its fields, so no clone is kept.
    pub(crate) fn record_extension_log(&self, entry: ExtensionLog) -> Result<(), Error> {
        let event = Event::ExtensionLog(entry);
        self.log.append(&event, None, None)?;
        if let Event::ExtensionLog(entry) = &event {
            self.diag.extension_log(&entry.extension, &entry.message);
        }
        Ok(())
    }

    /// `delivery` while a turn is in flight and nothing is pending.
    pub(crate) fn admit_running(&mut self, delivery: Delivery, turn: &TurnId) -> Result<(), Error> {
        match delivery {
            Delivery::Prompt(_, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(ack, ErrorCode::Busy, BUSY);
                }
            }
            // A turn started after `close` takes no steer; the turn in
            // flight when it arrived still does.
            Delivery::Steer(_, ack) if self.ending.after_close => {
                reject(ack, ErrorCode::Closing, CLOSING);
            }
            Delivery::Steer(message, ack) => {
                self.attended();
                accept(ack);
                self.queued.push_back(Queued::Steer(message));
                self.emit_queue(Some(turn))?;
            }
            Delivery::SteerDrop(id, ack) => {
                if drop_queued(&mut self.queued, &id) {
                    accept(ack);
                    self.emit_queue(Some(turn))?;
                } else {
                    reject(ack, ErrorCode::StaleRequest, STALE_STEER);
                }
            }
            // Held until the next step boundary; not listed on
            // `steering_queue`, which holds steering messages only.
            Delivery::Handoff(id, args, ack) => {
                accept(ack);
                self.queued
                    .push_back(Queued::Handoff(id, args.instructions));
            }
            // A switch admitted during a turn waits for the next turn
            // boundary. While `deferred` still holds one, a live one
            // waits behind it in arrival order.
            Delivery::Model(args, ack) => {
                if self
                    .deferred
                    .iter()
                    .any(|held| matches!(held, Delivery::Model(..)))
                {
                    self.deferred.push_back(Delivery::Model(args, ack));
                } else {
                    self.take_switch(args, ack, false)?;
                }
            }
            // While a turn runs the session cannot close first: rewind
            // once it ends.
            Delivery::Rewind(_, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(ack, ErrorCode::Busy, crate::rewind::command::TURN_RUNNING);
                }
            }
            Delivery::Reply(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_REPLY),
            Delivery::Close(ack) => self.take_close(ack),
            // A stale wake from an earlier turn's cancel: it carries no
            // meaning at a drain.
            Delivery::Cancelled => {}
            Delivery::Job(notice) => self.admit_job(notice),
            // Held until the next step boundary, as a job's end is.
            Delivery::JobLine(line) => self.queued.push_back(Queued::Line(line)),
            // Written at the drain that takes it: it never starts a turn
            // and never joins the model's input.
            Delivery::ExtensionExec(exec) => {
                self.log.append(&Event::ExtensionExec(exec), None, None)?;
            }
            Delivery::Interaction(requested) => self.record_interaction(requested)?,
            Delivery::Resolved(resolved, ack) => self.record_resolved(resolved, ack)?,
            Delivery::ExtensionLog(entry) => self.record_extension_log(entry)?,
        }
        Ok(())
    }

    /// Accepts `close`. No later turn starts, and no later approval can be
    /// answered (`docs/permissions.md`, "Headless").
    pub(crate) fn take_close(&mut self, ack: Ack) {
        accept(ack);
        self.closing = true;
        self.answerable = false;
    }
}

/// Accepts a command that answers nothing.
pub(crate) fn accept(ack: Ack) {
    (ack.0)(Ok(None));
}

/// Rejects a command. The sentence is Fiber's own
/// (`docs/invocation.md`, "Driver commands").
pub(crate) fn reject(ack: Ack, code: ErrorCode, message: &str) {
    (ack.0)(Err(Rejection {
        code,
        message: message.to_owned(),
    }));
}

/// A `prompt` whose first word is `/name` runs that skill with the rest
/// as its arguments (`docs/invocation.md`, "Driver commands"). The
/// expansion never fails the prompt: `None` sends it as written. Only a
/// first word starting with `/` reads the skill directories, so any other
/// prompt sends no filesystem read.
fn expanded(looped: &Loop, message: Message) -> Message {
    let slash = matches!(
        message.content.first(),
        Some(ContentPart::Text { text }) if crate::skills::split_command(text).is_some()
    );
    if !slash {
        return message;
    }
    // The repository's top level, as the opening message reads it
    // (`opening::collect`): a skill under a parent repository is found
    // from a subdirectory workspace.
    let (chain, _) = crate::opening::repo_chain(&looped.workspace);
    let top = chain.first().unwrap_or(&looped.workspace);
    let mut message = message;
    if let Some(content) = crate::skills::expand(&looped.prompt, top, &message.content) {
        message.content = content;
    }
    message
}

/// Whether `input` holds a message, not only job notices.
fn has_message(input: &TurnInput) -> bool {
    input
        .pieces
        .iter()
        .any(|piece| matches!(piece, Queued::Steer(_)))
}

/// Removes the unapplied steer `id` names. A prompt's message is not a
/// steer, wherever it sits, so it is skipped.
fn drop_piece(input: &mut TurnInput, id: &CommandId) -> bool {
    let found = input.pieces.iter().enumerate().position(|(index, piece)| {
        matches!(piece, Queued::Steer(message) if message.sender.command_id.as_ref() == Some(id))
            && !input.prompt_at.contains(&index)
    });
    let Some(index) = found else {
        return false;
    };
    input.pieces.remove(index);
    shift_past(&mut input.prompt_at, index);
    true
}

/// Moves each prompt index past the removed piece at `removed` down by one.
// `removed` is a steer's index, never a prompt's, so a mutant of `>` to `>=`
// changes nothing; the shift itself is tested through `drop_piece`.
#[cfg_attr(false, mutants::skip)]
fn shift_past(prompt_at: &mut [usize], removed: usize) {
    for at in prompt_at {
        if *at > removed {
            *at -= 1;
        }
    }
}

/// Removes the unapplied steer `id` names from `queued`.
fn drop_queued(queued: &mut std::collections::VecDeque<Queued>, id: &CommandId) -> bool {
    queued
        .iter()
        .position(
            |piece| matches!(piece, Queued::Steer(message) if message.sender.command_id.as_ref() == Some(id)),
        )
        .map(|index| queued.remove(index))
        .is_some()
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
