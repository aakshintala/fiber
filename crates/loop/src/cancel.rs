//! The signal that cancels a running turn (`docs/architecture.md`,
//! "Cancellation"): shared across threads, armed while a turn runs.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use contract::clock::Wake;
use contract::events::{CallStatus, ToolCallCompleted, TurnCompleted, TurnOutcome};
use contract::provider::{CallError, Delta, ModelCall, Reply};
use contract::tool::Cancel as _;

/// The cancel signal for one session's running turn. The loop arms it
/// before it writes `turn_started` and disarms it just before it writes
/// `turn_completed`; a driver cancels through [`TurnCancel::cancel`] from
/// any thread. Clones share one signal.
#[derive(Debug, Default)]
pub struct TurnCancel {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// A turn is running.
    armed: bool,
    /// A cancel landed since `arm`.
    cancelled: bool,
    /// The exit code of the signal that started a shutdown
    /// (`docs/invocation.md`, "Shutdown"). Set once; never cleared.
    shutdown: Option<i32>,
    /// Woken once on the cancel, outside the lock.
    subscribers: Vec<Weak<dyn Wake>>,
}

impl TurnCancel {
    /// Cancels the running turn from any thread. True when a turn was
    /// running, even one already cancelled; false when none is.
    pub fn cancel(&self) -> bool {
        let wakers = {
            let mut inner = lock(&self.inner);
            if !inner.armed {
                return false;
            }
            inner.cancelled = true;
            std::mem::take(&mut inner.subscribers)
        };
        for waker in wakers.into_iter().filter_map(|waker| waker.upgrade()) {
            waker.wake();
        }
        true
    }

    /// Starts a shutdown with exit code `code` (`docs/invocation.md`,
    /// "Shutdown"): the running turn is cancelled as [`Self::cancel`]
    /// cancels it, and no later turn arms. The first code stays.
    pub fn shutdown(&self, code: i32) {
        let wakers = {
            let mut inner = lock(&self.inner);
            inner.shutdown.get_or_insert(code);
            if inner.armed {
                inner.cancelled = true;
            }
            std::mem::take(&mut inner.subscribers)
        };
        for waker in wakers.into_iter().filter_map(|waker| waker.upgrade()) {
            waker.wake();
        }
    }

    /// The exit code of the shutdown, once one started.
    pub fn shutdown_code(&self) -> Option<i32> {
        lock(&self.inner).shutdown
    }

    /// The signal's state, read once under its lock. A shutdown wins over
    /// a cancel: it sets `cancelled` too.
    pub(crate) fn state(&self) -> SignalState {
        let inner = lock(&self.inner);
        if inner.shutdown.is_some() {
            SignalState::Shutdown
        } else if inner.cancelled {
            SignalState::Cancelled
        } else {
            SignalState::Live
        }
    }

    /// Arms the signal for a turn that is about to start: running, not
    /// cancelled, subscribers cleared. False, arming nothing, once a
    /// shutdown started.
    #[cfg(test)]
    pub(crate) fn arm(&self) -> bool {
        self.commit(Commit::Arm, || ()).is_some()
    }

    /// Runs `f` under the signal's lock when `mode` allows it, so a
    /// shutdown lands either before the write `f` makes (`None`, nothing
    /// written) or after it. `f` must call no method of this signal: the
    /// lock is not reentrant.
    pub(crate) fn commit<R>(&self, mode: Commit, f: impl FnOnce() -> R) -> Option<R> {
        let mut inner = lock(&self.inner);
        match mode {
            Commit::Arm => {
                if inner.shutdown.is_some() {
                    return None;
                }
                inner.armed = true;
                inner.cancelled = false;
                inner.subscribers.clear();
            }
            Commit::Step => {
                if inner.cancelled || inner.shutdown.is_some() {
                    return None;
                }
            }
        }
        Some(f())
    }

    /// Disarms the signal in one locked step, just before `turn_completed`
    /// is written. Returns true when a cancel landed since `arm`.
    pub(crate) fn disarm(&self) -> bool {
        let mut inner = lock(&self.inner);
        inner.armed = false;
        let cancelled = inner.cancelled;
        inner.cancelled = false;
        inner.subscribers.clear();
        cancelled
    }
}

