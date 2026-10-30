//! The extension's thread (`docs/extensions.md`, "How an extension runs").
//! It runs the entry script, then starts calls from the hub's queue in the
//! order Fiber asked them. A callback parked on `host.http` does not hold
//! the thread: the request runs elsewhere, and provider work runs meanwhile.
//! So does one parked on another host call (`host.oauth`) or asleep on the
//! extension's clock.
//! The next command does not: it stays queued until the parked command
//! finishes. Each parked callback still ends at its own deadline. The thread
//! quits once the extension is stopped.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use mlua::Thread;

use crate::Error;
use crate::host::{self, Reply};
use crate::oauth::{Browser, Deliver};

use super::host_calls::{Admitted, Call, admit};
#[cfg(test)]
use super::host_calls::{Pause, settle_hook};
use super::hub::{
    Hub, Job, Phase, Progress, ProviderLoad, Run, Shared, StopReason, not_registered, timed_out,
};
use super::{GRACE, LOAD_TIMEOUT, LuaExtension, Step, Target, Vm, expired};
#[cfg(test)]
use std::sync::Mutex;

/// A callback suspended on a host call. Its deadline keeps running.
struct Parked {
    id: u64,
    thread: Thread,
    target: Target,
    deadline: Option<Instant>,
    timeout: Duration,
    /// When a sleeping callback resumes.
    wake: Option<Instant>,
    /// Dropped with the callback: an off-thread wait that polls the other end
    /// stops, which frees its port or stops its contending for a lock.
    _cancel: Option<Sender<()>>,
    /// The `host.ask` question this callback waits on, if any: plain data,
    /// removed from the registry by whoever answers it first.
    ask: Option<contract::RequestId>,
}

/// What the thread does next, outside the lock.
enum Work {
    Load(String, Option<Instant>),
    Start(Job, Duration, Option<Instant>),
    Resume(Parked, Reply),
    /// A parked callback was dropped: free what it left in the VM.
    Collect,
}

/// Everything the extension's thread starts from: where it runs, its
/// bounds and its session.
pub(super) struct Start {
    pub(super) name: String,
    pub(super) dir: PathBuf,
    pub(super) home: PathBuf,
    /// Whether the VM runs the entry script: a disabled provider-only
    /// package's VM serves provider scripts but never runs `init.lua`.
    pub(super) run_entry: bool,
    pub(super) load_by: Option<Instant>,
    pub(super) memory_cap: usize,
    pub(super) browser: Arc<dyn Browser>,
    pub(super) session: Option<crate::host::Session>,
    pub(super) secrets: Vec<String>,
    pub(super) deps: BTreeMap<String, PathBuf>,
}

/// Spawns the extension's thread on the first call.
pub(super) fn start(extension: &LuaExtension, shared: &mut Shared) -> Result<(), Error> {
    if !matches!(shared.phase, Phase::Idle) {
        return Ok(());
    }
    let load_by = extension.hub.clock().now().checked_add(LOAD_TIMEOUT);
    let start = Start {
        name: extension.name.clone(),
        dir: extension.dir.clone(),
        home: extension.home.clone(),
        run_entry: extension.run_entry,
        load_by,
        memory_cap: extension.memory_cap,
        browser: Arc::clone(&extension.browser),
        session: extension.session.clone(),
        secrets: extension.secrets.clone(),
        deps: extension.deps.clone(),
    };
    let hub = Arc::clone(&extension.hub);
    thread::Builder::new()
        .name(format!("lua {}", extension.name))
        .spawn(move || {
            serve(hub, start);
        })
        .map_err(|source| Error::Io {
            path: extension.dir.clone(),
            source,
        })?;
    shared.phase = Phase::Registering {
        abandon_at: load_by.and_then(|at| at.checked_add(GRACE)),
    };
    Ok(())
}

