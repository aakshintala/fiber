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
use contract::tool::InnerCalls;
use serde_json::Value;

use crate::Error;
use crate::host::Reply;
use crate::host::script::HostScript;

use super::asks::PendingAsk;
use super::{ENTRY, GRACE, LOAD_TIMEOUT, Target, expired, timeout_ms};

pub(super) use super::declared::CallbackTimeouts;
pub(crate) use super::declared::{DeclaredHooks, HookPhase};

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
    LoadRecorded,
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
    pub(super) fn at_window(&self, at: Window) {
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
            shared.router.dispose();
            if !matches!(shared.phase, Phase::Stopped(_)) {
                shared.stop(StopReason::Stopped)
            } else {
                Vec::new()
            }
        };
        drop(unsent);
        self.notify();
    }

    /// Ends a timer firing: an `every` that was not cancelled fires again
    /// `ms` after its end; anything else is removed. The callback's result
    /// goes nowhere, and a failure is silent.
    pub(super) fn timer_call_done(&self, call_id: u64, timer_id: u64) {
        let now = self.clock.now();
        let mut shared = self.lock();
        shared.calls.remove(&call_id);
        shared.timers.end_firing(timer_id, now);
    }
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(super) phase: Phase,
    pub(super) loads: HashMap<String, ProviderLoad>,
    pub(super) pending_loads: VecDeque<String>,
    pub(super) misplaced: bool,
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
    /// Where the extension's deliveries, ephemeral events and drives go.
    pub(super) router: super::router::Router,
    /// The timers `host.after` and `host.every` set.
    pub(crate) timers: super::timers::Timers,
    next_id: u64,
    /// Started calls whose caller cancelled them (`docs/tools.md`,
    /// "Cancellation"), until the thread ends them.
    pub(super) cancelled: HashSet<u64>,
    /// Admitted `host.exec` runs by call id, until each group is empty or killed and drained.
    pub(super) execs: HashMap<u64, super::tool::ExecAdmit>,
    /// The admitted command whose inner call is running, from `taken` to
    /// the synchronous answer or drop. Checked only under the hub lock.
    pub(super) serving: Option<u64>,
    /// The inner-call runner of each admitted command or running tool call, by call id, so a
    /// `host.tool` yield admits only while its outer call runs.
    pub(super) runners: HashMap<u64, Arc<dyn InnerCalls>>,
    /// Raised into running Lua by the deadline hook while a cancelled call
    /// runs; set and cleared only under this lock.
    pub(super) interrupt: Arc<std::sync::atomic::AtomicBool>,
}

pub(super) enum ProviderLoad {
    Absent,
    Queued { asked: Instant },
    Running { abandon_at: Option<Instant> },
    Loaded,
    Failed(StopReason),
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
    pub(super) fn judge_load(
        &mut self,
        name: &str,
        provider: &str,
        asked: Instant,
        now: Instant,
    ) -> (Next, Vec<Delivery>) {
        if let Phase::Stopped(reason) = &self.phase {
            return (Next::Return(Err(reason.error(name))), Vec::new());
        }
        let callback = format!("providers/{provider}.lua");
        let until = match self.loads.get(provider) {
            Some(ProviderLoad::Absent) => {
                return (Next::Return(Ok(Value::Bool(false))), Vec::new());
            }
            Some(ProviderLoad::Loaded) => return (Next::Return(Ok(Value::Bool(true))), Vec::new()),
            Some(ProviderLoad::Failed(reason)) => {
                return (Next::Return(Err(reason.error(name))), Vec::new());
            }
            Some(ProviderLoad::Queued { asked }) => asked.checked_add(LOAD_TIMEOUT),
            Some(ProviderLoad::Running { abandon_at }) => {
                if expired(*abandon_at, now) {
                    let reason = StopReason::Abandoned {
                        extension: name.to_owned(),
                        callback,
                    };
                    let error = reason.error(name);
                    let unsent = self.stop(reason);
                    return (Next::Return(Err(error)), unsent);
                }
                return (Next::Sleep(*abandon_at), Vec::new());
            }
            None => asked.checked_add(LOAD_TIMEOUT),
        };
        if expired(until, now) {
            self.pending_loads.retain(|queued| queued != provider);
            self.loads.remove(provider);
            return (
                Next::Return(Err(Error::Timeout {
                    extension: name.to_owned(),
                    callback,
                    timeout_ms: timeout_ms(LOAD_TIMEOUT),
                })),
                Vec::new(),
            );
        }
        let (gate, unsent) = self.gate(name, now);
        match gate {
            Gate::Stopped(e) => (Next::Return(Err(e)), unsent),
            Gate::Wait(at) => (Next::Sleep(at.into_iter().chain(until).min()), unsent),
            Gate::Ready(_) => (Next::Sleep(until), unsent),
        }
    }

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
        if let Some(Progress::Started {
            deadline,
            parked: false,
        }) = self.calls.get(&id)
            && matches!(target, Target::Hook { .. })
            && expired(*deadline, now)
        {
            // A hook past its timeout has failed: the caller takes the
            // timeout at its deadline and the chain moves on, while the
            // worker's stop lands late and is dropped. Waiting the grace
            // would hold the answer on the worker's delivery.
            self.forget(id);
            return (Next::Return(Err(timed_out(name, target, timeout))), unsent);
        }
        let (until, running) = match self.calls.get(&id) {
            Some(Progress::Started {
                deadline,
                parked: false,
            }) if matches!(target, Target::Hook { .. }) => {
                // A running hook answers at its deadline (see above), so
                // wake there: the grace would hold the answer on a
                // withheld worker delivery.
                (*deadline, true)
            }
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
