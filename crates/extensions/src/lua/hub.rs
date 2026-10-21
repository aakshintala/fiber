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
//! A running hook or command is answered at its deadline.
//! Dropping the extension stops it. Stopped is final.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use contract::RequestId;
use contract::clock::{Clock, Wake};
use contract::diag::DebugLog;
use contract::events::{Event, Notice};
use contract::inbox::Delivery;
use contract::tool::InnerCalls;
use serde_json::Value;

use crate::Error;
use crate::host::Reply;
use crate::host::script::HostScript;
use contract::ErrorCode;

use super::asks::PendingAsk;
use super::{ENTRY, GRACE, LOAD_TIMEOUT, Target, expired, timeout_ms};

pub(super) use super::declared::CallbackTimeouts;
pub(crate) use super::declared::{DeclaredHooks, DeclaredWatchers, HookPhase};

pub(super) use super::errors::{StopReason, not_registered, stopped, timed_out};

pub(crate) struct Hub {
    shared: Mutex<Shared>,
    changed: Condvar,
    clock: Arc<dyn Clock>,
    /// The session's diagnostic sink, installed once: the first
    /// installation wins, so a later session never redirects a running
    /// VM's observation. Outside the shared state and the admission
    /// lock: installing it starts no work.
    diagnostics: Mutex<Option<Arc<dyn DebugLog>>>,
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
    /// The drain's pre-replay wait parked with a watcher delivery still
    /// queued or running: the test's proof the wait engaged, fired under
    /// the hub lock — the hook must not take it again.
    DrainWait,
}

#[cfg(test)]
type WindowHook = Arc<dyn Fn(&Hub, Window) + Send + Sync>;

/// Every live extension hub, weakly held: a shutdown stops each one, so a
/// callback in flight answers `Stopped` and its admitted `host.exec` runs
/// stop as on a cancel. Pruned on every stop; entries whose hub is gone
/// drop out.
static HUBS: Mutex<Vec<Weak<Hub>>> = Mutex::new(Vec::new());

/// Stops every live extension hub with `Stopped`, as a shutdown stops a
/// tool call's group (`docs/invocation.md`, "Shutdown"): each callback in
/// flight answers `Stopped`, each admitted `host.exec` run stops with
/// SIGTERM, SIGKILL 800 ms later for a group still alive, and every waiter
/// wakes. The session's shutdown calls this beside stopping jobs; hubs
/// created after it are the next session's, once this process is gone.
/// Takes the registry lock, then each hub's lock in turn, and nothing
/// while holding either: no waiter takes the registry lock under a hub
/// lock, so the order never inverts.
pub fn stop_every_callback() {
    let live: Vec<Arc<Hub>> = {
        let mut hubs = HUBS.lock().unwrap_or_else(PoisonError::into_inner);
        hubs.retain(|hub| hub.upgrade().is_some());
        hubs.iter().filter_map(Weak::upgrade).collect()
    };
    for hub in live {
        hub.stop_for_shutdown();
    }
}