/// Runs the entry script, under the deadline `load_by`, then serves calls
/// until the extension is stopped.
pub(super) fn serve(hub: Arc<Hub>, start: Start) {
    let loaded = Vm::load(&hub, &start);
    let vm = {
        let mut shared = hub.lock();
        if !matches!(shared.phase, Phase::Registering { .. }) {
            return;
        }
        let vm = match loaded {
            Ok(vm) => {
                let mut declared = vm.declared();
                declared.override_hooks(shared.hook_timeout);
                shared.phase = Phase::Ready(declared);
                Some(vm)
            }
            Err(e) => {
                shared.misplaced = matches!(&e, Error::Lua { message, .. }
                    if message.contains("fiber.provider:") && message.contains("not `init.lua`"));
                let unsent = shared.stop(StopReason::of(e));
                drop(shared);
                drop(unsent);
                hub.notify();
                return;
            }
        };
        hub.notify();
        vm
    };
    let Some(vm) = vm else {
        return;
    };
    let mut parked = Vec::new();
    while let Some(work) = next(&start.name, &hub, &mut parked) {
        // The call has started or resumed.
        hub.notify();
        let (id, target, step) = match work {
            Work::Load(name, deadline) => {
                let result = vm.load_provider(&name, deadline);
                let mut shared = hub.lock();
                if let Phase::Ready(timeouts) = &mut shared.phase {
                    let state = match result {
                        Ok(functions) => {
                            timeouts.providers.insert(name.clone(), functions);
                            ProviderLoad::Loaded
                        }
                        Err(e) => ProviderLoad::Failed(StopReason::of(e)),
                    };
                    shared.loads.insert(name, state);
                }
                #[cfg(test)]
                hub.at_window(super::hub::Window::LoadRecorded);
                drop(shared);
                hub.notify();
                continue;
            }
            Work::Start(job, timeout, deadline) => {
                let scope = match &job.target {
                    Target::Hook { .. } => host::state::Scope::Hook(job.id),
                    Target::Command(_)
                    | Target::Provider { .. }
                    | Target::Timer { .. }
                    | Target::Tool(_)
                    | Target::Search(_)
                    | Target::Watcher { .. }
                    | Target::Delegate { .. }
                    | Target::Effects(_) => host::state::Scope::Other,
                };
                vm.state.enter(scope);
                let step = vm.step(&job.target, &job.arg, timeout, deadline);
                vm.state.leave();
                (job.id, job.target, step)
            }
            Work::Resume(p, reply) => {
                let scope = match &p.target {
                    Target::Hook { .. } => host::state::Scope::Hook(p.id),
                    Target::Command(_)
                    | Target::Provider { .. }
                    | Target::Timer { .. }
                    | Target::Tool(_)
                    | Target::Search(_)
                    | Target::Watcher { .. }
                    | Target::Delegate { .. }
                    | Target::Effects(_) => host::state::Scope::Other,
                };
                vm.state.enter(scope);
                let step = host::resume_values(&vm.lua, &hub.clock_handle(), reply)
                    .map_err(|e| vm.error(&e))
                    .and_then(|args| vm.after(p.thread, args, &p.target, p.timeout, p.deadline));
                vm.state.leave();
                (p.id, p.target.clone(), step)
            }
            Work::Collect => {
                vm.collect();
                continue;
            }
        };
        #[cfg(test)]
        if matches!(&target, Target::Timer { .. }) && !matches!(&step, Ok(Step::Suspend { .. })) {
            settle_hook(&target, Pause::TimerDone);
        }
        // A callback that ended in an error may have left a held credential
        // in its coroutine.
        let failed = step.is_err();
        settle(&start, &hub, &mut parked, id, &target, step);
        // A finished timer leaves its Lua function, and a finished delegate
        // run its stored pair, which the thread frees.
        for timer_id in hub.take_timer_cleanup() {
            match vm.timer_funcs.set(
                i64::try_from(timer_id).unwrap_or(i64::MAX),
                mlua::Value::Nil,
            ) {
                Ok(()) | Err(_) => {}
            }
        }
        for handle in hub.take_delegate_cleanup() {
            vm.clear_delegate_handle(handle);
        }
        if failed {
            vm.collect();
        }
    }
    // Stopped: no callback waits on a queued reply any more, and dropping it
    // releases a credential lock in it.
    hub.lock().replies.clear();
}

