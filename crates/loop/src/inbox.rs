//! One drain for every delivery (`docs/architecture.md`, "One inbox").
//! The idle wait, a step boundary, the end-of-turn check and an approval
//! wait all admit a delivery through the same rules.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use contract::commands::AddDirectoryArgs;
use contract::commands::Reply;
use contract::emit::Emit;
use contract::events::DirectoryAdded;
use contract::events::{Event, ExtensionExec, ExtensionLog, ShellCommand, UsageRecorded};
use contract::inbox::{Ack, Delivery, Message, Rejection};
use contract::steering;
use contract::{CommandId, ErrorCode, RequestId, TurnId};

use crate::before_message::Hooked;
use crate::cancel::SignalState;
use crate::jobs::Queued;
use crate::{Error, Loop};

mod asked;
pub(crate) mod queue;
pub(crate) mod session_message;

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
    ) -> Result<InboxRecv, Error> {
        loop {
            if self.shutting_down() {
                return Ok(InboxRecv::Idle);
            }
            match self.inbox.try_recv() {
                Ok(delivery) => return Ok(InboxRecv::Delivery(delivery)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(InboxRecv::Closed),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            let running = self.running();
            self.record_waiting_extension_jobs()?;
            let clock = Arc::clone(self.log.clock());
            let model = self.model_running(running);
            let idle = self.idle_until(deadline, &model);
            let due = if check {
                self.check_deadline(&model)
            } else {
                None
            };
            if due.is_some_and(|until| clock.now() >= until) {
                return Ok(InboxRecv::Unattended);
            }
            let warm = warm.filter(|_| model.is_empty());
            if warm.is_some_and(|until| clock.now() >= until) {
                return Ok(InboxRecv::Warm);
            }
            // Idle and a refresh need no job running and the check needs
            // one; a warming wait has no idle deadline. So at most one of
            // the three is set.
            let deadline = idle.or(due).or(warm);
            if idle.is_some_and(|until| clock.now() >= until) {
                match self.last_idle_check() {
                    Ok(delivery) => return Ok(InboxRecv::Delivery(delivery)),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        return Ok(InboxRecv::Closed);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(InboxRecv::Idle),
                }
            }
            match self.recv_bounded(deadline) {
                Ok(delivery) => return Ok(InboxRecv::Delivery(delivery)),
                Err(RecvTimeoutError::Disconnected) => return Ok(InboxRecv::Closed),
                Err(RecvTimeoutError::Timeout) => {}
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
        // `rewound` or `moved` is the last line: the process ends,
        // starting no turn.
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
        if let Some(at) = self.warming.take_stopped() {
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
            let deferred = std::mem::take(&mut self.deferred);
            self.admit_idle_batch(deferred, &mut input)?;
            // Steers the previous turn kept start this one, in order,
            // ahead of whatever is already waiting. This holds after any
            // outcome, not only `interrupted`. While `closing` nothing
            // starts, so steers still queued are not run.
            // The claim is one section, as the apply claims it: the
            // staged entries leave the queue and the queue emits as the
            // section left it before anything selects holders. Only the
            // snapshot's holders start: a drop before the claim leaves
            // the holder with no entry, and the holder is skipped, so a
            // message dropped before the step boundary is never applied.
            // Only steers are listed on `steering_queue`.
            let emit = TurnEmit {
                log: &self.log,
                turn: None,
            };
            let taken = self.steering.take_staged(&emit);
            let queued = self.take_queued();
            let steering = &self.steering;
            let rest: Vec<Queued> = queued
                .into_iter()
                .filter(|piece| match piece {
                    Queued::Steer(message, _) => holder_kept(&taken, steering, message),
                    Queued::Job(..)
                    | Queued::Notice(..)
                    | Queued::Line(_)
                    | Queued::Handoff(..)
                    | Queued::Pending(..) => true,
                })
                .collect();
            let kept = rest.len();
            input.pieces.extend(rest);
            if kept > 0 || held {
                if let Some(hooks) = self.hooks.clone() {
                    self.drain_state(&hooks, None)?;
                }
                // Kept steers and notices moved into `turn_started`, and
                // held deliveries are already admitted above: what arrived
                // since is drained without blocking or re-announcing the
                // queue, which did not move again.
                let waiting = self.inbox.try_iter();
                self.admit_idle_batch(waiting, &mut input)?;
                if let Some(at) = self.warming.take_stopped() {
                    warming = None;
                    deadline = self.idle_from(at);
                }
            } else {
                let due = warming.and_then(|stop| self.warm_due(stop));
                if let (Some(stop), None) = (warming, due) {
                    warming = None;
                    deadline = self.idle_from(stop);
                }
                match self.recv_until(deadline, true, due)? {
                    InboxRecv::Delivery(first) => {
                        if let Some(hooks) = self.hooks.clone() {
                            self.drain_state(&hooks, None)?;
                        }
                        let batch = std::iter::once(first).chain(self.inbox.try_iter());
                        self.admit_idle_batch(batch, &mut input)?;
                        if let Some(at) = self.warming.take_stopped() {
                            warming = None;
                            deadline = self.idle_from(at);
                        }
                    }
                    InboxRecv::Unattended => self.check_jobs(&mut input)?,
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
                // what arrived while it waited joins this turn. A session
                // message in the input keeps the session from being idle
                // while the offer resolves (`docs/invocation.md`, "Lifecycle").
                self.message_waiting = session_message::has_session_message(&input.pieces);
                let offered = self.offer(None)?;
                self.message_waiting = false;
                if !offered {
                    return Ok(None);
                }
                let deferred = std::mem::take(&mut self.deferred);
                self.admit_idle_batch(deferred, &mut input)?;
                // A turn starting after `close` on a session message's back
                // takes no person steer, as one `ending` starts does
                // (`docs/tools.md`, "Messaging other sessions").
                if self.closing && session_message::has_session_message(&input.pieces) {
                    self.ending.after_close = true;
                }
                self.forget_input(&input);
                return Ok(Some(input));
            }
            // `rewound` or `moved` was written in the drain above: its
            // deliveries are refused, and no turn starts.
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
        // A session message still queued runs its turn after `close`; a
        // person's steer never does (`docs/tools.md`, "Messaging other sessions").
        self.queued.retain(|piece| {
            !matches!(piece, Queued::Steer(message, _) if !session_message::is_session(message))
        });
        // Session messages move into the turn's input first, ahead of
        // whatever is already waiting, as kept steers do in
        // `wait_for_turn`. Person steers still queued are dropped.
        // `take_for_ending` emits the empty queue when a message moved.
        let emit = TurnEmit {
            log: &self.log,
            turn: None,
        };
        let messages = self.steering.take_for_ending(&emit);
        let mut input = TurnInput::of(messages.into_iter().map(queued_piece).collect());
        // Message holders are already above as entries; drop them so no
        // message joins twice.
        let rest: Vec<Queued> = self
            .take_queued()
            .into_iter()
            .filter(|piece| match piece {
                Queued::Steer(message, _) => message.sender.command_id.is_none(),
                Queued::Job(..)
                | Queued::Notice(..)
                | Queued::Line(_)
                | Queued::Handoff(..)
                | Queued::Pending(..) => true,
            })
            .collect();
        input.pieces.extend(rest);
        let deferred = std::mem::take(&mut self.deferred);
        self.admit_idle_batch(deferred, &mut input)?;
        // Every turn from here on starts after `close`.
        self.ending.after_close = true;
        if session_message::has_session_message(&input.pieces) {
            self.forget_input(&input);
            return Ok(Some(input));
        }
        if !self.has_jobs() {
            return Ok(None);
        }
        loop {
            if !input.pieces.is_empty() {
                self.forget_input(&input);
                return Ok(Some(input));
            }
            let waiting = self.inbox.try_iter();
            self.admit_idle_batch(waiting, &mut input)?;
            let running = self.running();
            self.record_waiting_extension_jobs()?;
            let running = self.model_running(running);
            if !input.pieces.is_empty() {
                self.forget_input(&input);
                return Ok(Some(input));
            }
            if running.is_empty() {
                // A job's end is sent before `running` stops listing it, so
                // what is waiting now holds the last ends.
                let waiting = self.inbox.try_iter();
                self.admit_idle_batch(waiting, &mut input)?;
                if input.pieces.is_empty() {
                    return Ok(None);
                }
                self.forget_input(&input);
                return Ok(Some(input));
            }
            if self.notify_pending(running, &mut input.pieces) {
                self.forget_input(&input);
                return Ok(Some(input));
            }
            match self.recv_until(None, false, None)? {
                InboxRecv::Delivery(first) => {
                    let batch = std::iter::once(first).chain(self.inbox.try_iter());
                    self.admit_idle_batch(batch, &mut input)?;
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
        if let Some(hooks) = self.hooks.clone() {
            self.drain_state(&hooks, Some(turn))?;
        }
        let waiting = self.inbox.try_iter();
        let mut pending = self.inbox.pending(waiting);
        while let Some(delivery) = pending.next() {
            self.admit_running(delivery, turn)?;
        }
        // Commands held by an approval or interaction wait start only at
        // this step boundary; switches keep their place for the next turn.
        let held = std::mem::take(&mut self.deferred);
        for delivery in held {
            if let Delivery::InnerCall(request) = delivery {
                self.run_alone(request)?;
            } else {
                self.deferred.push_back(delivery);
            }
        }
        Ok(())
    }

    /// Logs everything queued, in arrival order: each steer as
    /// `steering_applied`, each job notice as `job_completed`. Clears the
    /// queue (`docs/loop.md`, "One step"). The apply first claims its
    /// snapshot in one section: the staged entries leave the queue and
    /// the queue emits as the section left it, with the lock released
    /// before anything logs. Only the snapshot's holders apply: a drop
    /// before the claim leaves the holder with no entry, and the apply
    /// skips it, so a drop the reader already acknowledged never logs.
    pub(crate) fn apply_steering(&mut self, turn: &TurnId) -> Result<(), Error> {
        let emit = TurnEmit {
            log: &self.log,
            turn: Some(turn.clone()),
        };
        let taken = self.steering.take_staged(&emit);
        let steering = &self.steering;
        self.queued.retain(|piece| match piece {
            Queued::Steer(message, _) => holder_kept(&taken, steering, message),
            Queued::Job(..)
            | Queued::Notice(..)
            | Queued::Line(_)
            | Queued::Handoff(..)
            | Queued::Pending(..) => true,
        });
        self.write_queued(turn)?;
        Ok(())
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
            | Delivery::Name(..)
            | Delivery::AddDirectory(..)
            | Delivery::Message(..)
            | Delivery::Handoff(..)
            | Delivery::Model(..)
            | Delivery::Reload(..)
            | Delivery::McpReconnect(..)
            | Delivery::Credential(..)
            | Delivery::Reply(..)
            | Delivery::Rewind(..)
            | Delivery::ChangeWorkspace(..)
            | Delivery::Interaction(_)
            | Delivery::Resolved(..)
            | Delivery::Job(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::ExtensionLog(_)
            | Delivery::ExtensionJob(..)
            | Delivery::ProposeNotes(..)
            | Delivery::ExtensionUsage(_)
            | Delivery::DelegateUsage { .. }
            | Delivery::InnerCall(_)
            | Delivery::Cancelled
            | Delivery::Shell(..)) => {
                self.admit_running(other, turn)?;
                Ok(Waited::Again)
            }
        }
    }

    /// Keeps the unhandled snapshot registered while any command starts
    /// synchronous work. Nested busy drains must not take its remainder,
    /// but the reconnect collector must still see it.
    // debt: raw receivers remain for waits that cannot start reconnects;
    // require leased admission before adding another starting consumer.
    fn admit_idle_batch(
        &mut self,
        deliveries: impl IntoIterator<Item = Delivery>,
        input: &mut TurnInput,
    ) -> Result<(), Error> {
        let mut pending = self.inbox.pending(deliveries);
        while let Some(delivery) = pending.next() {
            self.admit_idle(delivery, input)?;
        }
        Ok(())
    }

    /// `delivery` while the loop is still collecting a turn's input.
    pub(crate) fn admit_idle(
        &mut self,
        delivery: Delivery,
        input: &mut TurnInput,
    ) -> Result<(), Error> {
        // Once `rewound` is set every later delivery is refused, and
        // nothing is written (`docs/events.md`, "Rewind"). A
        // reader-admitted steer is removed with its delivery, so no
        // orphaned entry outlives the session.
        if self.rewound {
            if let Delivery::Steer(message, _) = &delivery
                && let Some(id) = &message.sender.command_id
            {
                let emit = TurnEmit {
                    log: &self.log,
                    turn: None,
                };
                let _ = self.steering.take(id, &emit);
            }
            crate::rewind::command::refuse_after_rewound(delivery);
            return Ok(());
        }
        match delivery {
            Delivery::ProposeNotes(ask, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    self.take_notes(ask, ack)?;
                }
            }
            Delivery::InnerCall(request) => self.run_alone(request)?,
            Delivery::Prompt(message, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    self.take_prompt(message, ack, input)?;
                }
            }
            Delivery::Steer(message, ack) => {
                // After `close` a steer joins only a turn a message starts.
                if self.closing && !has_message(input) {
                    // A reader-admitted steer leaves with its delivery,
                    // so no orphaned entry outlives the session.
                    if let Some(id) = &message.sender.command_id {
                        let emit = TurnEmit {
                            log: &self.log,
                            turn: None,
                        };
                        let _ = self.steering.take(id, &emit);
                    }
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    self.take_steer_idle(message, ack, input)?;
                }
            }
            Delivery::SteerDrop(id, ack) => {
                if drop_piece(input, &id) || self.remove_steer(&id, None) {
                    accept(ack);
                } else {
                    reject(ack, ErrorCode::StaleRequest, STALE_STEER);
                }
            }
            // A session message joins the turn's input, after `close` too.
            Delivery::Message(message, ack) => self.admit_message_idle(message, ack, input)?,
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
                self.take_switch(crate::switch::Asked::Model(args), ack, true)?;
            }
            Delivery::Credential(args, ack) => {
                self.take_switch(crate::switch::Asked::Credential(args), ack, true)?;
            }
            Delivery::Reload(ack) => self.take_reload(ack)?,
            // Restarts only the server it names, as a reload restarts
            // its servers (`docs/mcp.md`, "Servers, their state and the
            // per-server commands").
            Delivery::McpReconnect(args, ack) => self.take_reconnect(args.server, ack)?,
            // The person's name starts no turn, so it is taken after
            // `close` too: written, pinned and acked at this drain.
            Delivery::Name(args, ack) => self.take_name(args, ack)?,
            // An added folder starts no turn either: resolved, written
            // unless covered and acked at this drain, after `close` too.
            Delivery::AddDirectory(args, ack) => self.take_add_directory(args, ack)?,
            // A sent shell joins the next turn's input: written at this
            // drain, held for the next `turn_started`, starting no turn.
            Delivery::Shell(line, ack) => self.take_shell(line, ack)?,
            // Starts a new session that continues this one from an
            // earlier point, once, while idle (`docs/events.md`,
            // "Rewind").
            Delivery::Rewind(id, args, ack) => {
                self.take_rewind(id, args, ack, input)?;
            }
            // Starts a new session that continues this one in another
            // workspace, once, while idle (`docs/events.md`, "Change
            // workspace").
            Delivery::ChangeWorkspace(id, args, ack) => {
                self.take_change_workspace(id, args, ack, input)?;
            }
            Delivery::Reply(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_REPLY),
            Delivery::Close(ack) => self.take_close(ack),
            // A stale wake from an earlier turn's cancel: it carries no
            // meaning while idle.
            Delivery::Cancelled => {}
            // After `close`, news starts a turn only while the loop waits
            // for the session's jobs; without them the claim is left
            // untaken. The claim is asked here, at admission: a refused
            // claim starts no turn.
            Delivery::Job(notice) if self.handoff.carry.owned(&notice.completed.job_id.0) => {
                self.end_extension_job(notice)?;
            }
            Delivery::Job(notice) => {
                if self.closing && input.pieces.is_empty() && !self.has_jobs() {
                    // Left untaken: no pass, as the claim is never asked.
                } else {
                    self.redact_output(&notice.completed, None)?;
                    if let Some((ahead, completed, delegate)) = crate::jobs::claimed(notice) {
                        input.pieces.push(Queued::Job(ahead, completed, delegate));
                    }
                }
            }
            Delivery::JobLine(line) => {
                if !(self.closing && input.pieces.is_empty() && !self.has_jobs()) {
                    input.pieces.push(Queued::Line(line));
                }
            }
            Delivery::ExtensionExec(exec) => self.record_extension_exec(exec)?,
            Delivery::ExtensionJob(started, delegate) => {
                self.record_extension_job(started, delegate)?
            }
            Delivery::ExtensionUsage(line) => self.record_extension_usage(line)?,
            Delivery::DelegateUsage { session, line } => self.copy_usage(&session, line)?,
            Delivery::Interaction(requested) => self.record_interaction(requested)?,
            Delivery::Resolved(resolved, ack) => self.record_resolved(resolved, ack)?,
            Delivery::ExtensionLog(entry) => self.record_extension_log(entry)?,
        }
        Ok(())
    }

    /// Waits on the loop's clock until `until` for the next queued
    /// delivery. `None` blocks until a delivery arrives or the channel
    /// closes. A wait the clock ends without calling the receive closure
    /// maps to `Err(Timeout)`, as the idle wait treats it as another
    /// turn of the loop and the step's receive as its wait ending.
    pub(crate) fn recv_bounded(
        &mut self,
        until: Option<Instant>,
    ) -> Result<Delivery, RecvTimeoutError> {
        let clock = Arc::clone(self.log.clock());
        let mut slot = None;
        clock.wait_until(until, &mut |bound| {
            // `None` blocks until a delivery or the channel closes.
            slot = Some(match bound {
                None => self
                    .inbox
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
                Some(limit) => {
                    #[allow(
                        clippy::disallowed_methods,
                        reason = "the bound the injected clock's wait_until hands this closure"
                    )]
                    let got = self.inbox.recv_timeout(limit);
                    got
                }
            });
        });
        slot.unwrap_or(Err(RecvTimeoutError::Timeout))
    }

    /// Writes an `extension_exec` delivery at the drain that takes it:
    /// it never starts a turn and never joins the model's input.
    pub(crate) fn record_extension_exec(&self, exec: ExtensionExec) -> Result<(), Error> {
        self.log.append(&Event::ExtensionExec(exec), None, None)?;
        Ok(())
    }

    /// Writes an extension's `host.model` usage line: recorded in the log
    /// and folded into the ledger, with no turn and no action.
    pub(crate) fn record_extension_usage(&mut self, line: UsageRecorded) -> Result<(), Error> {
        self.write(&Event::UsageRecorded(line.clone()), None, None)?;
        self.ledger.record(&line);
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
            Delivery::ProposeNotes(_, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(ack, ErrorCode::Busy, BUSY);
                }
            }
            Delivery::InnerCall(request) => self.deferred.push_back(Delivery::InnerCall(request)),
            Delivery::Prompt(_, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(ack, ErrorCode::Busy, BUSY);
                }
            }
            // A turn started after `close` takes no steer; the turn in
            // flight when it arrived still does. A reader-admitted steer
            // leaves with its delivery, so no orphaned entry outlives
            // the session.
            Delivery::Steer(message, ack) if self.ending.after_close => {
                if let Some(id) = &message.sender.command_id {
                    let emit = TurnEmit {
                        log: &self.log,
                        turn: None,
                    };
                    let _ = self.steering.take(id, &emit);
                }
                reject(ack, ErrorCode::Closing, CLOSING);
            }
            Delivery::Steer(message, ack) => {
                self.take_steer_running(message, ack, turn)?;
            }
            Delivery::SteerDrop(id, ack) => {
                if self.remove_steer(&id, Some(turn)) {
                    accept(ack);
                } else if self.steering.is_admitted(&id) {
                    // Already dropped or a duplicate delivery: the
                    // reader's answer stands, nothing left to do.
                    accept(ack);
                } else {
                    reject(ack, ErrorCode::StaleRequest, STALE_STEER);
                }
            }
            // A session message queues for the next step boundary, in an
            // after-`close` turn too.
            Delivery::Message(message, ack) => self.admit_message_running(message, ack, turn)?,
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
                if self.has_deferred_switch() {
                    self.deferred.push_back(Delivery::Model(args, ack));
                } else {
                    self.take_switch(crate::switch::Asked::Model(args), ack, false)?;
                }
            }
            Delivery::Credential(args, ack) => {
                if self.has_deferred_switch() {
                    self.deferred.push_back(Delivery::Credential(args, ack));
                } else {
                    self.take_switch(crate::switch::Asked::Credential(args), ack, false)?;
                }
            }
            Delivery::McpReconnect(_, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(
                        ack,
                        ErrorCode::Busy,
                        "A turn is running; reconnect once it ends.",
                    );
                }
            }
            Delivery::Reload(ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(
                        ack,
                        ErrorCode::Busy,
                        "A turn is running; reload once it ends.",
                    );
                }
            }
            // The person's name starts no turn, so it is taken after
            // `close` too: written, pinned and acked at this drain.
            Delivery::Name(args, ack) => self.take_name(args, ack)?,
            // An added folder joins the running turn's boundary at once:
            // resolved, written unless covered and acked at this drain.
            Delivery::AddDirectory(args, ack) => self.take_add_directory(args, ack)?,
            // A sent shell joins the next turn's input, never the running
            // turn: written at this drain and held for the next
            // `turn_started`.
            Delivery::Shell(line, ack) => self.take_shell(line, ack)?,
            // While a turn runs the session cannot close first: rewind
            // once it ends.
            Delivery::Rewind(_, _, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(ack, ErrorCode::Busy, crate::rewind::command::TURN_RUNNING);
                }
            }
            // While a turn runs the session cannot close first: move
            // once it ends.
            Delivery::ChangeWorkspace(_, _, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(ack, ErrorCode::Busy, crate::rewind::workspace::TURN_RUNNING);
                }
            }
            Delivery::Reply(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_REPLY),
            Delivery::Close(ack) => self.take_close(ack),
            // A stale wake from an earlier turn's cancel: it carries no
            // meaning at a drain.
            Delivery::Cancelled => {}
            Delivery::Job(notice) if self.handoff.carry.owned(&notice.completed.job_id.0) => {
                self.end_extension_job(notice)?;
            }
            Delivery::Job(notice) => self.admit_job(notice, turn)?,
            // Held until the next step boundary, as a job's end is.
            Delivery::JobLine(line) => self.queued.push_back(Queued::Line(line)),
            Delivery::ExtensionExec(exec) => self.record_extension_exec(exec)?,
            Delivery::ExtensionJob(started, delegate) => {
                self.record_extension_job(started, delegate)?
            }
            Delivery::ExtensionUsage(line) => self.record_extension_usage(line)?,
            Delivery::DelegateUsage { session, line } => self.copy_usage(&session, line)?,
            Delivery::Interaction(requested) => self.record_interaction(requested)?,
            Delivery::Resolved(resolved, ack) => self.record_resolved(resolved, ack)?,
            Delivery::ExtensionLog(entry) => self.record_extension_log(entry)?,
        }
        Ok(())
    }

    /// Whether `deferred` holds a switch still waiting for the finishing
    /// turn: a live switch of either kind waits behind it in arrival order.
    fn has_deferred_switch(&self) -> bool {
        self.deferred
            .iter()
            .any(|held| matches!(held, Delivery::Model(..) | Delivery::Credential(..)))
    }

    /// Accepts `close`. No later turn starts, and no later approval can be
    /// answered (`docs/permissions.md`, "Headless"). The acknowledgement
    /// waits until the `session_status` carrying `closing` is what a new
    /// subscriber reads, or the status observer has ended.
    pub(crate) fn take_close(&mut self, ack: Ack) {
        self.closing = true;
        self.answerable = false;
        if let Some(tap) = &self.status_tap {
            tap.closing();
        }
        accept(ack);
    }

    /// Takes a sent driver shell: written as `shell_command` at this drain,
    /// then accepted with no result, starting no turn. A failed append
    /// returns before acknowledging, as every other take does.
    pub(crate) fn take_shell(&mut self, line: ShellCommand, ack: Ack) -> Result<(), Error> {
        let event = Event::ShellCommand(line);
        self.write(&event, None, None)?;
        accept(ack);
        Ok(())
    }

    /// Takes an `add_directory` command: resolves the path against the
    /// workspace, requires an existing directory outside a metadata
    /// tree, and writes it as `directory_added` unless the effective
    /// boundary already covers it, accepted with no event then. The
    /// write is fsynced before the ack runs, so a later call judges
    /// against the widened boundary. A failed append returns before
    /// folding or acknowledging, as every other take does.
    pub(crate) fn take_add_directory(
        &mut self,
        args: AddDirectoryArgs,
        ack: Ack,
    ) -> Result<(), Error> {
        match self.resolve_added(Path::new(&args.path)) {
            Ok(None) => accept(ack),
            Ok(Some(resolved)) => {
                self.record_added(&resolved, false)?;
                accept(ack);
            }
            Err(message) => reject(ack, ErrorCode::InvalidArguments, &message),
        }
        Ok(())
    }

    /// Writes one added folder as its `directory_added` event and tracks
    /// it, in that order: a later call judges against it only once its
    /// line is durable. A failed append returns before tracking, as
    /// every other take does.
    fn record_added(&mut self, resolved: &Path, inherited: bool) -> Result<(), Error> {
        let event = Event::DirectoryAdded(DirectoryAdded {
            path: resolved.display().to_string(),
            inherited,
        });
        self.write(&event, None, None)?;
        self.changes.track_added(resolved);
        Ok(())
    }

    /// Seeds a delegate's loop from its inherited parent roots: each
    /// root resolves exactly as an `add_directory` path does, and each
    /// resolved, non-covered root is written as the child's own
    /// `directory_added` before the opening (`docs/delegates.md`, "The
    /// tools"). A fresh spawn marks these as inherited because its
    /// opening lists them. A fork marks a root added after its point as
    /// a normal addition, whose message follows the prefix. A covered
    /// root writes nothing, and a removed root is skipped: it permits
    /// nothing.
    pub(crate) fn seed_inherited(
        &mut self,
        inherited: Vec<std::path::PathBuf>,
        is_fresh_spawn: bool,
    ) -> Result<(), Error> {
        for root in inherited {
            if let Ok(Some(resolved)) = self.resolve_added(&root) {
                self.record_added(&resolved, is_fresh_spawn)?;
            }
        }
        Ok(())
    }

    /// Resolves `raw` against the workspace with symlinks resolved: the
    /// folder's path when it names an existing directory outside the
    /// effective boundary and no metadata tree, `None` when the boundary
    /// already covers it, and a rejection sentence otherwise. A covered
    /// metadata path stays an accepted no-op; only a folder the command
    /// would add is rejected for sitting at or below `.git` or `.fiber`.
    fn resolve_added(&self, raw: &Path) -> Result<Option<std::path::PathBuf>, String> {
        let joined: std::path::PathBuf = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.workspace.join(raw)
        };
        let resolved = joined
            .canonicalize()
            .map_err(|_| NOT_A_DIRECTORY.to_owned())?;
        if !resolved.is_dir() {
            return Err(NOT_A_DIRECTORY.to_owned());
        }
        if covered(&resolved, &self.workspace, self.changes.added()) {
            return Ok(None);
        }
        if super::permission::is_metadata(&resolved) {
            return Err(METADATA.to_owned());
        }
        Ok(Some(resolved))
    }
}

