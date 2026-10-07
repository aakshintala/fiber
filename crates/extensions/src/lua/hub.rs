//! The one state every waiter observes (`docs/extensions.md`, "How an
//! extension runs" and "When an extension misbehaves"): the extension's
//! phase, the calls not yet started, each call's progress and the replies of
//! host calls, behind one lock. Every caller and the extension's thread wait
//! on one condition variable, each no later than its own limit, and every
//! change wakes them all.
//!
//! Idle becomes Registering on the first call. Registering becomes Ready when
//! the entry script returns, or Stopped when it errors, passes its deadline,
//! or is still running a grace period past it. Ready becomes Stopped when a
//! running callback is still running a grace period past its own deadline.
//! Dropping the extension stops it. Stopped is final.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::inbox::Delivery;
use serde_json::Value;

use crate::Error;
use crate::host::Reply;

use super::{ENTRY, GRACE, Target, expired};

pub(super) use super::declared::CallbackTimeouts;
pub(crate) use super::declared::{DeclaredHook, DeclaredHooks, HookPhase};

pub(crate) struct Hub {
    shared: Mutex<Shared>,
    changed: Condvar,
    clock: Arc<dyn Clock>,
    /// A test's pause inside the hub lock where a run's delivery is routed.
    #[cfg(test)]
    window: Mutex<Option<WindowHook>>,
}

/// Where a test may pause an extension delivery, inside the hub lock:
/// `send` has chosen the sender or the buffer, or `set_inbox` has
/// set the sender and taken the buffer but not yet flushed it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Window {
    Selected,
    Flushing,
}

#[cfg(test)]
type WindowHook = Arc<dyn Fn(&Hub, Window) + Send + Sync>;

