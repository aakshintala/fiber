//! Stopping every MCP server still starting (`docs/mcp.md`, "Starting
//! servers"). Live servers are listed in `support::group`'s process-wide
//! list instead, signalled as groups;
//! [`stop_every_start`] is what stays here.

use std::sync::{Mutex, MutexGuard, PoisonError, Weak};

use contract::clock::Wake;
use contract::tool::Cancel;

/// Stops every server still starting, and every start after it: the flag is
/// sticky for the life of the process. It sets the flag and wakes the parked
/// handshakes; the stop and reap run on each start's own thread. Idempotent.
pub fn stop_every_start() {
    *lock(&STOPPED) = true;
    // Dropped before any wake: a wake takes the shared lock.
    let waiting = std::mem::take(&mut *lock(&WAITING));
    for waker in waiting {
        if let Some(waker) = waker.upgrade() {
            waker.wake();
        }
    }
}

static STOPPED: Mutex<bool> = Mutex::new(false);
static WAITING: Mutex<Vec<Weak<dyn Wake>>> = Mutex::new(Vec::new());

/// The handshake's cancel, fired by [`stop_every_start`].
pub(crate) struct Stopping;

impl Cancel for Stopping {
    fn is_cancelled(&self) -> bool {
        *lock(&STOPPED)
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        let mut waiting = lock(&WAITING);
        // A finished start drops its bridge and leaves a dead entry: pruned
        // here, so the list holds only the starts still running.
        waiting.retain(|listed| listed.upgrade().is_some());
        waiting.push(waker);
    }
}

/// Runs while a signaller holds the child that keeps its group unreaped,
/// just before the SIGTERM: the seam a test pauses on to force a reap
/// against it.
#[cfg(test)]
pub(crate) fn before_signal() {
    crate::server::tests::before_signal();
}

#[cfg(not(test))]
pub(crate) fn before_signal() {}

/// Runs as a reap is about to take `which` lock (`child` or `live`): the
/// seam a test waits on to know the reap contends before it asserts.
#[cfg(test)]
pub(crate) fn before_lock(which: &'static str) {
    crate::server::tests::before_lock(which);
}

#[cfg(not(test))]
pub(crate) fn before_lock(_which: &'static str) {}

pub(crate) fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}