/// Waits for the next thing to do: a parked callback past its deadline is
/// failed here, a reply or a sleeper's wake resumes its callback, a queued
/// call starts if it may, and a due timer fires in the gaps. None once the
/// extension is stopped.
fn next(name: &str, hub: &Hub, parked: &mut Vec<Parked>) -> Option<Work> {
    let mut shared = hub.lock();
    // Nothing runs here: an interrupt raised for the callback that just
    // ended must not stop the next one.
    shared.interrupt.store(false, Ordering::SeqCst);
    loop {
        let now = hub.clock().now();
        if !matches!(shared.phase, Phase::Ready(_)) {
            return None;
        }
        if let Some(provider) = shared.pending_loads.pop_front()
            && let Some(ProviderLoad::Queued { asked }) = shared.loads.get(&provider)
        {
            let deadline = asked.checked_add(LOAD_TIMEOUT);
            if expired(deadline, now) {
                shared.pending_loads.push_front(provider);
            } else {
                shared.loads.insert(
                    provider.clone(),
                    ProviderLoad::Running {
                        abandon_at: deadline.and_then(|at| at.checked_add(GRACE)),
                    },
                );
                return Some(Work::Load(provider, deadline));
            }
        }
        if let Some(pos) = parked.iter().position(|p| shared.cancelled.contains(&p.id)) {
            // A cancelled callback goes, and its host call with it. The call
            // ends once no admitted `host.exec` run is going; a run still
            // going ends it when it delivers.
            let p = parked.swap_remove(pos);
            let unsent = p.ask.as_ref().and_then(|request| shared.decline(request));
            if !shared.exec_pending(p.id) {
                shared.end_cancelled(p.id);
            }
            hub.notify();
            drop(p);
            drop(shared);
            drop(unsent);
            return Some(Work::Collect);
        }
        if let Some(pos) = parked.iter().position(|p| expired(p.deadline, now)) {
            let p = parked.swap_remove(pos);
            // An ask whose callback timed out is declined by `fiber`,
            // whatever the target, before the timer and non-timer
            // branches below.
            let unsent = p.ask.as_ref().and_then(|request| shared.decline(request));
            if let Target::Delegate { .. } = p.target {
                // A delegate `run` has no waiter: its timeout ends the
                // item and is noticed as `hook_failed`, like a watcher.
                let result = Err(timed_out(name, &p.target, p.timeout, false));
                drop(shared);
                hub.finish_delegate(name, p.id, &p.target, result);
                shared = hub.lock();
            } else if let Target::Timer { id: timer_id } = p.target {
                // A firing past its timeout ends; an `every` keeps firing.
                shared.calls.remove(&p.id);
                shared.timers.end_firing(timer_id, now);
            } else if let Target::Watcher { index } = p.target {
                // A watcher delivery has no waiter: its timeout is a
                // notice naming the extension, and its result is dropped.
                let result = Err(timed_out(name, &p.target, p.timeout, false));
                drop(shared);
                hub.finish_watcher(name, p.id, index, result);
                shared = hub.lock();
            } else {
                let login = matches!(
                    shared.calls.get(&p.id),
                    Some(Progress::Started {
                        run: Run::LoginWait,
                        ..
                    })
                );
                shared.finish(p.id, Err(timed_out(name, &p.target, p.timeout, login)));
            }
            hub.notify();
            // The coroutine goes with `p`; the VM frees what it held.
            drop(p);
            drop(shared);
            drop(unsent);
            return Some(Work::Collect);
        }
        let due = shared.replies.pop().map(|(id, reply)| (id, Some(reply)));
        let due = due.or_else(|| {
            let pos = parked
                .iter()
                .position(|p| p.wake.is_some_and(|wake| now >= wake))?;
            parked.get(pos).map(|p| (p.id, None))
        });
        if let Some((id, reply)) = due {
            let Some(pos) = parked.iter().position(|p| p.id == id) else {
                // The callback is gone. Dropping its reply frees a lock in
                // it. A cancelled `host.exec` run ends its call here.
                if shared.end_cancelled(id) {
                    hub.notify();
                }
                continue;
            };
            let p = parked.swap_remove(pos);
            // A caller that has gone gave up on its own deadline.
            if let Some(progress) = shared.calls.get_mut(&id) {
                *progress = Progress::Started {
                    deadline: p.deadline,
                    run: Run::Running,
                };
                return Some(Work::Resume(p, reply.unwrap_or(Reply::Slept)));
            }
            continue;
        }
        /// One ordered stream: hooks, watcher deliveries, commands and
        /// delegate `run` deliveries form one stream, each finishing
        /// before the next starts. A queued stream job starts only when no
        /// stream job is parked and no earlier stream job in the queue is
        /// held. Provider functions still run in the gaps.
        fn is_stream(target: &Target) -> bool {
            matches!(
                target,
                Target::Command(_)
                    | Target::Hook { .. }
                    | Target::Watcher { .. }
                    | Target::Delegate { .. }
            )
        }
        let stream_busy = parked.iter().any(|p| is_stream(&p.target));
        let mut blocked = false;
        let mut startable = None;
        for (pos, job) in shared.queue.iter().enumerate() {
            if job.held {
                // A held command does not start, and a held stream job blocks
                // every stream job behind it; providers still run.
                if is_stream(&job.target) {
                    blocked = true;
                }
                continue;
            }
            if is_stream(&job.target) {
                if stream_busy || blocked {
                    continue;
                }
                startable = Some(pos);
                break;
            }
            startable = Some(pos);
            break;
        }
        if let Some(pos) = startable
            && let Some(job) = shared.queue.remove(pos)
        {
            let declared = if let Phase::Ready(timeouts) = &shared.phase {
                timeouts.timeout(&job.target)
            } else {
                None
            };
            let Some(timeout) = declared else {
                shared.finish(job.id, Err(not_registered(name, &job.target)));
                hub.notify();
                continue;
            };
            let deadline = job.asked.checked_add(timeout);
            // A starting watcher delivery marks its watcher busy, so an
            // ephemeral offer skips until it finishes; parking keeps the
            // mark, and only the finish clears it.
            if let Target::Watcher { index } = &job.target {
                shared.watcher_busy.insert(*index);
            }
            shared.calls.insert(
                job.id,
                Progress::Started {
                    deadline,
                    run: Run::Running,
                },
            );
            return Some(Work::Start(job, timeout, deadline));
        }
        // Timers run in the gaps: the block above returns when a queued
        // call started, so reaching here means none could start. One firing
        // at a time.
        if let Some((timer_id, timeout)) = shared.timers.fire(now) {
            let (job, timeout, deadline) = shared.push_timer(timer_id, timeout, now);
            return Some(Work::Start(job, timeout, deadline));
        }
        let wake = parked
            .iter()
            .flat_map(|p| [p.deadline, p.wake])
            .flatten()
            .chain(shared.timers.next_due())
            .min();
        shared = hub.wait(shared, wake);
    }
}

