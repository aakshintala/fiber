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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::RequestId;
use contract::clock::{Clock, Wake};
use contract::inbox::Delivery;
use serde_json::Value;

use crate::Error;
use crate::host::Reply;
use crate::host::script::HostScript;

use super::asks::PendingAsk;
use super::{ENTRY, GRACE, Target, expired};

pub(super) use super::declared::CallbackTimeouts;
pub(crate) use super::declared::{DeclaredHooks, HookPhase};

/// How many deliveries ending before any inbox are kept, newest first:
/// an extension that ends more runs before any sender still delivers its
/// latest lines in call order.
pub(super) const MAX_PENDING_DELIVERIES: usize = 1024;

/// How many ephemeral events emitted before any emitter are kept, newest
/// first, flushed in call order on the first emitter.
pub(super) const MAX_PENDING_EVENTS: usize = 1024;
pub(super) use super::errors::{StopReason, not_registered, stopped, timed_out};

pub(crate) struct Hub {
    shared: Mutex<Shared>,
    changed: Condvar,
    clock: Arc<dyn Clock>,
    /// Host replies supplied only by the case runner (`docs/testing.md`, "Testing an extension").
    host_script: Mutex<Option<Arc<HostScript>>>,
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
    Emitting,
    EmitFlushing,
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
            host_script: Mutex::new(None),
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

    pub(crate) fn set_host_script(&self, script: Arc<HostScript>) {
        *self
            .host_script
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(script);
    }

    pub(crate) fn host_script(&self) -> Option<Arc<HostScript>> {
        self.host_script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(super) fn clock_handle(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Shared> {
        // A panic aborts the process (`docs/code-quality.md`, "Panics"), so
        // no holder can leave the lock poisoned.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The hub lock when nothing holds it: a test's pause point learns
    /// whether an OAuth start runs under the admission lock. None while
    /// the admission lock is held, including on this thread.
    #[cfg(test)]
    pub(super) fn try_lock(&self) -> Option<MutexGuard<'_, Shared>> {
        match self.shared.try_lock() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::WouldBlock) => None,
            Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
        }
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
        // The guard is held from the check into the condvar wait, so a wake
        // under the same lock is never missed.
        support::clock::park(
            self.clock.as_ref(),
            until,
            None,
            &self.changed,
            shared,
            |_| false,
        )
        .unwrap_or_else(|| self.lock())
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
    /// dropped, releasing a credential lock in it.
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
        // Whatever is not sent is moved out and dropped only after the
        // lock is released: a dropped `Resolved` answers its driver,
        // which may re-enter this hub.
        let unsent: VecDeque<Delivery> = {
            let mut shared = self.lock();
            if shared.disposed {
                return;
            }
            if shared.sealed {
                // set_inbox_on_a_sealed_hub_sends_nothing: the buffer is
                // moved out, never flushed, so no extension line follows
                // `fiber_exited`.
                std::mem::take(&mut shared.buffer)
            } else {
                shared.inbox = Some(inbox.clone());
                let buffered = std::mem::take(&mut shared.buffer);
                #[cfg(test)]
                self.at_window(Window::Flushing);
                let mut unsent = VecDeque::new();
                let mut failed = false;
                for delivery in buffered {
                    // `send` on an `mpsc` Sender never blocks; held under
                    // the hub lock so sender choice, buffering and
                    // flushing serialize.
                    if !failed {
                        match inbox.send(delivery) {
                            Ok(()) => continue,
                            Err(failed_send) => {
                                failed = true;
                                unsent.push_back(failed_send.0);
                                continue;
                            }
                        }
                    }
                    unsent.push_back(delivery);
                }
                unsent
            }
        };
        drop(unsent);
    }

