//! One running call's interaction (`docs/events.md`, "Interactions"): the
//! call's thread raises an ask and blocks, and the loop thread writes its
//! lines, fits a `reply` to it and releases the call with the answer. Only
//! the loop thread writes, so the slot carries the ask across and the
//! answer back (`docs/architecture.md`, "Streaming").

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use contract::RequestId;
use contract::clock::Wake;
use contract::commands::ReplyAnswer;
use contract::events::{Answer, Interaction};
use contract::tool::{Answered, Asking, Check};

use crate::mint;
use crate::progress::Stream;

/// Where one call's ask stands.
enum Slot {
    /// Nothing asked.
    Idle,
    /// Asked; the loop thread has not taken it yet.
    Raised(Asking),
    /// Taken by the loop thread, which writes its line or answers it next.
    Taken,
    /// Its `interaction_requested` is written; a `reply` may answer it.
    Pending {
        request: RequestId,
        interaction: Interaction,
        until: Option<Instant>,
        check: Option<Check>,
        /// Raised with [`Asking::suspends`].
        suspends: bool,
    },
    /// Answered; the asking thread has not taken the answer yet.
    Resolved(Answered),
}

/// The slot and whether it is closed for good: a closed slot never blocks
/// an ask again.
struct Inner {
    slot: Slot,
    closed: bool,
    /// The id the next raise is written under, bound by
    /// [`AskSlot::reraise`].
    reraise: Option<RequestId>,
}

/// One running call's ask, shared by its thread and the loop thread.
pub(crate) struct AskSlot {
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl std::fmt::Debug for AskSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = lock(&self.inner);
        let state = match &inner.slot {
            Slot::Idle => "idle",
            Slot::Raised(_) => "raised",
            Slot::Taken => "taken",
            Slot::Pending { .. } => "pending",
            Slot::Resolved(_) => "resolved",
        };
        f.debug_struct("AskSlot")
            .field("state", &state)
            .field("closed", &inner.closed)
            .finish()
    }
}

/// What a `reply` naming a request makes of one slot.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fitted {
    /// The slot holds no pending request of that id.
    NotThis,
    /// It names this slot's request, but its keys do not fit the kind or
    /// the asker's check refused it.
    Unfit,
    /// It answers this slot's request.
    Fits(Answer),
}

impl Default for AskSlot {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                slot: Slot::Idle,
                closed: false,
                reraise: None,
            }),
            cv: Condvar::new(),
        }
    }
}

impl AskSlot {
    /// Raises `asking` from the call's thread, wakes the loop thread through
    /// `wake`, and blocks until the loop resolves it. A closed slot returns
    /// [`Answered::NoAnswer`] at once and wakes nobody. A second ask from
    /// the same call waits for the first to end.
    pub(crate) fn ask(&self, asking: Asking, wake: &dyn Wake) -> Answered {
        let mut inner = lock(&self.inner);
        loop {
            if inner.closed {
                return Answered::NoAnswer;
            }
            if matches!(inner.slot, Slot::Idle) {
                break;
            }
            inner = self.wait(inner);
        }
        inner.slot = Slot::Raised(asking);
        drop(inner);
        wake.wake();
        let mut inner = lock(&self.inner);
        loop {
            match std::mem::replace(&mut inner.slot, Slot::Idle) {
                Slot::Resolved(answered) => {
                    self.cv.notify_all();
                    return answered;
                }
                held @ (Slot::Idle | Slot::Raised(_) | Slot::Taken | Slot::Pending { .. }) => {
                    inner.slot = held;
                }
            }
            if inner.closed {
                return Answered::NoAnswer;
            }
            inner = self.wait(inner);
        }
    }

    /// The ask raised since the last pass, taking it.
    pub(crate) fn take_raised(&self) -> Option<Asking> {
        let mut inner = lock(&self.inner);
        match std::mem::replace(&mut inner.slot, Slot::Taken) {
            Slot::Raised(asking) => Some(asking),
            held @ (Slot::Idle | Slot::Taken | Slot::Pending { .. } | Slot::Resolved(_)) => {
                inner.slot = held;
                None
            }
        }
    }