/// Parks the callback and marks it parked: the admission lock when the arm
/// still holds it, a fresh `hub.lock()` once the arm dropped it. Parked
/// before the spawn, under the admission lock, so a reply arriving from a
/// spawn that answers in-process never lands on an unparked id.
#[allow(clippy::too_many_arguments, reason = "one park for every arm")]
fn park(
    parked: &mut Vec<Parked>,
    shared: &mut Shared,
    id: u64,
    thread: Thread,
    target: Target,
    deadline: Option<Instant>,
    timeout: Duration,
    wake: Option<Instant>,
    cancel: Option<Sender<()>>,
    ask: Option<contract::RequestId>,
    run: Run,
) {
    parked.push(Parked {
        id,
        thread,
        target: target.clone(),
        deadline,
        timeout,
        wake,
        _cancel: cancel,
        ask,
    });
    if let Some(progress) = shared.calls.get_mut(&id) {
        *progress = Progress::Started { deadline, run };
    }
    // A hook that suspends after its caller took `Timeout` is parked, not
    // running: it ends at its own deadline without abandoning the VM.
    if shared.abandon.as_ref().is_some_and(|watch| watch.id == id) {
        shared.abandon = None;
    }
}

/// Ends a spawn that failed after the park: drops exactly the callback
/// just parked, so nothing later resumes it, and fails the call as the
/// step's own I/O error. An exec spawn failure also clears its admission,
/// so a cancelled call never waits on a run that never started.
fn spawn_failed(
    hub: &Arc<Hub>,
    parked: &mut Vec<Parked>,
    mut shared: std::sync::MutexGuard<'_, Shared>,
    id: u64,
    dir: &std::path::Path,
    source: std::io::Error,
    exec: bool,
) {
    // The spawn failed after the park: drop exactly the callback just
    // parked, so nothing later resumes it.
    take_parked(parked, id);
    if exec {
        shared.finish_exec(id);
    }
    drop(shared);
    hub.finish(
        id,
        Err(Error::Io {
            path: dir.to_owned(),
            source,
        }),
    );
}

