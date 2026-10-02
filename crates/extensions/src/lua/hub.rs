//! The one state every waiter observes (`docs/extensions.md`, "How an
//! extension runs" and "When an extension misbehaves"): the extension's
//! phase, the calls not yet started, each call's progress and the replies of
//! `host.http`, behind one lock. Every caller and the extension's thread wait
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
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::Error;

use super::{ENTRY, GRACE, Target, expired, timeout_ms};

/// A `host.http` reply: the status and body, or why it failed.
pub(super) type HttpResult = Result<(u16, Vec<u8>), String>;

#[derive(Default)]
pub(super) struct Hub {
    shared: Mutex<Shared>,
    changed: Condvar,
}

impl Hub {
    pub(super) fn lock(&self) -> MutexGuard<'_, Shared> {
        // A panic aborts the process (`docs/code-quality.md`, "Panics"), so
        // no holder can leave the lock poisoned.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wakes every waiter to look at the state again.
    pub(super) fn notify(&self) {
        self.changed.notify_all();
    }

    /// Sleeps until a change or `until`, whichever is first.
    pub(super) fn wait<'a>(
        &self,
        shared: MutexGuard<'a, Shared>,
        until: Option<Instant>,
    ) -> MutexGuard<'a, Shared> {
        let Some(until) = until else {
            return self
                .changed
                .wait(shared)
                .unwrap_or_else(PoisonError::into_inner);
        };
        self.changed
            .wait_timeout(shared, until.saturating_duration_since(Instant::now()))
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }

    /// Records `result` for the call `id`, if its caller still waits.
    pub(super) fn finish(&self, id: u64, result: Result<Value, Error>) {
        self.lock().finish(id, result);
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
    /// Replies of `host.http` calls for parked callbacks, by call id.
    pub(super) replies: Vec<(u64, HttpResult)>,
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
    /// Started, with the deadline it declared. `parked` while suspended on
    /// `host.http`, when the thread is free to fail it at its deadline.
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

/// Commands and provider functions, in separate maps so a shared name cannot collide.
#[derive(Default)]
pub(super) struct CallbackTimeouts {
    pub(super) commands: BTreeMap<String, Duration>,
    pub(super) providers: BTreeMap<String, BTreeMap<String, Duration>>,
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
        }
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
    pub(super) fn gate(&mut self, name: &str) -> Gate<'_> {
        if let Phase::Registering { abandon_at } = self.phase
            && expired(abandon_at, &Instant::now)
        {
            // ponytail: the abandoned thread is leaked, still running, until
            // the process exits; Rust cannot stop a thread.
            self.phase = Phase::Stopped(Error::Abandoned {
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
    pub(super) fn judge(&mut self, name: &str, id: u64, target: &Target, asked: Instant) -> Next {
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
        let declared = match self.gate(name) {
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
        if !expired(until, &Instant::now) {
            return Next::Sleep(until);
        }
        self.forget(id);
        if !running {
            // Queued or parked, not on the thread: failing it leaves the VM up.
            return Next::Return(Err(timed_out(name, target, timeout)));
        }
        // ponytail: the abandoned thread is leaked, still running, until
        // the process exits; Rust cannot stop a thread.
        self.phase = Phase::Stopped(stopped(name));
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

    /// Drops the call `id`: its caller has stopped waiting.
    fn forget(&mut self, id: u64) {
        self.calls.remove(&id);
        self.queue.retain(|job| job.id != id);
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
        Target::Provider { .. } => Error::UnknownCallback {
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
        | Error::ProviderMissing { .. }
        | Error::ModelMissing { .. }
        | Error::UnknownModel { .. }
        | Error::Ambiguous { .. }
        | Error::UnknownCommand { .. }
        | Error::UnknownCallback { .. }
        | Error::BadReturn { .. }
        | Error::NoModel => stopped(name),
    }
}