    /// The next raise is written under `request` instead of a new id: a
    /// request raised again on resume keeps its `request_id`
    /// (`docs/invocation.md`, "Lifecycle").
    pub(crate) fn reraise(&self, request: RequestId) {
        lock(&self.inner).reraise = Some(request);
    }

    /// The id a raise is written under: the one [`AskSlot::reraise`]
    /// bound, once, and a new one otherwise.
    pub(crate) fn next_request(&self) -> RequestId {
        lock(&self.inner)
            .reraise
            .take()
            .unwrap_or_else(|| RequestId(mint("r_")))
    }

    /// Records the taken ask as pending under `request`, once its line is
    /// written.
    pub(crate) fn pend(
        &self,
        request: RequestId,
        interaction: Interaction,
        until: Option<Instant>,
        check: Option<Check>,
        suspends: bool,
    ) {
        lock(&self.inner).slot = Slot::Pending {
            request,
            interaction,
            until,
            check,
            suspends,
        };
    }

    /// Whether the pending interaction was raised with
    /// [`Asking::suspends`]; false when none is pending.
    pub(crate) fn pending_suspends(&self) -> bool {
        matches!(lock(&self.inner).slot, Slot::Pending { suspends: true, .. })
    }

    /// The pending request and its `until`, if one is pending.
    pub(crate) fn pending(&self) -> Option<(RequestId, Option<Instant>)> {
        match &lock(&self.inner).slot {
            Slot::Pending { request, until, .. } => Some((request.clone(), *until)),
            Slot::Idle | Slot::Raised(_) | Slot::Taken | Slot::Resolved(_) => None,
        }
    }

    /// Fits `answer` if this slot's pending request is `request`: the kind's
    /// fit first, then the asker's check, which a decline never reaches
    /// (`docs/invocation.md`, "Replying").
    pub(crate) fn fit(&self, request: &RequestId, answer: &ReplyAnswer) -> Fitted {
        let inner = lock(&self.inner);
        let Slot::Pending {
            request: pending,
            interaction,
            check,
            ..
        } = &inner.slot
        else {
            return Fitted::NotThis;
        };
        if pending != request {
            return Fitted::NotThis;
        }
        match interaction.fit(answer) {
            None => Fitted::Unfit,
            Some(declined @ Answer::Declined { .. }) => Fitted::Fits(declined),
            Some(fitted) => match check {
                Some(check) if !check(&fitted) => Fitted::Unfit,
                Some(_) | None => Fitted::Fits(fitted),
            },
        }
    }

    /// Releases the asking thread with `answered`.
    pub(crate) fn resolve(&self, answered: Answered) {
        lock(&self.inner).slot = Slot::Resolved(answered);
        self.cv.notify_all();
    }

    /// Ends the slot for good: an ask raised or pending is released with
    /// [`Answered::NoAnswer`], and every later ask gets it at once. An
    /// answer already resolved is still taken by its asker.
    pub(crate) fn close(&self) {
        let mut inner = lock(&self.inner);
        inner.closed = true;
        if !matches!(inner.slot, Slot::Resolved(_)) {
            inner.slot = Slot::Idle;
        }
        self.cv.notify_all();
    }

    fn wait<'a>(&self, inner: MutexGuard<'a, Inner>) -> MutexGuard<'a, Inner> {
        self.cv.wait(inner).unwrap_or_else(PoisonError::into_inner)
    }
}

/// Closes every stream's slot when dropped: `run_batch` returns, `Ok` or
/// `Err`, and no call's thread is left blocked in an ask while the scope
/// joins it. Each stream is added before its worker is spawned.
#[derive(Default)]
pub(crate) struct Release(Vec<Arc<Stream>>);

impl Release {
    /// Holds `stream` until the drop.
    pub(crate) fn add(&mut self, stream: Arc<Stream>) {
        self.0.push(stream);
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        for stream in &self.0 {
            stream.asking().close();
        }
    }
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "asking_tests.rs"]
mod tests;
