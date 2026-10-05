//! The extension's thread (`docs/extensions.md`, "How an extension runs").
//! It runs the entry script, then starts calls from the hub's queue in the
//! order Fiber asked them. A callback parked on `host.http` does not hold
//! the thread: the request runs elsewhere, and provider work runs meanwhile.
//! So does one parked on another host call (`host.oauth`) or asleep on the
//! extension's clock.
//! The next command does not: it stays queued until the parked command
//! finishes. Each parked callback still ends at its own deadline. The thread
//! quits once the extension is stopped.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use mlua::Thread;

use crate::Error;
use crate::host::{self, Reply, Request};
use crate::oauth::{self, Browser, Deliver};

use super::hub::{Hub, Job, Phase, Progress, not_registered, timed_out};
use super::{Step, Target, Vm, expired};

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

/// Runs the entry script, under the deadline `load_by`, then serves calls
/// until the extension is stopped.
pub(super) fn serve(
    name: &str,
    dir: &Path,
    home: &Path,
    hub: &Arc<Hub>,
    load_by: Option<Instant>,
    memory_cap: usize,
    browser: Arc<dyn Browser>,
) {
    let loaded = Vm::load(
        name,
        dir,
        home,
        hub.clock_handle(),
        load_by,
        memory_cap,
        browser,
    );
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
    while let Some(work) = next(name, hub, &mut parked) {
        // The call has started or resumed.
        hub.notify();
        let (id, step) = match work {
            Work::Start(job, timeout, deadline) => {
                let step = vm.step(&job.target, &job.arg, timeout, deadline);
                (job.id, step)
            }
            Work::Resume(p, reply) => {
                let step = host::resume_values(&vm.lua, &hub.clock_handle(), reply)
                    .map_err(|e| vm.error(&e))
                    .and_then(|args| vm.after(p.thread, args, &p.target, p.timeout, p.deadline));
                (p.id, step)
            }
            Work::Collect => {
                vm.collect();
                continue;
            }
        };
        // A callback that ended in an error may have left a held credential
        // in its coroutine.
        let failed = step.is_err();
        settle(name, dir, home, hub, &mut parked, id, step);
        if failed {
            vm.collect();
        }
    }
    // Stopped: no callback waits on a queued reply any more, and dropping it
    // releases a credential lock in it.
    hub.lock().replies.clear();
}

/// Waits for the next thing to do: a parked callback past its deadline is
/// failed here, a reply or a sleeper's wake resumes its callback, and a
/// queued call starts if it may. None once the extension is stopped.
fn next(name: &str, hub: &Hub, parked: &mut Vec<Parked>) -> Option<Work> {
    let mut shared = hub.lock();
    loop {
        let now = hub.clock().now();
        if !matches!(shared.phase, Phase::Ready(_)) {
            return None;
        }
        if let Some(pos) = parked.iter().position(|p| expired(p.deadline, now)) {
            let p = parked.swap_remove(pos);
            shared.finish(p.id, Err(timed_out(name, &p.target, p.timeout)));
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
        let command_parked = parked
            .iter()
            .any(|p| matches!(p.target, Target::Command(_)));
        let startable = shared
            .queue
            .iter()
            .position(|job| !(command_parked && matches!(job.target, Target::Command(_))));
        if let Some(job) = startable.and_then(|pos| shared.queue.remove(pos)) {
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
        let wake = parked
            .iter()
            .flat_map(|p| [p.deadline, p.wake])
            .flatten()
            .min();
        shared = hub.wait(shared, wake);
    }
}

/// Records a finished callback's result, or parks it on its host call.
fn settle(
    name: &str,
    dir: &Path,
    home: &Path,
    hub: &Arc<Hub>,
    parked: &mut Vec<Parked>,
    id: u64,
    step: Result<Step, Error>,
) {
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
        Request::Lock => {
            let cancel = match &target {
                Target::Provider { name, .. } => oauth::lock(home, name, &deliver),
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