/// A test's withheld hook or command completion: the extension and the hook
/// point or command whose finished callbacks wait, and the release the test
/// drops to let them through. No public seam reaches a worker's completion, so an
/// integration test cannot withhold it: this pause is `cfg(test)` for the
/// in-crate repro, which states that reason here. The worker blocks holding
/// no lock, so the test stays free to advance the clock and release it.
/// Dropping the sender releases every waiter at once, including when the
/// test panics, so a withheld worker never hangs the suite.
#[cfg(test)]
struct Withheld {
    extension: String,
    target: WithheldTarget,
    release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
}

/// Which finished callbacks a `Withheld` pause holds: one hook point, one
/// command name, or one watcher's index. One field holds either, so an
/// armed pause can never name more than one of them.
#[cfg(test)]
enum WithheldTarget {
    Hook(String),
    Command(String),
    Watcher(usize),
}

#[cfg(test)]
static WITHHELD: Mutex<Option<Withheld>> = Mutex::new(None);

/// Withholds the finished callbacks of `extension`'s hooks at `point` until
/// the test drops the matching sender and clears this. Narrow: only that
/// extension's hooks at that point wait, so other tests' workers pass
/// through while it is armed.
#[cfg(test)]
pub(super) fn withhold_hook_completion(
    extension: &str,
    point: &str,
    release: std::sync::mpsc::Receiver<()>,
) {
    *support::lock(&WITHHELD) = Some(Withheld {
        extension: extension.to_owned(),
        target: WithheldTarget::Hook(point.to_owned()),
        release: Arc::new(Mutex::new(release)),
    });
}

/// Withholds the finished commands of `extension`'s `command` until the test
/// drops the matching sender and clears this. Narrow: only that extension's
/// calls of that command wait, so other tests' workers pass through while
/// it is armed.
#[cfg(test)]
pub(super) fn withhold_command_completion(
    extension: &str,
    command: &str,
    release: std::sync::mpsc::Receiver<()>,
) {
    *support::lock(&WITHHELD) = Some(Withheld {
        extension: extension.to_owned(),
        target: WithheldTarget::Command(command.to_owned()),
        release: Arc::new(Mutex::new(release)),
    });
}

/// Withholds the finished watcher callbacks of `extension`'s watcher
/// `index` until the test drops the matching sender and clears this.
/// Narrow: only that extension's watcher waits, so other tests' workers
/// pass through while it is armed.
#[cfg(test)]
pub(super) fn withhold_watcher_completion(
    extension: &str,
    index: usize,
    release: std::sync::mpsc::Receiver<()>,
) {
    *support::lock(&WITHHELD) = Some(Withheld {
        extension: extension.to_owned(),
        target: WithheldTarget::Watcher(index),
        release: Arc::new(Mutex::new(release)),
    });
}

/// Stops withholding hook, command or watcher completions.
#[cfg(test)]
pub(super) fn release_hook_completion() {
    *support::lock(&WITHHELD) = None;
}