impl Hub {
    /// The hub every waiter and the extension's thread share, subscribed to
    /// `clock` so a move of that clock wakes them.
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        let hub = Arc::new(Self {
            shared: Mutex::new(Shared::default()),
            changed: Condvar::new(),
            clock: Arc::clone(&clock),
            diagnostics: Mutex::new(None),
            host_script: Mutex::new(None),
            #[cfg(test)]
            window: Mutex::new(None),
        });
        let cloned = Arc::clone(&hub);
        let wake: Arc<dyn Wake> = cloned;
        clock.subscribe(Arc::downgrade(&wake));
        HUBS.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::downgrade(&hub));
        hub
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Installs the session's diagnostic sink, once: the first
    /// installation wins and a later one changes nothing. Runs no
    /// callback, so installing it before provider work is silent.
    pub(crate) fn set_diagnostics(&self, debug: Arc<dyn DebugLog>) {
        let mut slot = self
            .diagnostics
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(debug);
        }
    }

    /// The installed sink, if any.
    pub(crate) fn diagnostics(&self) -> Option<Arc<dyn DebugLog>> {
        self.diagnostics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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
    /// A watched hook or command that settled disarms its watch instead: its caller
    /// already took `Timeout`, so the late result is dropped.
    pub(super) fn finish(&self, id: u64, result: Result<Value, Error>) {
        let mut shared = self.lock();
        if shared.abandon.as_ref().is_some_and(|watch| watch.id == id) {
            shared.abandon = None;
        } else {
            shared.finish(id, result);
        }
        drop(shared);
        self.notify();
    }

    /// Records a watcher delivery's end: a failure gives a `notice` naming
    /// the extension through the late-bound emitter (dropped after the
    /// seal, when the drain collects failures itself instead), and the
    /// watcher's busy bit clears. The result is dropped unless the drain
    /// keeps it to collect: a mid-session delivery has no waiter.
    pub(super) fn finish_watcher(
        &self,
        name: &str,
        id: u64,
        index: usize,
        result: Result<Value, Error>,
    ) {
        // The session-end drain waits on its own delivery, so the result
        // is recorded for its waiter and no notice is emitted: the log is
        // already closed to new lines. A mid-session delivery has no
        // waiter, so its failure is noticed and the call forgotten — even
        // while the drain keeps its own call: a live delivery finishing
        // under the drain still needs its notice, and the drain never
        // collects another call's result.
        let kept = self.lock().keep_watcher_done;
        if kept == Some(id) {
            if let Some(progress) = self.lock().calls.get_mut(&id) {
                *progress = Progress::Done(result);
            }
        } else if let Err(e) = &result {
            self.emit(Event::Notice(Notice {
                code: ErrorCode::ExtensionFailed,
                message: format!(
                    "The `watcher:{index}` watcher failed, so its event was dropped: {e}"
                ),
                extension: Some(name.to_owned()),
            }));
        }
        let mut shared = self.lock();
        if shared.abandon.as_ref().is_some_and(|watch| watch.id == id) {
            shared.abandon = None;
        }
        shared.watcher_busy.remove(&index);
        if shared.keep_watcher_done != Some(id) {
            shared.calls.remove(&id);
        }
        drop(shared);
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

    /// Stops this hub for a shutdown, keeping its router: every callback in
    /// flight answers `Stopped`, every admitted `host.exec` run stops
    /// without the Lua thread, and every waiter wakes. Later deliveries
    /// still route, so a run that ends as it stops is still recorded.
    fn stop_for_shutdown(&self) {
        let unsent: Unsent = {
            let mut shared = self.lock();
            let unsent = if !matches!(shared.phase, Phase::Stopped(_)) {
                shared.stop(StopReason::Stopped).into()
            } else {
                Unsent::default()
            };
            // Every call still on the hub is cancelled with it, so `judge`
            // waits for an admitted `host.exec` run's end before answering
            // `Stopped`, as for a cancelled tool call. The hook path never
            // sees the turn's cancel itself, so the stop marks its calls.
            let started: Vec<u64> = shared
                .calls
                .iter()
                .filter_map(|(id, progress)| {
                    matches!(progress, Progress::Started { .. }).then_some(*id)
                })
                .collect();
            shared.cancelled.extend(started);
            unsent
        };
        drop(unsent);
        self.notify();
    }

    /// Waits until no watcher delivery is queued or running on this hub:
    /// the drain's pre-replay wait, so a durable delivery the live offer
    /// already covered has run before the drain treats it as covered. A
    /// stop wakes it too: what never runs is then refused as `Stopped` by
    /// the drain's own push, never lost silent.
    pub(super) fn wait_for_watchers_idle(&self) {
        let mut shared = self.lock();
        #[cfg(test)]
        let mut signalled = false;
        while !shared.watcher_idle() && !matches!(shared.phase, Phase::Stopped(_)) {
            #[cfg(test)]
            if !signalled {
                signalled = true;
                self.at_window(Window::DrainWait);
            }
            shared = self.wait(shared, None);
        }
    }

    /// Closes admission and waits until every admitted exec has queued its line.
    pub(super) fn wait_for_execs(&self) {
        // Empty is final only after the same gate that admits runs closes.
        self.stop_for_shutdown();
        let mut shared = self.lock();
        while !shared.execs.is_empty() {
            shared = self.wait(shared, None);
        }
    }

    /// Records the extension's drop under the same lock that routes
    /// deliveries: anything routed after this is dropped. A drop cancels
    /// no delegate: the session's end owns stopping those.
    pub(crate) fn dispose(&self, _name: &str) {
        let unsent: Unsent = {
            let mut shared = self.lock();
            shared.router.dispose();
            if !matches!(shared.phase, Phase::Stopped(_)) {
                shared.stop(StopReason::Stopped).into()
            } else {
                Unsent::default()
            }
        };
        drop(unsent);
        self.notify();
    }

    /// Records the delegate `id` against its `run` handle and the
    /// per-call timeout the `host.delegate` yield carried: the service
    /// calls `bind` before any child spawns, under the registry's start
    /// gate, so an abandon after this cancels it. The timeout is judged
    /// from the stream item's push instant.
    pub(crate) fn bind_delegate(&self, id: contract::JobId, handle: u64, timeout: Duration) {
        let mut shared = self.lock();
        shared.delegates.insert(id, (handle, timeout));
    }

    /// Forgets the delegate `id`: its start failed after `bind` ran, so no
    /// abandon may cancel it.
    pub(crate) fn unbind_delegate(&self, id: &contract::JobId) {
        let mut shared = self.lock();
        shared.delegates.remove(id);
    }

    /// Holds `finished` for its job when this extension binds it: the
    /// completion pushes the `run` item, so a finish alone runs nothing.
    /// False when no binding names the job: a model-owned delegate's
    /// finish passes through.
    pub(crate) fn hold_delegate_finish(
        &self,
        finished: &contract::events::DelegateFinished,
    ) -> bool {
        let mut shared = self.lock();
        if !shared.delegates.contains_key(&finished.job_id) {
            return false;
        }
        shared
            .held_finishes
            .insert(finished.job_id.clone(), finished.clone());
        true
    }

    /// Pushes the one stream item for `completed` when this extension
    /// still binds its job, consuming the binding and any held finish: a
    /// second completion for the same id pushes nothing. Refused while the
    /// extension is not running: nothing is queued. The caller drops the
    /// guard before `run` executes.
    pub(crate) fn push_delegate_run(
        &self,
        name: &str,
        completed: &contract::events::JobCompleted,
    ) -> bool {
        let mut shared = self.lock();
        if !matches!(shared.phase, Phase::Ready(_)) {
            return false;
        }
        let Some((handle, timeout)) = shared.delegates.remove(&completed.job_id) else {
            return false;
        };
        let finished = shared.held_finishes.remove(&completed.job_id);
        let asked = self.clock.now();
        shared.push(
            super::Target::Delegate {
                extension: name.to_owned(),
                job_id: completed.job_id.clone(),
                handle,
                timeout,
            },
            super::delegates::run_arg(completed, finished.as_ref()),
            asked,
        );
        drop(shared);
        self.notify();
        true
    }

    /// Forgets the bindings for `jobs`, with their held finishes
    /// and queued-but-unstarted `run` items: after the normal end releases
    /// an id, its completion queues nothing and its queued item never
    /// starts. A `run` already on the thread runs out its timeout.
    pub(crate) fn release_delegates(&self, jobs: &[contract::JobId]) {
        let mut shared = self.lock();
        for id in jobs {
            shared.delegates.remove(id);
            shared.held_finishes.remove(id);
        }
        let dropped: Vec<u64> = shared
            .queue
            .iter()
            .filter(|job| {
                matches!(&job.target, super::Target::Delegate { job_id, .. } if jobs.contains(job_id))
            })
            .map(|job| job.id)
            .collect();
        for id in dropped {
            shared.forget(id);
        }
    }

    /// Ends a delegate `run` item: no caller waits on it, so the call is
    /// forgotten rather than recorded; a failure is noticed as
    /// `hook_failed` naming the extension, and the `run` handle is queued
    /// for the thread to clear from the VM beside the hub entries. Runs on
    /// the extension's thread.
    pub(super) fn finish_delegate(
        &self,
        name: &str,
        id: u64,
        target: &super::Target,
        result: Result<Value, Error>,
    ) {
        let super::Target::Delegate { job_id, handle, .. } = target else {
            return;
        };
        if let Err(e) = &result {
            self.emit(Event::Notice(Notice {
                code: ErrorCode::HookFailed,
                message: format!("The delegate `run` failed, so its result was dropped: {e}"),
                extension: Some(name.to_owned()),
            }));
        }
        let mut shared = self.lock();
        // The thread clears the `run` handle from the VM after the call,
        // beside the timer functions: the end consumed it.
        shared.delegate_cleanup.push(*handle);
        // The push consumed the binding; this clears what a push that
        // never ran (a stop between the two) left behind.
        shared.delegates.remove(job_id);
        shared.held_finishes.remove(job_id);
        shared.forget(id);
        drop(shared);
        self.notify();
    }

    /// Cancels the delegate `id` when the extension was abandoned while its
    /// start ran: an abandon between `bind` and the job's record found no
    /// job to stop, so the post-start check ends it. A hub stopped any
    /// other way keeps its delegate running.
    pub(crate) fn cancel_delegate_if_abandoned(
        &self,
        service: &Arc<dyn contract::extension::HostDelegate>,
        id: &contract::JobId,
    ) {
        let abandoned = self.lock().abandoned;
        if abandoned {
            service.cancel(std::slice::from_ref(id));
        }
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
    pub(super) state: Arc<crate::host::state::ExtensionState>,
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
    /// The `host.model` cancel of each call with one in flight, by call id.
    /// Ended when the callback ends, the call is cancelled, or the hub stops.
    pub(super) models: HashMap<u64, Arc<super::host_calls::ModelCancel>>,
    /// The admitted command whose inner call is running, from `taken` to
    /// the synchronous answer or drop. Checked only under the hub lock.
    pub(super) serving: Option<u64>,
    /// The inner-call runner of each admitted command or running tool call, by call id, so a
    /// `host.tool` yield admits only while its outer call runs.
    pub(super) runners: HashMap<u64, Arc<dyn InnerCalls>>,
    /// Raised into running Lua by the deadline hook while a cancelled call
    /// runs; set and cleared only under this lock.
    pub(super) interrupt: Arc<std::sync::atomic::AtomicBool>,
    /// A hook or command whose caller took `Timeout` at its deadline while it still
    /// ran on the thread. The worker may be stuck inside the VM — that is
    /// when abandonment matters — so the next judge to see it past its
    /// grace abandons the VM (the lane's choice: the next judge, not the
    /// worker). One slot is enough: only one call runs on the thread at a
    /// time, and parking or settling the watched hook disarms it.
    pub(super) abandon: Option<AbandonWatch>,
    /// Each started delegate job against its `run` handle and the per-call
    /// timeout the `host.delegate` yield carried, inserted by `bind` before
    /// any child spawns. A failed start after `bind` removes its entry; an
    /// abandon takes every entry to cancel; the push of its `run` item
    /// consumes it, so a second completion pushes nothing. The timeout is
    /// judged from the stream item's push instant.
    pub(super) delegates: HashMap<contract::JobId, (u64, Duration)>,
    /// The `run` handles whose entries the extension's thread clears from
    /// the VM after each delegate end, beside the timer functions: the end
    /// consumed them.
    pub(super) delegate_cleanup: Vec<u64>,
    /// Each bound delegate job's finish, held from `delegate_finished`
    /// until its `job_completed` pushes the `run` item. A finish for a job
    /// no extension binds is never held.
    pub(super) held_finishes: HashMap<contract::JobId, contract::events::DelegateFinished>,
    /// Set by `abandon`: a leaked thread of this VM may still run Lua, and
    /// its `host.delegate` is refused from here on.
    pub(super) abandoned: bool,
    /// The watchers with a delivery queued or running, by watcher index:
    /// an ephemeral offer skips while its watcher is here. Set when the
    /// thread starts or resumes the delivery, cleared when it finishes.
    /// Durable offers always queue, whatever is here.
    pub(super) watcher_busy: HashSet<usize>,
    /// The drain's call id while its finished watcher delivery is kept
    /// for the drain to collect instead of dropping it. Only the
    /// session-end drain sets it, when no mid-session delivery waits on
    /// anything — and only for its own call, so a live delivery finishing
    /// under it still notices its failure instead of being retained with
    /// no waiter to collect it.
    pub(super) keep_watcher_done: Option<u64>,
}

/// What a waiter drops after it releases the hub lock: the deliveries
/// stopping declined, and the delegate jobs an abandon took to cancel.
/// Only an abandon fills the ids, and every abandon answers, so the arms
/// that drop theirs while they still hold the lock always carry none:
/// dropping one there sends no cancel.
#[derive(Default)]
pub(super) struct Unsent {
    deliveries: Vec<Delivery>,
    service: Option<Arc<dyn contract::extension::HostDelegate>>,
    ids: Vec<contract::JobId>,
}

impl From<Vec<Delivery>> for Unsent {
    fn from(deliveries: Vec<Delivery>) -> Self {
        Self {
            deliveries,
            service: None,
            ids: Vec::new(),
        }
    }
}

impl Drop for Unsent {
    fn drop(&mut self) {
        // Every arm that carries ids drops the guard first, so the cancel
        // never runs under the hub lock; the service takes no lock of its
        // own while the guard is held. The deliveries drop with the value.
        let deliveries = std::mem::take(&mut self.deliveries);
        let service = self.service.take();
        let ids = std::mem::take(&mut self.ids);
        drop(deliveries);
        if !ids.is_empty()
            && let Some(service) = service
        {
            service.cancel(&ids);
        }
    }
}

/// A hook or command still on the thread after its caller answered `Timeout` at its
/// deadline: which call to abandon, and the deadline its grace runs from.
pub(super) struct AbandonWatch {
    pub(super) id: u64,
    pub(super) target: Target,
    pub(super) deadline: Option<Instant>,
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

#[derive(Clone, Copy)]
pub(super) enum Run {
    Running,
    Parked,
    LoginWait,
}

pub(super) enum Progress {
    /// Waiting in the queue.
    Queued,
    /// Started, with its deadline and whether it is running or suspended.
    Started {
        deadline: Option<Instant>,
        run: Run,
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

/// A running hook, watcher or command answers at its deadline; every other target
/// waits out its grace.
fn answers_at_deadline(target: &Target) -> bool {
    matches!(
        target,
        Target::Hook { .. } | Target::Watcher { .. } | Target::Command(_)
    )
}

impl Shared {
    pub(super) fn judge_load(
        &mut self,
        name: &str,
        provider: &str,
        asked: Instant,
        now: Instant,
    ) -> (Next, Unsent) {
        if let Phase::Stopped(reason) = &self.phase {
            return (Next::Return(Err(reason.error(name))), Unsent::default());
        }
        let callback = format!("providers/{provider}.lua");
        let until = match self.loads.get(provider) {
            Some(ProviderLoad::Absent) => {
                return (Next::Return(Ok(Value::Bool(false))), Unsent::default());
            }
            Some(ProviderLoad::Loaded) => {
                return (Next::Return(Ok(Value::Bool(true))), Unsent::default());
            }
            Some(ProviderLoad::Failed(reason)) => {
                return (Next::Return(Err(reason.error(name))), Unsent::default());
            }
            Some(ProviderLoad::Queued { asked }) => asked.checked_add(LOAD_TIMEOUT),
            Some(ProviderLoad::Running { abandon_at }) => {
                if expired(*abandon_at, now) {
                    let reason = StopReason::Abandoned {
                        extension: name.to_owned(),
                        callback,
                    };
                    let error = reason.error(name);
                    let unsent = self.abandon(reason);
                    return (Next::Return(Err(error)), unsent);
                }
                return (Next::Sleep(*abandon_at), Unsent::default());
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
                Unsent::default(),
            );
        }
        let (gate, unsent) = self.gate(name, now);
        match gate {
            Gate::Stopped(e) => (Next::Return(Err(e)), unsent),
            Gate::Wait(at) => (Next::Sleep(at.into_iter().chain(until).min()), unsent),
            Gate::Ready(_) => (Next::Sleep(until), unsent),
        }
    }

    /// No watcher delivery queued or running: what the drain's pre-replay
    /// wait holds for.
    fn watcher_idle(&self) -> bool {
        self.watcher_busy.is_empty()
            && !self
                .queue
                .iter()
                .any(|job| matches!(&job.target, Target::Watcher { .. }))
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
        if matches!(&target, Target::Hook { .. }) {
            self.state.begin(id);
        }
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
                run: Run::Running,
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
    pub(super) fn gate(&mut self, name: &str, now: Instant) -> (Gate<'_>, Unsent) {
        let mut unsent = Unsent::default();
        if let Phase::Registering { abandon_at } = self.phase
            && expired(abandon_at, now)
        {
            // debt: the abandoned thread is leaked, still running, until the
            // process exits, and a credential lock its VM holds with it; Rust
            // cannot stop a thread. Cap abandoned VMs per session if leaked
            // threads or locks show (docs/performance.md).
            unsent = self
                .stop(StopReason::Abandoned {
                    extension: name.to_owned(),
                    callback: ENTRY.to_owned(),
                })
                .into();
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
    ) -> (Next, Unsent) {
        // Stopped is final for every waiter, one holding a result included.
        if let Phase::Stopped(reason) = &self.phase {
            // A stopped call the shutdown cancelled, whose admitted
            // `host.exec` run is still going, waits for the run's end, as a
            // cancelled tool call does (`judge_tool`): the shutdown already
            // stopped the run with SIGTERM, and the caller must not return
            // — the process must not exit — before the run answers. The
            // run's end notifies, and the next judge answers `Stopped`. An
            // uncancelled call still fails at once.
            if self.exec_pending(id) && self.cancelled.contains(&id) {
                return (Next::Sleep(None), Unsent::default());
            }
            let e = reason.error(name);
            self.forget(id);
            return (Next::Return(Err(e)), Unsent::default());
        }
        // A watched hook or command still running past its grace: its caller took
        // `Timeout` at its deadline and is gone, and the worker may be
        // stuck in the VM, so the next judge to see it abandons the VM
        // with the abandoned reason, as its own judge would below.
        let watch_due = self
            .abandon
            .as_ref()
            .is_some_and(|watch| expired(watch.deadline.and_then(|at| at.checked_add(GRACE)), now));
        if watch_due && let Some(watch) = self.abandon.take() {
            let reason = StopReason::Abandoned {
                extension: name.to_owned(),
                callback: watch.target.to_string(),
            };
            let error = reason.error(name);
            let unsent = self.abandon(reason);
            return (Next::Return(Err(error)), unsent);
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
                Unsent::default(),
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
            run: Run::Running,
        }) = self.calls.get(&id)
            && answers_at_deadline(target)
            && expired(*deadline, now)
        {
            if expired(deadline.and_then(|at| at.checked_add(GRACE)), now) {
                // Still on the thread past its grace: the interrupt could
                // not stop the VM. Abandon it with the abandoned reason,
                // as for any running call past its grace below; later
                // calls get the stored error from the stopped phase above.
                self.forget(id);
                let reason = StopReason::Abandoned {
                    extension: name.to_owned(),
                    callback: target.to_string(),
                };
                let error = reason.error(name);
                let unsent = self.abandon(reason);
                return (Next::Return(Err(error)), unsent);
            }
            // A hook or command past its timeout has failed: the caller takes the
            // timeout at its deadline and the chain moves on, while the
            // worker's stop lands late and is dropped. Waiting the grace
            // would hold the answer on the worker's delivery. The watch
            // leaves the abandonment to the next judge past the grace.
            self.abandon = Some(AbandonWatch {
                id,
                target: target.clone(),
                deadline: *deadline,
            });
            self.forget(id);
            return (
                Next::Return(Err(timed_out(name, target, timeout, false))),
                unsent,
            );
        }
        let mut login = false;
        let (until, running) = match self.calls.get(&id) {
            Some(Progress::Started {
                deadline,
                run: Run::Running,
            }) if answers_at_deadline(target) => {
                // A running hook or command answers at its deadline (see above), so
                // wake there: the grace would hold the answer on a
                // withheld worker delivery.
                (*deadline, true)
            }
            Some(Progress::Started { deadline, run }) => {
                login = matches!(run, Run::LoginWait);
                (
                    deadline.and_then(|at| at.checked_add(GRACE)),
                    matches!(run, Run::Running),
                )
            }
            _ => (asked.checked_add(timeout), false),
        };
        if !expired(until, now) {
            return (Next::Sleep(until), unsent);
        }
        self.forget(id);
        if !running {
            // Queued or parked, not on the thread: failing it leaves the VM up.
            return (
                Next::Return(Err(timed_out(name, target, timeout, login))),
                unsent,
            );
        }
        // debt: as in `gate`, the abandoned thread leaks until the process exits.
        unsent = self.abandon(StopReason::Stopped);
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
    /// Every in-flight `host.model` call's cancel is set, as on the seal.
    /// Every held ask is declined by `fiber` in the same critical section;
    /// returns what could not be sent, for the caller to drop afterwards.
    pub(super) fn stop(&mut self, reason: StopReason) -> Vec<Delivery> {
        self.phase = Phase::Stopped(reason);
        self.abandon = None;
        self.replies.clear();
        self.execs.values_mut().for_each(|admit| admit.stop_take());
        self.models.values().for_each(|cancel| cancel.end());
        // stop_declines_every_held_ask: one `Resolved` per held ask.
        let held: Vec<RequestId> = self.asks.keys().cloned().collect();
        let mut unsent = Vec::new();
        for request in &held {
            unsent.extend(self.decline(request));
        }
        unsent
    }

    /// Abandons the extension with `reason` and cancels its bound
    /// delegates: a VM abandoned mid-session must not keep a delegate
    /// running on its leaked thread. Runs `stop`, marks the hub abandoned
    /// and takes every binding with the router's service, for the caller
    /// to drop after it releases the hub lock, so the cancel never runs
    /// under it.
    pub(super) fn abandon(&mut self, reason: StopReason) -> Unsent {
        let deliveries = self.stop(reason);
        self.abandoned = true;
        let ids: Vec<contract::JobId> = std::mem::take(&mut self.delegates).into_keys().collect();
        let service = if ids.is_empty() {
            None
        } else {
            self.router.delegate.clone()
        };
        Unsent {
            deliveries,
            service,
            ids,
        }
    }

    /// Drops the call `id`: its caller has stopped waiting. An in-flight
    /// `host.model` call's cancel is set: its callback ended.
    fn forget(&mut self, id: u64) {
        self.calls.remove(&id);
        self.cancelled.remove(&id);
        self.queue.retain(|job| job.id != id);
        self.end_model(id);
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