    /// Sets the ephemeral emitter `host.status`, `host.widget` and `host.emit`
    /// write through, flushing what was buffered before it in call order.
    /// A later emitter replaces the last, as `set_inbox` does. Holds the hub
    /// lock through the flush, so `seal` cannot return between the choice
    /// and the last write: `fiber_exited` follows every flushed line. The
    /// emitter is the log, which takes only its own lock and never calls
    /// back into the hub, so the order hub-then-log never reverses.
    pub(crate) fn set_emit(&self, emit: std::sync::Arc<dyn contract::emit::Emit>) {
        let mut shared = self.lock();
        if shared.disposed || shared.sealed {
            return;
        }
        shared.emitter = Some(emit.clone());
        let buffered = std::mem::take(&mut shared.emit_buffer);
        #[cfg(test)]
        self.at_window(Window::EmitFlushing);
        for event in buffered {
            // Ephemeral only, so the log takes its own lock and fans out
            // without file I/O and never calls back into the hub; held
            // under the hub lock so a concurrent `seal` cannot interleave.
            emit.emit(&event);
        }
    }

    /// Emits an ephemeral `event` through the late-bound emitter, or buffers
    /// it in call order when none arrived yet. After the drop or `seal`,
    /// it is dropped. Holds the hub lock through the emission, so `seal`
    /// under the same lock drops every later emission and `fiber_exited`
    /// follows every emitted line. The emitter is the log, which takes only
    /// its own lock and never calls back into the hub, so the order
    /// hub-then-log never reverses.
    pub(crate) fn emit(&self, event: contract::events::Event) {
        let mut shared = self.lock();
        if shared.disposed || shared.sealed {
            return;
        }
        let Some(emitter) = shared.emitter.clone() else {
            shared.emit_buffer.push_back(event);
            if shared.emit_buffer.len() > MAX_PENDING_EVENTS {
                drop(shared.emit_buffer.pop_front());
            }
            return;
        };
        #[cfg(test)]
        self.at_window(Window::Emitting);
        // Held under the hub lock so a concurrent `seal` cannot slip in
        // between the choice and the write; `shared` drops after.
        emitter.emit(&event);
    }

    /// Hands the in-process driver to this extension's `host.drive`.
    /// A later driver replaces the last, as `set_inbox` does.
    pub(crate) fn set_driver(&self, drive: std::sync::Arc<dyn contract::extension::Drive>) {
        let mut shared = self.lock();
        if shared.disposed {
            return;
        }
        shared.driver = Some(drive);
    }

    /// The driver `host.drive` sends through: none before `drive_to`, once
    /// dropped, or once sealed, so no drive follows `fiber_exited`.
    pub(crate) fn driver(&self) -> Option<std::sync::Arc<dyn contract::extension::Drive>> {
        let shared = self.lock();
        if shared.disposed || shared.sealed {
            return None;
        }
        shared.driver.clone()
    }

    /// Drops every later emission and delivery from this extension; called by
    /// `Session::quiesce` before `fiber_exited`. A running callback is not
    /// stopped; only its output is dropped.
    pub(crate) fn seal(&self) {
        self.lock().sealed = true;
    }

    /// Releases a held queued call, so the stream may start it.
    pub(crate) fn release(&self, id: u64) {
        {
            let mut shared = self.lock();
            if let Some(job) = shared.queue.iter_mut().find(|job| job.id == id) {
                job.held = false;
            }
        }
        self.notify();
    }