/// Waits for the test's release when `target` is a withheld hook or command
/// completion, and passes everything else through.
#[cfg(test)]
fn await_completion_release(extension: &str, target: &Target) {
    let waiter = support::lock(&WITHHELD)
        .as_ref()
        .filter(|withheld| {
            withheld.extension == extension
                && match (&withheld.target, target) {
                    (WithheldTarget::Hook(point), Target::Hook { point: held, .. }) => {
                        point == held
                    }
                    (WithheldTarget::Command(command), Target::Command(held)) => command == held,
                    (WithheldTarget::Watcher(index), Target::Watcher { index: held }) => {
                        index == held
                    }
                    _ => false,
                }
        })
        .map(|withheld| Arc::clone(&withheld.release));
    if let Some(waiter) = waiter {
        let _ignored = support::lock(&waiter).recv();
    }
}

/// Records a finished callback's result, or suspends it on its host call.
/// A finished timer firing ends the firing instead: its result goes nowhere,
/// and an `every` that was not cancelled fires again.
///
/// `admit` says what each suspended callback waits on; this suspends it
/// under one lock hold, starts any off-thread work only after, and drops
/// an unsent ask delivery once the lock is released.
fn settle(
    start: &Start,
    hub: &Arc<Hub>,
    parked: &mut Vec<Parked>,
    id: u64,
    target: &Target,
    step: Result<Step, Error>,
) {
    let dir = start.dir.as_path();
    if let Target::Timer { id: timer_id } = target
        && !matches!(step, Ok(Step::Suspend { .. }))
    {
        hub.timer_call_done(id, *timer_id);
        return;
    }
    let (thread, target, deadline, timeout, request) = match step {
        Ok(Step::Done(value)) => {
            // A delegate `run` has no waiter: its result is dropped, and
            // a failure is noticed as `hook_failed`, like a watcher.
            if let Target::Delegate { .. } = target {
                hub.finish_delegate(&start.name, id, target, Ok(value));
                return;
            }
            #[cfg(test)]
            await_completion_release(&start.name, target);
            // A watcher delivery has no waiter: a failure is a notice
            // naming the extension, and its result is dropped.
            if let Target::Watcher { index } = target {
                hub.finish_watcher(&start.name, id, *index, Ok(value));
                return;
            }
            return hub.finish(id, Ok(value));
        }
        Err(e) => {
            if let Target::Delegate { .. } = target {
                hub.finish_delegate(&start.name, id, target, Err(e));
                return;
            }
            #[cfg(test)]
            await_completion_release(&start.name, target);
            if let Target::Watcher { index } = target {
                hub.finish_watcher(&start.name, id, *index, Err(e));
                return;
            }
            return hub.finish(id, Err(e));
        }
        Ok(Step::Suspend {
            thread,
            target,
            deadline,
            timeout,
            request,
        }) => (thread, target, deadline, timeout, request),
    };
    #[cfg(test)]
    settle_hook(&target, Pause::Admission);
    // The eligible HTTP observation is snapshotted before admission, as
    // owned context: no timer starts and nothing is written until the
    // admitted worker executes. Every other host call carries none.
    let http = match &request {
        host::Request::Http(_) => super::http_observation(hub, &target),
        host::Request::Ask(_)
        | host::Request::Tool(_)
        | host::Request::Drive(_)
        | host::Request::Model(_)
        | host::Request::Delegate { .. }
        | host::Request::Exec(_)
        | host::Request::Callback { .. }
        | host::Request::Lock
        | host::Request::Sleep(_) => None,
    };
    // Read before the admission lock below takes it.
    let driver = hub.driver();
    let model = hub.model();
    let delegate = hub.delegate();
    // Every host call is admitted here: a stopped extension, whose callback
    // may still run on an abandoned thread, or a cancelled call starts
    // nothing. Work that spawns starts under this lock hold; the rest drops
    // it first and is stopped with its parked callback.
    let Some(shared) = hub.admit(id) else {
        return;
    };
    let deliver: Deliver = {
        let hub = Arc::clone(hub);
        Arc::new(move |reply| hub.deliver(id, reply))
    };
    let call = Call {
        id,
        target: &target,
        deadline,
        http,
    };
    let run = if request.waits_on_login()
        && matches!(
            target,
            Target::Provider {
                credential: Some(super::CredentialFor::Stored(_)),
                ..
            }
        ) {
        Run::LoginWait
    } else {
        Run::Parked
    };
    let admitted = admit(
        start, hub, shared, call, request, driver, model, delegate, &deliver,
    );
    let (mut shared, wake, cancel, ask, unsent, spawn) = match admitted {
        Admitted::Asked {
            shared,
            request,
            unsent,
        } => (shared, None, None, Some(request), unsent, None),
        Admitted::Spawning { shared, spawn } => (shared, None, None, None, None, Some(spawn)),
        Admitted::Replied(reply) => {
            deliver(reply);
            (hub.lock(), None, None, None, None, None)
        }
        Admitted::Started(cancel) => (hub.lock(), None, Some(cancel), None, None, None),
        Admitted::Sleep(wake) => (hub.lock(), Some(wake), None, None, None, None),
        Admitted::Nothing => (hub.lock(), None, None, None, None, None),
    };
    // Test-only: the spawn-failure seam fires only for spawned work of
    // the armed provider. Checked before `park` moves the target.
    #[cfg(test)]
    let fail_spawn = spawn.as_ref().is_some_and(|_| take_spawn_failure(&target));
    park(
        parked,
        &mut shared,
        id,
        thread,
        target,
        deadline,
        timeout,
        wake,
        cancel,
        ask,
        run,
    );
    if let Some(spawn) = spawn {
        // Test-only: no public API can make the spawn fail, so the
        // spawn-failure path takes this seam.
        #[cfg(test)]
        if fail_spawn {
            spawn_failed(
                hub,
                parked,
                shared,
                id,
                dir,
                std::io::Error::other("the test failed the spawn"),
                spawn.exec,
            );
            return;
        }
        if let Err(source) = spawn.builder.spawn(spawn.work) {
            spawn_failed(hub, parked, shared, id, dir, source, spawn.exec);
            return;
        }
    }
    drop(shared);
    #[cfg(any(test, feature = "test-hooks"))]
    if matches!(run, Run::LoginWait) {
        crate::login_wait_recorded(&start.name);
    }
    hub.notify();
    drop(unsent);
}

