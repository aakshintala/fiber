//! The extension's thread (`docs/extensions.md`, "How an extension runs").
//! It runs the entry script, then starts calls from the hub's queue in the
//! order Fiber asked them. A callback parked on `host.http` does not hold
//! the thread: the request runs elsewhere, and provider work runs meanwhile.
//! The next command does not: it stays queued until the parked command
//! finishes. Each parked callback still ends at its own deadline. The thread
//! quits once the extension is stopped.

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use mlua::Thread;

use crate::Error;
use crate::host;

use super::hub::{HttpResult, Hub, Job, Phase, Progress, not_registered, timed_out};
use super::{Step, Target, Vm, expired};

/// A callback suspended on `host.http`. Its deadline keeps running.
struct Parked {
    id: u64,
    thread: Thread,
    target: Target,
    deadline: Option<Instant>,
    timeout: Duration,
}

/// What the thread does next, outside the lock.
enum Work {
    Start(Job, Duration, Option<Instant>),
    Resume(Parked, HttpResult),
}

/// Runs the entry script, under the deadline `load_by`, then serves calls
/// until the extension is stopped.
pub(super) fn serve(name: &str, dir: &Path, home: &Path, hub: &Arc<Hub>, load_by: Option<Instant>) {
    let loaded = Vm::load(name, dir, home, load_by);
    let vm = {
        let mut shared = hub.lock();
        if !matches!(shared.phase, Phase::Registering { .. }) {
            return;
        }
        let vm = match loaded {
            Ok(vm) => {
                shared.phase = Phase::Ready(vm.declared());
                Some(vm)
            }
            Err(e) => {
                shared.phase = Phase::Stopped(e);
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
            Work::Resume(p, result) => {
                let step = host::resume_values(&vm.lua, result)
                    .map_err(|e| vm.error(&e))
                    .and_then(|args| vm.after(p.thread, args, &p.target, p.timeout, p.deadline));
                (p.id, step)
            }
        };
        settle(name, dir, hub, &mut parked, id, step);
    }
}

/// Waits for the next thing to do: a parked callback past its deadline is
/// failed here, a `host.http` reply resumes its callback, and a queued call
/// starts if it may. None once the extension is stopped.
fn next(name: &str, hub: &Hub, parked: &mut Vec<Parked>) -> Option<Work> {
    let mut shared = hub.lock();
    loop {
        if !matches!(shared.phase, Phase::Ready(_)) {
            return None;
        }
        if let Some(pos) = parked.iter().position(|p| expired(p.deadline)) {
            let p = parked.swap_remove(pos);
            shared.finish(p.id, Err(timed_out(name, &p.target, p.timeout)));
            hub.notify();
            continue;
        }
        if let Some((id, result)) = shared.replies.pop() {
            let Some(pos) = parked.iter().position(|p| p.id == id) else {
                continue;
            };
            let p = parked.swap_remove(pos);
            // A caller that has gone gave up on its own deadline.
            if let Some(progress) = shared.calls.get_mut(&id) {
                *progress = Progress::Started {
                    deadline: p.deadline,
                    parked: false,
                };
                return Some(Work::Resume(p, result));
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
        let wake = parked.iter().filter_map(|p| p.deadline).min();
        shared = hub.wait(shared, wake);
    }
}

/// Records a finished callback's result, or parks it on `host.http`.
fn settle(
    name: &str,
    dir: &Path,
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
    let http = Arc::clone(hub);
    let spawned = thread::Builder::new()
        .name(format!("http {name}"))
        .spawn(move || {
            let result = host::perform(&request);
            http.lock().replies.push((id, result));
            http.notify();
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
    parked.push(Parked {
        id,
        thread,
        target,
        deadline,
        timeout,
    });
    if let Some(progress) = hub.lock().calls.get_mut(&id) {
        *progress = Progress::Started {
            deadline,
            parked: true,
        };
    }
    hub.notify();
}
