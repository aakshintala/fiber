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

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use mlua::Table;
use serde_json::Value;

use crate::Error;
use crate::host::Reply;

use super::{ENTRY, GRACE, Target, expired, timeout_ms};

pub(super) struct Hub {
    shared: Mutex<Shared>,
    changed: Condvar,
    clock: Arc<dyn Clock>,
}

impl Hub {
    /// The hub every waiter and the extension's thread share, subscribed to
    /// `clock` so a move of that clock wakes them.
    pub(super) fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        let hub = Arc::new(Self {
            shared: Mutex::new(Shared::default()),
            changed: Condvar::new(),
            clock: Arc::clone(&clock),
        });
        let cloned = Arc::clone(&hub);
        let wake: Arc<dyn Wake> = cloned;
        clock.subscribe(Arc::downgrade(&wake));
        hub
    }

    pub(super) fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    pub(super) fn clock_handle(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Shared> {
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
}

#[derive(Default)]
pub(super) struct Shared {
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
    next_id: u64,
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

/// What the entry script registered: commands, provider functions and hooks,
/// in separate maps so a shared name cannot collide.
#[derive(Default)]
pub(super) struct CallbackTimeouts {
    pub(super) commands: BTreeMap<String, Duration>,
    pub(super) providers: BTreeMap<String, BTreeMap<String, Duration>>,
    pub(super) hooks: DeclaredHooks,
}

impl CallbackTimeouts {
    /// The timeout `target` declared, if the entry script registered it.
    pub(super) fn timeout(&self, target: &Target) -> Option<Duration> {
        match target {
            Target::Command(name) => self.commands.get(name).copied(),
            Target::Provider { name, function } => self
                .providers
                .get(name)
                .and_then(|fns| fns.get(*function))
                .copied(),
            Target::Hook { point, index } => self
                .hooks
                .by_point
                .get(point)
                .and_then(|hooks| hooks.get(*index))
                .map(|hook| hook.timeout),
        }
    }

    /// Every hook runs under `timeout` instead of the one it declared.
    pub(super) fn override_hooks(&mut self, timeout: Option<Duration>) {
        let Some(timeout) = timeout else {
            return;
        };
        for hook in self.hooks.by_point.values_mut().flatten() {
            hook.timeout = timeout;
        }
    }
}

/// A hook's phase (`docs/extensions.md`, "When several hooks share a
/// point"), in the order the phases run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HookPhase {
    Sanitize,
    Transform,
    Check,
}

/// One hook the entry script registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredHook {
    pub(crate) phase: HookPhase,
    /// `on_failure` is `blocking`.
    pub(crate) blocking: bool,
    /// Its timeout, or the configured override.
    pub(crate) timeout: Duration,
}

/// The hooks the entry script registered, by point in registration order,
/// and a message for each it tried to register and could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeclaredHooks {
    pub(crate) by_point: BTreeMap<String, Vec<DeclaredHook>>,
    pub(crate) problems: Vec<String>,
}

impl DeclaredHooks {
    /// The hooks in `hooks`, the table `fiber.hook` fills, and the
    /// refusals in `problems`. The prelude checked each field.
    pub(super) fn read(hooks: &Table, problems: &Table) -> Self {
        let mut declared = Self::default();
        for (point, list) in hooks.pairs::<String, Table>().flatten() {
            let list = list
                .sequence_values::<Table>()
                .flatten()
                .map(|spec| DeclaredHook {
                    phase: match spec.get::<String>("phase").as_deref() {
                        Ok("sanitize") => HookPhase::Sanitize,
                        Ok("check") => HookPhase::Check,
                        _ => HookPhase::Transform,
                    },
                    blocking: spec
                        .get::<String>("on_failure")
                        .is_ok_and(|failure| failure == "blocking"),
                    timeout: Duration::from_millis(spec.get::<u64>("timeout").unwrap_or(0)),
                })
                .collect();
            declared.by_point.insert(point, list);
        }
        declared.problems = problems.sequence_values::<String>().flatten().collect();
        declared
    }
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
        Target::Provider { .. } | Target::Hook { .. } => Error::UnknownCallback {
            extension: name.to_owned(),
            callback: target.to_string(),
        },
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
        | Error::BadRepositoryPath { .. }
        | Error::Pin { .. }
        | Error::ChangedWhileCopying { .. }
        | Error::NoModel => stopped(name),
    }
}
