//! The slot's start lease: one in-flight start at a time, so a
//! concurrent start or stop waits for the attempt's outcome instead of
//! spawning beside it. The behavior lives here so `mod.rs` stays under
//! the size guideline: identical shapes, reached through
//! [`Slot::acquire`], which `serve`, `stop` and the re-list hold across
//! their attempts.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use super::Slot;

/// One in-flight start's hold on its slot: clearing the flag and waking
/// the waiters on drop, so an unwind releases the lease too.
pub(super) struct LeaseGuard<'a> {
    lease: &'a Mutex<bool>,
    wake: &'a Condvar,
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        *lock(self.lease) = false;
        self.wake.notify_all();
    }
}

/// Locks `state`, recovering from poison as the slot always has: a
/// panicking attempt never wedges the next one.
pub(super) fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Slot {
    /// Takes the slot's start lease, waiting while another attempt holds
    /// it: one start runs at a time, and a stop waits for the in-flight
    /// one. The mutex is never held across a start, only across the flag;
    /// the guard releases it on unwind too, so a panic never wedges the
    /// slot.
    pub(super) fn acquire(&self) -> LeaseGuard<'_> {
        let mut busy = lock(&self.lease);
        while *busy {
            #[cfg(test)]
            self.note_lease_wait();
            busy = self.wake.wait(busy).unwrap_or_else(PoisonError::into_inner);
        }
        *busy = true;
        LeaseGuard {
            lease: &self.lease,
            wake: &self.wake,
        }
    }

    /// Whether both locks are free right now: the state lock and the
    /// lease lock. Test-only: a test's timing guard calls it while the
    /// attempt's guard drops, proving no diagnostic drop holds either
    /// lock. A mutant that moves the drop under one fails the test.
    #[cfg(test)]
    pub(crate) fn locks_free(&self) -> bool {
        self.state.try_lock().is_ok() && self.lease.try_lock().is_ok()
    }

    /// Signals once when an acquirer finds the lease held. Test-only:
    /// installed before a contender starts, taken on its first wait.
    #[cfg(test)]
    fn note_lease_wait(&self) {
        if let Some(hook) = lock(&self.wait_hook).take() {
            hook.send(()).unwrap_or(());
        }
    }

    /// Installs the one-shot [`Slot::note_lease_wait`] signal. Test-only.
    #[cfg(test)]
    pub(crate) fn on_lease_wait(&self, hook: std::sync::mpsc::Sender<()>) {
        *lock(&self.wait_hook) = Some(hook);
    }
}
