//! The request/response rendezvous for MCP servers: shared slots, injected-clock
//! waits, cancellation and gone notifications (`docs/mcp.md`, "Calls").

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Instant;

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;

use crate::registry::lock;
use crate::rpc::Outcome;

/// What a wait loop sees, read under the shared lock after subscribing, so
/// a response that lands between the subscribe and the read is still
/// visible.
pub(crate) struct View {
    pub(crate) response: Option<Outcome>,
    pub(crate) gone: bool,
    pub(crate) cancelled: bool,
    pub(crate) seq: u64,
}

#[derive(Default)]
pub(crate) struct SharedState {
    pub(crate) seq: u64,
    next_id: u64,
    pending: BTreeMap<u64, Option<Outcome>>,
    pub(crate) gone: bool,
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) inner: Mutex<SharedState>,
    pub(crate) cv: Condvar,
}

impl Wake for Shared {
    fn wake(&self) {
        // The sequence moves under the same lock as the wait, so a clock
        // advance that lands before the condvar wait is still visible when
        // the waiter checks.
        let mut guard = lock(&self.inner);
        guard.seq = guard.seq.wrapping_add(1);
        drop(guard);
        self.cv.notify_all();
    }
}

impl Shared {
    /// The next request id. Ids start at 1 and are never reused: a `u64`
    /// counter a session cannot exhaust.
    pub(crate) fn next_id(&self) -> u64 {
        let mut state = lock(&self.inner);
        state.next_id += 1;
        state.next_id
    }

    pub(crate) fn insert(&self, id: u64) {
        lock(&self.inner).pending.insert(id, None);
    }

    pub(crate) fn remove(&self, id: u64) {
        lock(&self.inner).pending.remove(&id);
    }

    pub(crate) fn view(&self, id: u64, cancel: &dyn Cancel) -> View {
        let state = lock(&self.inner);
        View {
            response: state.pending.get(&id).and_then(|slot| slot.clone()),
            gone: state.gone,
            cancelled: cancel.is_cancelled(),
            seq: state.seq,
        }
    }

    /// Delivers `response` to its id's slot, or discards it when the slot
    /// is gone: a late response to a timed-out id never misroutes.
    pub(crate) fn deliver(&self, id: u64, outcome: Outcome) {
        let mut state = lock(&self.inner);
        if let Some(slot) = state.pending.get_mut(&id) {
            *slot = Some(outcome);
            state.seq = state.seq.wrapping_add(1);
            drop(state);
            self.cv.notify_all();
        }
    }

    /// Marks the server gone and wakes every waiter.
    pub(crate) fn gone(&self) {
        let mut state = lock(&self.inner);
        state.gone = true;
        state.seq = state.seq.wrapping_add(1);
        drop(state);
        self.cv.notify_all();
    }
}

/// True when the waiter stops waiting: a response landed (`seq` moved) or
/// the call was cancelled. A pure function of its inputs so a test pins all
/// four combinations without parking a thread.
fn should_stop(seq: u64, seen: u64, cancelled: bool) -> bool {
    seq != seen || cancelled
}

/// Blocks until woken or `until` passes on the clock, releasing every lock
/// first: the response lands under the shared lock, so joining ahead of
/// that release would deadlock.
pub(crate) fn park(
    clock: &dyn Clock,
    shared: &Shared,
    cancel: &dyn Cancel,
    until: Instant,
    seen: u64,
) {
    // Taken before `wait_until`, and held until the condvar wait, so a wake
    // blocks on this lock instead of notifying nobody.
    let mut slot = Some(lock(&shared.inner));
    clock.wait_until(Some(until), &mut |bound| {
        let Some(guard) = slot.take() else {
            return;
        };
        if should_stop(guard.seq, seen, cancel.is_cancelled()) {
            slot = Some(guard);
            return;
        }
        slot = Some(match bound {
            Some(bound) => {
                shared
                    .cv
                    .wait_timeout(guard, bound)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => shared
                .cv
                .wait(guard)
                .unwrap_or_else(PoisonError::into_inner),
        });
    });
}

/// The call's cancel reaches the wait through this bridge. It holds the
/// shared state weakly and is dropped when the wait ends, so a fired cancel
/// wakes only the waits still running.
pub(crate) struct CancelBridge(Weak<Shared>);

impl CancelBridge {
    pub(crate) fn arm(shared: &Arc<Shared>) -> Arc<Self> {
        Arc::new(Self(Arc::downgrade(shared)))
    }
}

impl Wake for CancelBridge {
    fn wake(&self) {
        if let Some(shared) = self.0.upgrade() {
            shared.wake();
        }
    }
}

/// A cancel that never fires, for `initialize` and `tools/list`: the
/// startup deadline, not a person, ends those waits.
pub(crate) struct NoCancel;

impl Cancel for NoCancel {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

#[cfg(test)]
#[path = "wait_tests.rs"]
mod tests;