impl Hub {
    /// The hub every waiter and the extension's thread share, subscribed to
    /// `clock` so a move of that clock wakes them.
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        let hub = Arc::new(Self {
            shared: Mutex::new(Shared::default()),
            changed: Condvar::new(),
            clock: Arc::clone(&clock),
            #[cfg(test)]
            window: Mutex::new(None),
        });
        let cloned = Arc::clone(&hub);
        let wake: Arc<dyn Wake> = cloned;
        clock.subscribe(Arc::downgrade(&wake));
        hub
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    pub(super) fn clock_handle(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Shared> {
        // A panic aborts the process (`docs/code-quality.md`, "Panics"), so
        // no holder can leave the lock poisoned.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wakes every waiter to look at the state again.
    pub(super) fn notify(&self) {
        self.changed.notify_all();
    }

    /// Sleeps until a change or `until`, whichever is first. `until` at or
    /// before the clock's now returns without blocking.
    pub(super) fn wait<'a>(
        &'a self,
        shared: MutexGuard<'a, Shared>,
        until: Option<Instant>,
    ) -> MutexGuard<'a, Shared> {
        let changed = &self.changed;
        // `FnMut` cannot move the guard out and back. The slot holds it
        // across the one call `wait_until` makes.
        let mut slot = Some(shared);
        self.clock.wait_until(until, &mut |bound| {
            let Some(shared) = slot.take() else {
                return;
            };
            // A zero bound returns at once: `wait_timeout` of zero does not block.
            slot = Some(match bound {
                Some(d) => {
                    changed
                        .wait_timeout(shared, d)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => changed.wait(shared).unwrap_or_else(PoisonError::into_inner),
            });
        });
        match slot {
            Some(shared) => shared,
            None => self.lock(),
        }
    }

    /// Blocks until `done` holds, or `timeout` of real time passes. For a
    /// test that waits on the hub's own signal.
    #[cfg(test)]
    pub(super) fn wait_for(
        &self,
        shared: MutexGuard<'_, Shared>,
        timeout: Duration,
        mut done: impl FnMut(&Shared) -> bool,
    ) -> bool {
        let (guard, _) = self
            .changed
            .wait_timeout_while(shared, timeout, |state| !done(state))
            .unwrap_or_else(PoisonError::into_inner);
        done(&guard)
    }

    /// Runs `hook` at each delivery window from now on.
    #[cfg(test)]
    pub(super) fn pause_at_windows(&self, hook: WindowHook) {
        *self.window.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    #[cfg(test)]
    fn at_window(&self, at: Window) {
        let hook = self
            .window
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(self, at);
        }
    }

    /// Records `result` for the call `id`, if its caller still waits.
    pub(super) fn finish(&self, id: u64, result: Result<Value, Error>) {
        self.lock().finish(id, result);
        self.notify();
    }

    /// Hands the call `id`'s parked callback the answer to what it waits on.
    /// A stopped extension's thread takes no more replies, so the reply is
    /// dropped, which releases a credential lock in it.
    pub(super) fn deliver(&self, id: u64, reply: Reply) {
        let mut shared = self.lock();
        if !matches!(shared.phase, Phase::Ready(_)) {
            return;
        }
        shared.replies.push((id, reply));
        drop(shared);
        self.notify();
    }

    /// Hands the session loop's inbox to the extension's deliveries: one
    /// that ended before any sender is buffered and sent on the first
    /// sender; a later sender replaces the last. A delivery routed after
    /// the extension is dropped is dropped, as is a sender that arrives
    /// after it. Routing holds one lock, so a delivery ending against
    /// `deliver_to` is sent, never stranded: an `mpsc` send never blocks,
    /// so sending under the lock is safe.
    pub(crate) fn set_inbox(&self, inbox: std::sync::mpsc::Sender<Delivery>) {
        let mut shared = self.lock();
        if shared.disposed {
            return;
        }
        shared.inbox = Some(inbox.clone());
        let buffered = std::mem::take(&mut shared.buffer);
        #[cfg(test)]
        self.at_window(Window::Flushing);
        for delivery in buffered {
            // `send` on an `mpsc` Sender never blocks; held under the hub
            // lock so a concurrent `send` cannot interleave.
            if inbox.send(delivery).is_err() {
                return;
            }
        }
    }

    /// Records the extension's drop under the same lock that routes
    /// deliveries: anything routed after this is dropped.
    pub(crate) fn dispose(&self, name: &str) {
        let mut shared = self.lock();
        shared.disposed = true;
        if !matches!(shared.phase, Phase::Stopped(_)) {
            shared.stop(stopped(name));
        }
        drop(shared);
        self.notify();
    }

    /// Registers a timer `host.after` or `host.every` set: `every` holds
    /// its period for rescheduling, `None` for a one-shot `after`.
    /// Returns its id, assigned in set order from 0.
    pub(crate) fn add_timer(
        &self,
        every: Option<Duration>,
        due: Instant,
        timeout: Duration,
    ) -> u64 {
        let mut shared = self.lock();
        let id = shared.next_timer;
        shared.next_timer = shared.next_timer.wrapping_add(1);
        shared.timers.insert(
            id,
            Timer {
                id,
                every,
                due,
                timeout,
                cancelled: false,
                firing: false,
            },
        );
        drop(shared);
        self.notify();
        id
    }

    /// Stops a timer: a due firing never starts, and a mid-firing `every`
    /// does not fire again. Unknown or finished ids are a no-op.
    pub(crate) fn cancel_timer(&self, id: u64) {
        let mut shared = self.lock();
        // A listed timer is never both cancelled and idle: an idle cancel
        // removes it, and a cancelled firing leaves at its end.
        let remove = match shared.timers.get_mut(&id) {
            Some(timer) => {
                timer.cancelled = true;
                !timer.firing
            }
            None => false,
        };
        if remove {
            shared.timers.remove(&id);
            shared.timer_cleanup.push(id);
        }
        drop(shared);
        self.notify();
    }

    /// Ends a timer firing: an `every` that was not cancelled fires again
    /// `ms` after its end; anything else is removed. The callback's result
    /// goes nowhere, and a failure is silent.
    pub(super) fn timer_call_done(&self, call_id: u64, timer_id: u64) {
        let now = self.clock.now();
        let mut shared = self.lock();
        shared.calls.remove(&call_id);
        shared.timer_end(timer_id, now);
    }

    /// The timer ids whose Lua functions the extension's thread frees.
    pub(super) fn take_timer_cleanup(&self) -> Vec<u64> {
        std::mem::take(&mut self.lock().timer_cleanup)
    }

    /// Routes an extension delivery to the loop's inbox, or buffers it in
    /// call order when no sender arrived yet. One routed after the drop
    /// is dropped instead; routing holds one lock, so nothing is stranded.
    pub(crate) fn send(&self, delivery: Delivery) {
        let mut shared = self.lock();
        if shared.disposed {
            return;
        }
        let inbox = shared.inbox.clone();
        #[cfg(test)]
        self.at_window(Window::Selected);
        if let Some(inbox) = inbox {
            // `send` on an `mpsc` Sender never blocks; held under the hub
            // lock so sender choice, buffering and flushing serialize.
            match inbox.send(delivery) {
                Ok(()) | Err(_) => {}
            }
        } else {
            shared.buffer.push(delivery);
        }
    }
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(super) phase: Phase,
    /// Calls the thread has not started, in the order Fiber asked.
    pub(super) queue: VecDeque<Job>,
    /// Each waiting call's progress, by id.
    pub(super) calls: HashMap<u64, Progress>,
    /// Answers to what parked callbacks wait on, by call id.
    pub(super) replies: Vec<(u64, Reply)>,
    /// Every hook's timeout once the entry script returns, when
    /// configuration overrides them.
    pub(super) hook_timeout: Option<Duration>,
    /// The loop's inbox for the extension's deliveries, set by `deliver_to`.
    pub(super) inbox: Option<std::sync::mpsc::Sender<Delivery>>,
    /// Deliveries that ended before any inbox, in call order, sent on the
    /// first one.
    pub(super) buffer: Vec<Delivery>,
    /// The extension was dropped: anything routed after this is dropped,
    /// under the same lock that routes deliveries.
    pub(super) disposed: bool,
    /// The timers `host.after` and `host.every` set, by id.
    pub(crate) timers: HashMap<u64, Timer>,
    /// Timer ids whose Lua functions the extension's thread still frees.
    pub(crate) timer_cleanup: Vec<u64>,
    next_id: u64,
    /// The next timer's id, assigned in set order from 0.
    pub(crate) next_timer: u64,
}

/// A timer `host.after` or `host.every` set: when its callback fires next,
/// on the extension's clock (`docs/extensions.md`, "Host calls").
#[derive(Debug)]
pub(crate) struct Timer {
    /// Assigned in set order from 0.
    pub(crate) id: u64,
    /// The period an `every` reschedules with; `None` for an `after`.
    pub(crate) every: Option<Duration>,
    /// When the callback fires next.
    pub(crate) due: Instant,
    /// The firing's timeout, from its start.
    pub(crate) timeout: Duration,
    /// `:cancel()` stopped it; a mid-firing cancel finishes the firing.
    pub(crate) cancelled: bool,
    /// A firing is running; one firing at a time.
    pub(crate) firing: bool,
}

#[derive(Default)]
pub(super) enum Phase {
    /// Never called: no VM, no thread.
    #[default]
    Idle,
    /// The entry script is running. Past `abandon_at` it is abandoned.
    Registering { abandon_at: Option<Instant> },
    /// The entry script returned and registered these callbacks.
    Ready(CallbackTimeouts),
    /// Final. Every call gets this error.
    Stopped(Error),
}

pub(super) enum Progress {
    /// Waiting in the queue.
    Queued,
    /// Started, with the deadline it declared. `parked` while suspended on a
    /// host call, when the thread is free to fail it at its deadline.
    Started {
        deadline: Option<Instant>,
        parked: bool,
    },
    Done(Result<Value, Error>),
}

pub(super) struct Job {
    pub(super) id: u64,
    pub(super) target: Target,
    pub(super) arg: Value,
    /// When Fiber asked. Waiting counts against the callback's timeout from
    /// here.
    pub(super) asked: Instant,
}

/// What registration lets a waiter do.
pub(super) enum Gate<'a> {
    Ready(&'a CallbackTimeouts),
    /// Wait, no later than this.
    Wait(Option<Instant>),
    Stopped(Error),
}

/// What a waiting call does next.
pub(super) enum Next {
    Return(Result<Value, Error>),
    Sleep(Option<Instant>),
}

impl Shared {
    /// Queues a call and returns its id.
    pub(super) fn push(&mut self, target: Target, arg: Value, asked: Instant) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.queue.push_back(Job {
            id,
            target,
            arg,
            asked,
        });
        self.calls.insert(id, Progress::Queued);
        id
    }