/// What one read of the signal found ([`TurnCancel::state`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalState {
    /// No cancel and no shutdown since `arm`.
    Live,
    /// A cancel landed since `arm`; no shutdown started.
    Cancelled,
    /// A shutdown started.
    Shutdown,
}

/// What [`TurnCancel::commit`] checks before it runs its write.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Commit {
    /// Arms the turn; refused once a shutdown started.
    Arm,
    /// A step's start; refused once the turn is cancelled.
    Step,
}

impl contract::tool::Cancel for TurnCancel {
    /// True once a cancel landed since `arm`, and for the rest of the
    /// process once a shutdown started.
    fn is_cancelled(&self) -> bool {
        let inner = lock(&self.inner);
        inner.cancelled || inner.shutdown.is_some()
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        lock(&self.inner).subscribers.push(waker);
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

impl crate::Loop {
    /// Disarms the signal in one locked step, immediately before
    /// `turn_completed` is written. A cancel that landed turns an outcome
    /// of `completed` into `interrupted`; `failed` stays `failed`. A
    /// cancel after the disarm finds the signal disarmed and is
    /// `stale_request`.
    pub(crate) fn disarm_cancel(&self, completed: &mut TurnCompleted) {
        if self.cancel.disarm() {
            interrupted(completed);
        }
    }

    /// Whether a cancel landed since the turn was armed.
    pub(crate) fn turn_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Whether a shutdown started (`docs/invocation.md`, "Shutdown").
    pub(crate) fn shutting_down(&self) -> bool {
        self.cancel.shutdown_code().is_some()
    }
}

/// A cancel landed before the disarm: `completed` becomes `interrupted` and
/// carries no questions, which only a turn that ended on them carries;
/// `failed` and `interrupted` are unchanged.
pub(crate) fn interrupted(completed: &mut TurnCompleted) {
    if completed.outcome == TurnOutcome::Completed {
        completed.outcome = TurnOutcome::Interrupted;
        completed.questions = None;
    }
}

/// The completion of a call the cancel reached before it ran: `cancelled`,
/// telling the model it never started. A `permission_resolved` line already
/// written stays as decided.
pub(crate) fn never_ran() -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Cancelled,
        ..crate::completion::completed("Cancelled before it ran.".to_owned(), None)
    })
}

impl crate::Loop {
    /// The completion of a call the cancel reached before it ran. Under a
    /// shutdown no `after_tool` hook runs to redact output, so the
    /// completion carries no content and no artifact
    /// (`docs/invocation.md`, "Shutdown"); an ordinary cancel tells the
    /// model it never started.
    pub(crate) fn cancelled_before_ran(&self) -> Box<ToolCallCompleted> {
        if self.shutting_down() {
            Box::new(ToolCallCompleted {
                status: CallStatus::Cancelled,
                ..crate::completion::completed(String::new(), None)
            })
        } else {
            never_ran()
        }
    }
}

/// Wakes `call`'s `cancel` when the turn is cancelled.
struct CallWaker {
    call: Arc<dyn ModelCall>,
}

impl Wake for CallWaker {
    fn wake(&self) {
        self.call.cancel();
    }
}

/// Runs `call` cancellably: subscribes a waker that ends the model call on
/// cancel, then checks the signal after subscribing, so a cancel that
/// landed before the subscription still ends a call that has not yet run.
/// A waker may run after `disarm` or the next `arm`; it then holds a
/// `ModelCall` whose `run` has returned, so the wake upgrades to a call
/// that has ended.
#[allow(
    clippy::result_large_err,
    reason = "the error is the model call's, returned unchanged"
)]
pub(crate) fn run_cancellable(
    cancel: &TurnCancel,
    call: Box<dyn ModelCall>,
    sink: &mut dyn FnMut(Delta),
) -> Result<Reply, CallError> {
    let call: Arc<dyn ModelCall> = call.into();
    let waker: Arc<dyn Wake> = Arc::new(CallWaker {
        call: Arc::clone(&call),
    });
    cancel.subscribe(Arc::downgrade(&waker));
    if cancel.is_cancelled() {
        call.cancel();
    }
    // Held while `run` blocks: the subscription keeps only a `Weak`, so
    // the waker must stay alive for the wake to reach the call.
    let _keep = waker;
    call.run(sink)
}

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod tests;
