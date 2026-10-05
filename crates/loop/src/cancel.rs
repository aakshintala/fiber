//! The signal that cancels a running turn (`docs/architecture.md`,
//! "Cancellation"): shared across threads, armed while a turn runs.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use contract::clock::Wake;
use contract::events::{
    CallStatus, DecidedBy, Decision, PermissionResolved, ToolCallCompleted, TurnOutcome,
};
use contract::provider::{CallError, Delta, ModelCall, Reply};
use contract::shapes::{ContentPart, Failure};
use contract::tool::Cancel as _;
use contract::{ErrorCode, RequestId};

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

    /// Arms the signal for a turn that is about to start: running, not
    /// cancelled, subscribers cleared.
    pub(crate) fn arm(&self) {
        let mut inner = lock(&self.inner);
        inner.armed = true;
        inner.cancelled = false;
        inner.subscribers.clear();
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

impl contract::tool::Cancel for TurnCancel {
    fn is_cancelled(&self) -> bool {
        lock(&self.inner).cancelled
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        lock(&self.inner).subscribers.push(waker);
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

impl crate::Loop {
    /// Arms the cancel signal for the turn about to start: running, not
    /// cancelled, subscribers cleared. Called before `turn_started` is
    /// written.
    pub(crate) fn arm_cancel(&self) {
        self.cancel.arm();
    }

    /// Disarms the signal in one locked step, immediately before
    /// `turn_completed` is written. A cancel that landed turns an outcome
    /// of `completed` into `interrupted`; `failed` stays `failed`. A
    /// cancel after the disarm finds the signal disarmed and is
    /// `stale_request`.
    pub(crate) fn disarm_cancel(&self, completed: &mut contract::events::TurnCompleted) {
        if self.cancel.disarm() && completed.outcome == TurnOutcome::Completed {
            completed.outcome = TurnOutcome::Interrupted;
        }
    }

    /// Whether a cancel landed since the turn was armed.
    pub(crate) fn turn_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// The completion of a call the cancel reached before it ran: `cancelled`,
/// telling the model it never started. A `permission_resolved` line already
/// written stays as decided.
pub(crate) fn never_ran() -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Cancelled,
        ..completed("Cancelled before it ran.".to_owned(), None)
    })
}

/// How a call the reply cut off completes (`docs/loop.md`, "A reply cut off
/// by the output limit").
pub(crate) fn truncated() -> ToolCallCompleted {
    failed(
        ErrorCode::OutputTruncated,
        "Your reply reached its output limit, so this call did not run and its \
         arguments may be incomplete. Make the call again, split into smaller \
         calls if it was large."
            .to_owned(),
    )
}

/// A `permission_resolved` line carrying `request_id`, `decision`,
/// `decided_by`, `reason` and `feedback`: every other key is absent. A call
/// whose answer remembered something sets `grant` or `rule` on it.
pub(crate) fn resolved(
    request_id: Option<RequestId>,
    decision: Decision,
    decided_by: DecidedBy,
    reason: Option<String>,
    feedback: Option<String>,
) -> PermissionResolved {
    PermissionResolved {
        request_id,
        decision,
        decided_by,
        reason,
        feedback,
        grant: None,
        rule: None,
        reviewer: None,
    }
}

/// A call that was denied with `reason`, the model told `text`.
pub(crate) fn denied(reason: &str, text: String) -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Denied,
        reason: Some(reason.to_owned()),
        ..completed(text, None)
    })
}

/// A call that failed with `code` before it ran, the model told `message`.
pub(crate) fn failed(code: ErrorCode, message: String) -> ToolCallCompleted {
    ToolCallCompleted {
        status: CallStatus::Failed,
        ..completed(
            message.clone(),
            Some(Failure {
                code,
                message,
                retry_after: None,
                provider: None,
            }),
        )
    }
}

pub(crate) fn completed(text: String, error: Option<Failure>) -> ToolCallCompleted {
    ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error,
        process: None,
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![ContentPart::Text { text }]
        },
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
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