    /// Queues a timer firing as `Target::Timer`, under its own `timeout`
    /// from `now`: the firing, not a queued call. Returns the call's id,
    /// its timeout and its deadline.
    pub(super) fn push_timer(
        &mut self,
        timer_id: u64,
        timeout: Duration,
        now: Instant,
    ) -> (Job, Duration, Option<Instant>) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let deadline = now.checked_add(timeout);
        self.calls.insert(
            id,
            Progress::Started {
                deadline,
                parked: false,
            },
        );
        (
            Job {
                id,
                target: Target::Timer { id: timer_id },
                arg: Value::Null,
                asked: now,
            },
            timeout,
            deadline,
        )
    }

    /// Starts the earliest due timer, when one waits: due, neither
    /// cancelled nor firing. Marks it firing and returns its id and
    /// timeout. One firing at a time.
    pub(super) fn timer_fire(&mut self, now: Instant) -> Option<(u64, Duration)> {
        let timer = self
            .timers
            .values_mut()
            .filter(|timer| !timer.cancelled && !timer.firing && timer.due <= now)
            .min_by_key(|timer| (timer.due, timer.id))?;
        timer.firing = true;
        Some((timer.id, timer.timeout))
    }

    /// Ends a timer firing at `now`: an `every` that was not cancelled
    /// fires again `ms` after its end; anything else leaves, freeing its
    /// Lua function on the extension's thread.
    pub(super) fn timer_end(&mut self, id: u64, now: Instant) {
        let reschedule = match self.timers.get_mut(&id) {
            Some(timer) if !timer.cancelled => timer.every,
            Some(_) | None => None,
        };
        match reschedule {
            Some(every) => {
                if let Some(timer) = self.timers.get_mut(&id) {
                    timer.due = now.checked_add(every).unwrap_or(now);
                    timer.firing = false;
                }
            }
            None => {
                self.timers.remove(&id);
                self.timer_cleanup.push(id);
            }
        }
    }

    /// The earliest a waiting timer fires, for the thread's next wake.
    pub(super) fn timer_wake(&self) -> Option<Instant> {
        self.timers
            .values()
            .filter(|timer| !timer.cancelled && !timer.firing)
            .map(|timer| timer.due)
            .min()
    }

    /// Whether registration is done, still running, or has stopped the
    /// extension. A waiter that finds the entry script past its grace
    /// abandons it.
    pub(super) fn gate(&mut self, name: &str, now: Instant) -> Gate<'_> {
        if let Phase::Registering { abandon_at } = self.phase
            && expired(abandon_at, now)
        {
            // debt: the abandoned thread is leaked, still running, until the
            // process exits, and a credential lock its VM holds with it; Rust
            // cannot stop a thread. Cap abandoned VMs per session if leaked
            // threads or locks show (docs/performance.md).
            self.stop(Error::Abandoned {
                extension: name.to_owned(),
                callback: ENTRY.to_owned(),
            });
        }
        match &self.phase {
            // A caller starts the thread before it waits, so no thread is
            // coming: failing beats waiting forever.
            Phase::Idle => Gate::Stopped(stopped(name)),
            Phase::Registering { abandon_at } => Gate::Wait(*abandon_at),
            Phase::Ready(timeouts) => Gate::Ready(timeouts),
            Phase::Stopped(e) => Gate::Stopped(again(name, e)),
        }
    }

    /// Judges the call `id` against the phase and its own progress.
    pub(super) fn judge(
        &mut self,
        name: &str,
        id: u64,
        target: &Target,
        asked: Instant,
        now: Instant,
    ) -> Next {
        // Stopped is final for every waiter, one holding a result included.
        if let Phase::Stopped(e) = &self.phase {
            let e = again(name, e);
            self.forget(id);
            return Next::Return(Err(e));
        }
        if !matches!(
            self.calls.get(&id),
            Some(Progress::Queued | Progress::Started { .. })
        ) {
            return Next::Return(match self.calls.remove(&id) {
                Some(Progress::Done(result)) => result,
                _ => Err(stopped(name)),
            });
        }
        let declared = match self.gate(name, now) {
            Gate::Ready(timeouts) => timeouts.timeout(target),
            Gate::Wait(until) => return Next::Sleep(until),
            Gate::Stopped(e) => {
                self.forget(id);
                return Next::Return(Err(e));
            }
        };
        let Some(timeout) = declared else {
            self.forget(id);
            return Next::Return(Err(not_registered(name, target)));
        };
        let (until, running) = match self.calls.get(&id) {
            Some(Progress::Started { deadline, parked }) => {
                (deadline.and_then(|at| at.checked_add(GRACE)), !*parked)
            }
            _ => (asked.checked_add(timeout), false),
        };
        if !expired(until, now) {
            return Next::Sleep(until);
        }
        self.forget(id);
        if !running {
            // Queued or parked, not on the thread: failing it leaves the VM up.
            return Next::Return(Err(timed_out(name, target, timeout)));
        }
        // debt: the abandoned thread is leaked, still running, until the
        // process exits, and a credential lock its VM holds with it; Rust
        // cannot stop a thread. Cap abandoned VMs per session if leaked
        // threads or locks show (docs/performance.md).
        self.stop(stopped(name));
        Next::Return(Err(Error::Abandoned {
            extension: name.to_owned(),
            callback: target.to_string(),
        }))
    }

    /// Records `result` for the call `id`, if its caller still waits.
    pub(super) fn finish(&mut self, id: u64, result: Result<Value, Error>) {
        if let Some(progress) = self.calls.get_mut(&id) {
            *progress = Progress::Done(result);
        }
    }

    /// Stops the extension with `e`. The replies no thread will take are
    /// dropped, which releases a credential lock in one.
    pub(super) fn stop(&mut self, e: Error) {
        self.phase = Phase::Stopped(e);
        self.replies.clear();
    }

    /// Drops the call `id`: its caller has stopped waiting.
    fn forget(&mut self, id: u64) {
        self.calls.remove(&id);
        self.queue.retain(|job| job.id != id);
    }
}