/// Whether `resolved` sits inside the effective boundary: the workspace
/// or one of the session's added folders. Both sides are resolved, so a
/// symlink inside the boundary that points out is not covered.
fn covered(
    resolved: &std::path::Path,
    workspace: &std::path::Path,
    added: &[std::path::PathBuf],
) -> bool {
    resolved.starts_with(workspace)
        || added
            .iter()
            .any(|root| resolved == root || resolved.starts_with(root))
}

/// An `add_directory` path that names nothing usable.
const NOT_A_DIRECTORY: &str = "That path names no directory to add.";

/// An `add_directory` path at or below a metadata directory: such a
/// folder never joins the boundary.
const METADATA: &str = "That path sits at or under a metadata directory.";

/// The loop's steering-queue emitter: appends about the drain or boundary
/// that reconciled, inside the shared queue's lock section. A poisoned log
/// ends the loop at its next durable write: the line is ephemeral, so
/// nothing durable is lost by dropping it here.
pub(crate) struct TurnEmit<'a> {
    pub(crate) log: &'a std::sync::Arc<log::Log>,
    pub(crate) turn: Option<TurnId>,
}

impl Emit for TurnEmit<'_> {
    fn emit(&self, event: &Event) {
        match self.log.append(event, self.turn.clone(), None) {
            Ok(_) | Err(_) => {}
        }
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

/// Whether `input` holds a message, not only job notices.
fn has_message(input: &TurnInput) -> bool {
    input
        .pieces
        .iter()
        .any(|piece| matches!(piece, Queued::Steer(..)))
}

/// Removes the unapplied steer `id` names. A prompt's message is not a
/// steer, wherever it sits, so it is skipped.
fn drop_piece(input: &mut TurnInput, id: &CommandId) -> bool {
    let found = input.pieces.iter().enumerate().position(|(index, piece)| {
        matches!(piece, Queued::Steer(message, _) if message.sender.command_id.as_ref() == Some(id) && !session_message::is_session(message))
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

/// A shared-queue entry as the loop's queued piece: the hook verdict it
/// staged, defaulting an unstaged move to unhooked.
pub(crate) fn queued_piece(entry: steering::Entry) -> Queued {
    let staged = entry.staged.unwrap_or_default();
    Queued::Steer(
        Message {
            content: entry.content,
            sender: entry.sender,
        },
        Hooked {
            state: staged.state,
            changed_by: staged.changed_by,
            context: staged.context,
        },
    )
}

/// Whether a queued steer holder still applies under the claimed `taken`
/// snapshot: a holder whose own origin's entry was claimed, or a
/// never-admitted id, which is a forged kept steer the loop takes as it
/// always did. A holder matches only its own origin's entry: a session
/// message's command id comes from another session and is not globally
/// unique, so a same-id session entry never rescues a dropped steer.
fn holder_kept(taken: &[steering::Entry], steering: &steering::Queue, message: &Message) -> bool {
    match &message.sender.command_id {
        None => true,
        Some(id) => {
            taken
                .iter()
                .any(|entry| entry.id == *id && entry.sender.origin == message.sender.origin)
                || !steering.is_admitted(id)
        }
    }
}

impl Loop {
    /// Removes the shared-queue entry `id` names, emitting the queue as the
    /// section left it: `turn` names the drain or boundary that
    /// reconciled. Reports whether one was removed.
    fn remove_steer(&self, id: &CommandId, turn: Option<&TurnId>) -> bool {
        let emit = TurnEmit {
            log: &self.log,
            turn: turn.cloned(),
        };
        self.steering.remove(id, &emit)
    }

    /// Forgets the shared-queue ids of the steers `input` carries: the turn
    /// takes them, so a later drop is stale
    /// (`docs/invocation.md`, "What each command does").
    fn forget_input(&self, input: &TurnInput) {
        for piece in &input.pieces {
            if let Queued::Steer(message, _) = piece
                && let Some(id) = &message.sender.command_id
            {
                self.steering.forget(id);
            }
        }
    }
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
