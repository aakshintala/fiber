//! The extension's thread (`docs/extensions.md`, "How an extension runs").
//! It runs the entry script, then starts calls from the hub's queue in the
//! order Fiber asked them. A callback parked on `host.http` does not hold
//! the thread: the request runs elsewhere, and provider work runs meanwhile.
//! So does one parked on another host call (`host.oauth`) or asleep on the
//! extension's clock.
//! The next command does not: it stays queued until the parked command
//! finishes. Each parked callback still ends at its own deadline. The thread
//! quits once the extension is stopped.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use contract::events::ExtensionExec;
use contract::shapes::Process;
use mlua::Thread;

use crate::Error;
use crate::host::{self, Reply, Request, exec};
use crate::oauth::{self, Browser, Deliver};

use super::hub::{Hub, Job, Phase, Progress, Shared, not_registered, timed_out};
use super::{GRACE, LOAD_TIMEOUT, LuaExtension, Step, Target, Vm, expired};

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
}

/// What the thread does next, outside the lock.
enum Work {
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
    pub(super) load_by: Option<Instant>,
    pub(super) memory_cap: usize,
    pub(super) browser: Arc<dyn Browser>,
    pub(super) session: Option<crate::host::Session>,
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
        load_by,
        memory_cap: extension.memory_cap,
        browser: Arc::clone(&extension.browser),
        session: extension.session.clone(),
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
                shared.stop(e);
                None
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
            Work::Start(job, timeout, deadline) => {
                let step = vm.step(&job.target, &job.arg, timeout, deadline);
                (job.id, job.target, step)
            }
            Work::Resume(p, reply) => {
                let step = host::resume_values(&vm.lua, &hub.clock_handle(), reply)
                    .map_err(|e| vm.error(&e))
                    .and_then(|args| vm.after(p.thread, args, &p.target, p.timeout, p.deadline));
                (p.id, p.target.clone(), step)
            }
            Work::Collect => {
                vm.collect();
                continue;
            }
        };
        // A callback that ended in an error may have left a held credential
        // in its coroutine.
        let failed = step.is_err();
        settle(&start, &hub, &mut parked, id, &target, step);
        // A finished timer leaves its Lua function, which the thread frees.
        for timer_id in hub.take_timer_cleanup() {
            match vm.timer_funcs.set(
                i64::try_from(timer_id).unwrap_or(i64::MAX),
                mlua::Value::Nil,
            ) {
                Ok(()) | Err(_) => {}
            }
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
    loop {
        let now = hub.clock().now();
        if !matches!(shared.phase, Phase::Ready(_)) {
            return None;
        }
        if let Some(pos) = parked.iter().position(|p| expired(p.deadline, now)) {
            let p = parked.swap_remove(pos);
            if let Target::Timer { id: timer_id } = p.target {
                // A firing past its timeout ends; an `every` keeps firing.
                shared.calls.remove(&p.id);
                shared.timer_end(timer_id, now);
            } else {
                shared.finish(p.id, Err(timed_out(name, &p.target, p.timeout)));
            }
            hub.notify();
            // The coroutine goes with `p`; the VM frees what it held.
            drop(p);
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
                // The callback is gone. Dropping its reply frees a lock in it.
                continue;
            };
            let p = parked.swap_remove(pos);
            // A caller that has gone gave up on its own deadline.
            if let Some(progress) = shared.calls.get_mut(&id) {
                *progress = Progress::Started {
                    deadline: p.deadline,
                    parked: false,
                };
                return Some(Work::Resume(p, reply.unwrap_or(Reply::Slept)));
            }
            continue;
        }
        /// One ordered stream: hooks, watcher deliveries and commands form one
        /// stream, each finishing before the next starts. A queued stream job
        /// starts only when no stream job is parked and no earlier stream job
        /// in the queue is held. Provider functions still run in the gaps.
        fn is_stream(target: &Target) -> bool {
            matches!(target, Target::Command(_) | Target::Hook { .. })
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
            shared.calls.insert(
                job.id,
                Progress::Started {
                    deadline,
                    parked: false,
                },
            );
            return Some(Work::Start(job, timeout, deadline));
        }
        // Timers run in the gaps: the block above returns when a queued
        // call started, so reaching here means none could start. One firing
        // at a time.
        if let Some((timer_id, timeout)) = shared.timer_fire(now) {
            let (job, timeout, deadline) = shared.push_timer(timer_id, timeout, now);
            return Some(Work::Start(job, timeout, deadline));
        }
        let wake = parked
            .iter()
            .flat_map(|p| [p.deadline, p.wake])
            .flatten()
            .chain(shared.timer_wake())
            .min();
        shared = hub.wait(shared, wake);
    }
}