    /// Records the extension's drop under the same lock that routes
    /// deliveries: anything routed after this is dropped.
    pub(crate) fn dispose(&self, _name: &str) {
        let unsent = {
            let mut shared = self.lock();
            shared.disposed = true;
            if !matches!(shared.phase, Phase::Stopped(_)) {
                shared.stop(StopReason::Stopped)
            } else {
                Vec::new()
            }
        };
        drop(unsent);
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
    /// or after `seal` is dropped instead; routing holds one lock, so nothing is stranded.
    pub(crate) fn send(&self, delivery: Delivery) {
        let unsent = {
            let mut shared = self.lock();
            #[cfg(test)]
            self.at_window(Window::Selected);
            shared.route(delivery)
        };
        // Dropped only after the lock is released: a dropped `Resolved`
        // answers its driver, which may re-enter this hub.
        drop(unsent);
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
    /// The open `host.ask` questions, by request id.
    pub(super) asks: HashMap<RequestId, PendingAsk>,
    /// Whether a client can answer a `host.ask`: set by the session, false
    /// until it is. An ask with no answerer returns declined at once.
    pub(super) answerable: bool,
    /// The loop's inbox for the extension's deliveries, set by `deliver_to`.
    pub(super) inbox: Option<std::sync::mpsc::Sender<Delivery>>,
    /// Deliveries that ended before any inbox, in call order, sent on the
    /// first one: at most [`MAX_PENDING_DELIVERIES`], oldest dropped first.
    pub(super) buffer: VecDeque<Delivery>,
    /// The extension was dropped: anything routed after this is dropped,
    /// under the same lock that routes deliveries.
    pub(super) disposed: bool,
    /// `seal` was called: every later emission and delivery is dropped,
    /// under the same lock that routes them.
    pub(super) sealed: bool,
    /// The ephemeral emitter `host.status`, `host.widget` and `host.emit`
    /// write through, set late by `emit_to`.
    pub(super) emitter: Option<std::sync::Arc<dyn contract::emit::Emit>>,
    /// The in-process driver `host.drive` sends through, set late by
    /// `drive_to`. None before it, or once the extension is sealed.
    pub(super) driver: Option<std::sync::Arc<dyn contract::extension::Drive>>,
    /// Ephemeral events emitted before any emitter, in call order, flushed
    /// on the first one: at most [`MAX_PENDING_EVENTS`], oldest dropped
    /// first.
    pub(super) emit_buffer: VecDeque<contract::events::Event>,
    /// The timers `host.after` and `host.every` set, by id.
    pub(crate) timers: HashMap<u64, Timer>,
    /// Timer ids whose Lua functions the extension's thread still frees.
    pub(crate) timer_cleanup: Vec<u64>,
    next_id: u64,
    /// Started calls whose caller cancelled them (`docs/tools.md`,
    /// "Cancellation"), until the thread ends them.
    pub(super) cancelled: HashSet<u64>,
    /// Admitted `host.exec` runs by call id, until each group is empty or killed and drained.
    pub(super) execs: HashMap<u64, super::tool::ExecAdmit>,
    /// Raised into running Lua by the deadline hook while a cancelled call
    /// runs; set and cleared only under this lock.
    pub(super) interrupt: Arc<std::sync::atomic::AtomicBool>,
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
    /// Final. Every call gets this reason's error.
    Stopped(StopReason),
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
    /// Cancelled before it returned: it ran no further.
    Cancelled,
}

pub(super) struct Job {
    pub(super) id: u64,
    pub(super) target: Target,
    pub(super) arg: Value,
    /// When Fiber asked. Waiting counts against the callback's timeout from
    /// here.
    pub(super) asked: Instant,
    /// A held job never starts until `release`; every stream job behind it waits too.
    pub(super) held: bool,
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
        self.push_at(target, arg, asked, false)
    }

    /// Queues a held call, which never starts until `release`.
    pub(super) fn push_held(&mut self, target: Target, arg: Value, asked: Instant) -> u64 {
        self.push_at(target, arg, asked, true)
    }

    fn push_at(&mut self, target: Target, arg: Value, asked: Instant, held: bool) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.queue.push_back(Job {
            id,
            target,
            arg,
            asked,
            held,
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
                held: false,
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
    /// abandons it. Returns what stopping declined, for the caller to drop
    /// once the hub lock is released; empty unless this call abandoned.
    pub(super) fn gate(&mut self, name: &str, now: Instant) -> (Gate<'_>, Vec<Delivery>) {
        let mut unsent = Vec::new();
        if let Phase::Registering { abandon_at } = self.phase
            && expired(abandon_at, now)
        {
            // debt: the abandoned thread is leaked, still running, until the
            // process exits, and a credential lock its VM holds with it; Rust
            // cannot stop a thread. Cap abandoned VMs per session if leaked
            // threads or locks show (docs/performance.md).
            unsent = self.stop(StopReason::Abandoned {
                extension: name.to_owned(),
                callback: ENTRY.to_owned(),
            });
        }
        let gate = match &self.phase {
            // No thread is coming: a caller starts the thread before it
            // waits, so failing beats waiting forever.
            Phase::Idle => Gate::Stopped(stopped(name)),
            Phase::Registering { abandon_at } => Gate::Wait(*abandon_at),
            Phase::Ready(timeouts) => Gate::Ready(timeouts),
            Phase::Stopped(reason) => Gate::Stopped(reason.error(name)),
        };
        (gate, unsent)
    }

    /// Judges the call `id` against the phase and its own progress.
    /// Returns what stopping declined, for the caller to drop once the hub
    /// lock is released; empty unless this call abandoned the extension.
    pub(super) fn judge(
        &mut self,
        name: &str,
        id: u64,
        target: &Target,
        asked: Instant,
        now: Instant,
    ) -> (Next, Vec<Delivery>) {
        // Stopped is final for every waiter, one holding a result included.
        if let Phase::Stopped(reason) = &self.phase {
            let e = reason.error(name);
            self.forget(id);
            return (Next::Return(Err(e)), Vec::new());
        }
        if !matches!(
            self.calls.get(&id),
            Some(Progress::Queued | Progress::Started { .. })
        ) {
            return (
                Next::Return(match self.calls.remove(&id) {
                    Some(Progress::Done(result)) => result,
                    _ => Err(stopped(name)),
                }),
                Vec::new(),
            );
        }
        let (gate, mut unsent) = self.gate(name, now);
        let declared = match gate {
            Gate::Ready(timeouts) => timeouts.timeout(target),
            Gate::Wait(until) => return (Next::Sleep(until), unsent),
            Gate::Stopped(e) => {
                self.forget(id);
                return (Next::Return(Err(e)), unsent);
            }
        };
        let Some(timeout) = declared else {
            self.forget(id);
            return (Next::Return(Err(not_registered(name, target))), unsent);
        };
        let (until, running) = match self.calls.get(&id) {
            Some(Progress::Started { deadline, parked }) => {
                (deadline.and_then(|at| at.checked_add(GRACE)), !*parked)
            }
            _ => (asked.checked_add(timeout), false),
        };
        if !expired(until, now) {
            return (Next::Sleep(until), unsent);
        }
        self.forget(id);
        if !running {
            // Queued or parked, not on the thread: failing it leaves the VM up.
            return (Next::Return(Err(timed_out(name, target, timeout))), unsent);
        }
        // debt: as in `gate`, the abandoned thread leaks until the process exits.
        unsent = self.stop(StopReason::Stopped);
        (
            Next::Return(Err(Error::Abandoned {
                extension: name.to_owned(),
                callback: target.to_string(),
            })),
            unsent,
        )
    }

    /// Records `result` for the call `id`, if its caller still waits. A
    /// cancelled call that failed ends cancelled.
    pub(super) fn finish(&mut self, id: u64, result: Result<Value, Error>) {
        let stopped = self.cancelled.remove(&id) && result.is_err();
        if let Some(progress) = self.calls.get_mut(&id) {
            *progress = if stopped {
                Progress::Cancelled
            } else {
                Progress::Done(result)
            };
        }
    }

    /// Stops the extension with `reason`. The replies no thread will take are
    /// dropped, which releases a credential lock in one. Admitted exec runs
    /// stop without the Lua thread: their sender's drop reaches the run.
    /// Every held ask is declined by `fiber` in the same critical section;
    /// returns what could not be sent, for the caller to drop afterwards.
    pub(super) fn stop(&mut self, reason: StopReason) -> Vec<Delivery> {
        self.phase = Phase::Stopped(reason);
        self.replies.clear();
        self.execs.values_mut().for_each(|admit| admit.stop_take());
        // stop_declines_every_held_ask: one `Resolved` per held ask.
        let held: Vec<RequestId> = self.asks.keys().cloned().collect();
        let mut unsent = Vec::new();
        for request in &held {
            unsent.extend(self.decline(request));
        }
        unsent
    }

    /// Drops the call `id`: its caller has stopped waiting.
    fn forget(&mut self, id: u64) {
        self.calls.remove(&id);
        self.cancelled.remove(&id);
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

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