impl Wake for Hub {
    fn wake(&self) {
        // The hub lock is taken before the notify, so a waiter that has
        // judged and not yet parked cannot miss this wake.
        let _guard = self.lock();
        self.changed.notify_all();
    }
}

pub(super) fn stopped(name: &str) -> Error {
    Error::Stopped {
        extension: name.to_owned(),
    }
}

pub(super) fn timed_out(name: &str, target: &Target, timeout: Duration) -> Error {
    Error::Timeout {
        extension: name.to_owned(),
        callback: target.to_string(),
        timeout_ms: timeout_ms(timeout),
    }
}

pub(super) fn not_registered(name: &str, target: &Target) -> Error {
    match target {
        Target::Command(command) => Error::UnknownCommand {
            extension: name.to_owned(),
            command: command.clone(),
        },
        Target::Provider { .. } | Target::Hook { .. } | Target::Timer { .. } => {
            Error::UnknownCallback {
                extension: name.to_owned(),
                callback: target.to_string(),
            }
        }
    }
}

/// A copy of the error that stopped the extension, for the next caller.
/// These are the errors a stopped extension can hold.
fn again(name: &str, e: &Error) -> Error {
    match e {
        Error::Io { path, source } => Error::Io {
            path: path.clone(),
            source: io::Error::new(source.kind(), source.to_string()),
        },
        Error::Lua { extension, message } => Error::Lua {
            extension: extension.clone(),
            message: message.clone(),
        },
        Error::Damaged(inner) => Error::Damaged(inner.clone()),
        Error::Timeout {
            extension,
            callback,
            timeout_ms,
        } => Error::Timeout {
            extension: extension.clone(),
            callback: callback.clone(),
            timeout_ms: *timeout_ms,
        },
        Error::Abandoned {
            extension,
            callback,
        } => Error::Abandoned {
            extension: extension.clone(),
            callback: callback.clone(),
        },
        Error::Stopped { .. }
        | Error::Config(_)
        | Error::Overlaps { .. }
        | Error::NeedsNewerFiber { .. }
        | Error::ApiVersion { .. }
        | Error::BadVersion { .. }
        | Error::BadName { .. }
        | Error::GitMissing
        | Error::Git { .. }
        | Error::NoRepository { .. }
        | Error::MajorConflict { .. }
        | Error::NoVersion { .. }
        | Error::Unresolved
        | Error::NotInstalled { .. }
        | Error::WrongName { .. }
        | Error::Busy
        | Error::SlugTaken { .. }
        | Error::NoTag { .. }
        | Error::BadRecord { .. }
        | Error::InstallStep { .. }
        | Error::InstallExited { .. }
        | Error::Download { .. }
        | Error::BinaryChecksum { .. }
        | Error::Rollback { .. }
        | Error::ProviderMissing { .. }
        | Error::ModelMissing { .. }
        | Error::UnknownModel { .. }
        | Error::Ambiguous { .. }
        | Error::UnknownCommand { .. }
        | Error::UnknownCallback { .. }
        | Error::BadReturn { .. }
        | Error::Credential(_)
        | Error::RefreshRejected { .. }
        | Error::RefreshUnreachable { .. }
        | Error::BadRepositoryPath { .. }
        | Error::Pin { .. }
        | Error::ChangedWhileCopying { .. }
        | Error::NoModel => stopped(name),
    }
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