/// Records a finished callback's result, or parks it on its host call. A
/// finished timer firing ends the firing instead: its result goes nowhere,
/// and an `every` that was not cancelled fires again.
fn settle(
    start: &Start,
    hub: &Arc<Hub>,
    parked: &mut Vec<Parked>,
    id: u64,
    target: &Target,
    step: Result<Step, Error>,
) {
    let name = start.name.as_str();
    let dir = start.dir.as_path();
    let home = start.home.as_path();
    if let Target::Timer { id: timer_id } = target
        && !matches!(step, Ok(Step::Suspend { .. }))
    {
        hub.timer_call_done(id, *timer_id);
        return;
    }
    let (thread, target, deadline, timeout, request) = match step {
        Ok(Step::Done(value)) => return hub.finish(id, Ok(value)),
        Err(e) => return hub.finish(id, Err(e)),
        Ok(Step::Suspend {
            thread,
            target,
            deadline,
            timeout,
            request,
        }) => (thread, target, deadline, timeout, request),
    };
    let deliver: Deliver = {
        let hub = Arc::clone(hub);
        Arc::new(move |reply| hub.deliver(id, reply))
    };
    let (cancel, wake) = match request {
        Request::Http(request) => {
            let spawned = thread::Builder::new()
                .name(format!("http {name}"))
                .spawn(move || deliver(Reply::Http(host::perform(&request))));
            if let Err(source) = spawned {
                return hub.finish(
                    id,
                    Err(Error::Io {
                        path: dir.to_owned(),
                        source,
                    }),
                );
            }
            (None, None)
        }
        Request::Callback { port } => (oauth::listen(port, &deliver), None),
        Request::Exec(request) => {
            let hub_exec = Arc::clone(hub);
            let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
            let meta = ExecMeta {
                extension: name.to_owned(),
                program: request.program.clone(),
                args: request.args.clone(),
                cwd: request.cwd.clone(),
            };
            let clock = hub.clock_handle();
            let spawned = thread::Builder::new()
                .name(format!("exec {name}"))
                .spawn(move || {
                    let outcome = exec::run(&request, clock.as_ref(), deadline, cancel_rx);
                    match &outcome {
                        Ok(ran) => {
                            hub_exec.send(contract::inbox::Delivery::ExtensionExec(meta.exec(ran)))
                        }
                        Err(failed) => {
                            if let Some(ran) = &failed.ran {
                                hub_exec
                                    .send(contract::inbox::Delivery::ExtensionExec(meta.exec(ran)));
                            }
                        }
                    }
                    deliver(match outcome {
                        Ok(ran) => Reply::Exec(Ok(ran)),
                        Err(failed) => Reply::Exec(Err((failed.code, failed.message))),
                    });
                });
            if let Err(source) = spawned {
                return hub.finish(
                    id,
                    Err(Error::Io {
                        path: dir.to_owned(),
                        source,
                    }),
                );
            }
            (Some(cancel_tx), None)
        }
        Request::Lock => {
            let cancel = match &target {
                Target::Provider {
                    credential: Some(pair),
                    ..
                } => oauth::lock(home, pair, &deliver),
                Target::Provider {
                    name,
                    function,
                    credential: None,
                } => {
                    deliver(Reply::Lock(Err(format!(
                        "host.oauth.refresh: {name}.{function} has no credential to refresh; only credential() refreshes"
                    ))));
                    None
                }
                Target::Command(_) => {
                    deliver(Reply::Lock(Err(
                        "host.oauth.refresh: a command has no provider credential to refresh"
                            .to_owned(),
                    )));
                    None
                }
                Target::Hook { .. } => {
                    deliver(Reply::Lock(Err(
                        "host.oauth.refresh: a hook has no provider credential to refresh"
                            .to_owned(),
                    )));
                    None
                }
                Target::Timer { .. } => {
                    deliver(Reply::Lock(Err(
                        "host.oauth.refresh: a timer has no provider credential to refresh"
                            .to_owned(),
                    )));
                    None
                }
            };
            (cancel, None)
        }
        // Past the end of time is no sleep.
        Request::Sleep(d) => (None, hub.clock().now().checked_add(d)),
    };
    parked.push(Parked {
        id,
        thread,
        target,
        deadline,
        timeout,
        wake,
        _cancel: cancel,
    });
    if let Some(progress) = hub.lock().calls.get_mut(&id) {
        *progress = Progress::Started {
            deadline,
            parked: true,
        };
    }
    hub.notify();
}

/// What a finished `host.exec` run is logged as: the extension, the program
/// with its arguments and working directory, and how it ended.
struct ExecMeta {
    extension: String,
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
}

impl ExecMeta {
    fn exec(&self, ran: &exec::Ran) -> ExtensionExec {
        ExtensionExec {
            extension: self.extension.clone(),
            program: self.program.clone(),
            args: self.args.clone(),
            cwd: self.cwd.display().to_string(),
            process: Process {
                exit_code: ran.exit_code,
                signal: ran.signal.clone(),
                timed_out: ran.timed_out,
            },
        }
    }
}
