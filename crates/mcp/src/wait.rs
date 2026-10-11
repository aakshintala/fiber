//! The request/response rendezvous for MCP servers: shared slots, injected-clock
//! waits, cancellation and gone notifications (`docs/mcp.md`, "Calls").

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

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
    /// Whether the remote server ended its session under this wait.
    pub(crate) expired: bool,
    pub(crate) cancelled: bool,
    pub(crate) seq: u64,
}

#[derive(Default)]
pub(crate) struct SharedState {
    pub(crate) seq: u64,
    next_id: u64,
    pending: BTreeMap<u64, Option<Outcome>>,
    pub(crate) gone: bool,
    /// Whether the remote server ended its session: set with `gone` by
    /// [`Shared::expire_if_awaiting`], never alone.
    pub(crate) expired: bool,
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
            expired: state.expired,
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

    /// Whether `id` still waits for its answer (its slot exists and holds none).
    pub(crate) fn awaiting(&self, id: u64) -> bool {
        lock(&self.inner)
            .pending
            .get(&id)
            .is_some_and(|slot| slot.is_none())
    }

    /// Marks the server gone only when `id` still waits, in one step under
    /// the lock; returns whether it did. A response that already delivered
    /// its answer, or a wait that already ended and removed its slot,
    /// never marks the server gone (`docs/mcp.md`, "When a server dies").
    pub(crate) fn gone_if_awaiting(&self, id: u64) -> bool {
        let mut state = lock(&self.inner);
        let awaiting = state.pending.get(&id).is_some_and(|slot| slot.is_none());
        if awaiting {
            state.gone = true;
            state.seq = state.seq.wrapping_add(1);
        }
        drop(state);
        if awaiting {
            self.cv.notify_all();
        }
        awaiting
    }

    /// Marks the session expired (and so gone) only when `id` still waits
    /// and the server is not already gone, in one step under the lock;
    /// returns whether it did. A `404` read after its call ended never
    /// expires the server: precedence is the waiter's, exactly as for a
    /// death (`docs/mcp.md`, "When a server dies").
    pub(crate) fn expire_if_awaiting(&self, id: u64) -> bool {
        let mut state = lock(&self.inner);
        let awaiting = state.pending.get(&id).is_some_and(|slot| slot.is_none());
        let live = !state.gone;
        if awaiting && live {
            state.gone = true;
            state.expired = true;
            state.seq = state.seq.wrapping_add(1);
        }
        drop(state);
        if awaiting && live {
            self.cv.notify_all();
        }
        awaiting && live
    }
}

/// The deadline `timeout` after now on `clock`, or now when the sum
/// overflows. It reads the clock once.
pub(crate) fn deadline(clock: &dyn Clock, timeout: Duration) -> Instant {
    let now = clock.now();
    now.checked_add(timeout).unwrap_or(now)
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
    // The guard is held from the check into the condvar wait, so a wake
    // under the same lock is never missed.
    let _guard = support::clock::park(
        clock,
        Some(until),
        None,
        &shared.cv,
        lock(&shared.inner),
        |guard| should_stop(guard.seq, seen, cancel.is_cancelled()),
    );
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