/// Forces the next spawned off-thread work of `provider` to fail as a
/// spawn failure. `cfg(test)`: no public API can make the spawn fail, so
/// the spawn-failure path needs this seam. Scoped by provider name, so
/// parallel tests never trip it; fires once per arming.
#[cfg(test)]
static FAIL_SPAWN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Arms the spawn-failure seam for the next spawned work of `provider`.
#[cfg(test)]
pub(crate) fn fail_next_spawn_for(provider: &str) {
    *support::lock(&FAIL_SPAWN) = Some(provider.to_owned());
}

/// Takes a pending spawn failure for `target`, disarming the seam when it
/// fires. Any other target passes through and leaves the arming in place.
#[cfg(test)]
fn take_spawn_failure(target: &Target) -> bool {
    let armed = support::lock(&FAIL_SPAWN).clone();
    match (armed, target) {
        (Some(provider), Target::Provider { name, .. }) if *name == provider => {
            *support::lock(&FAIL_SPAWN) = None;
            true
        }
        _ => false,
    }
}

/// Drops the parked callback `id`, when it is still parked, and reports
/// whether it was there: a failed drive spawn drops exactly what `settle`
/// just parked, so no later reply resumes it and the thread frees what it
/// held. Only that entry goes; every other parked callback keeps its
/// deadline.
fn take_parked(parked: &mut Vec<Parked>, id: u64) -> bool {
    let Some(pos) = parked.iter().position(|p| p.id == id) else {
        return false;
    };
    parked.swap_remove(pos);
    true
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
