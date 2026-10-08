//! What `wait` and `stop` block on: the generation a job's end, a cancel
//! and a clock move all bump (`docs/tools.md`, "Background jobs"). The
//! registry holds one as a field and calls into it; this module owns the
//! counter and the condvar and never reaches into the registry's jobs.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;

/// What `wait` and `stop` block on. Every bump is a job's end, a cancel or
/// a clock move, so a wake that lands before the condvar wait is still
/// visible in the generation.
pub(super) struct Parker {
    seq: Mutex<u64>,
    cv: Condvar,
}

impl Parker {
    pub(super) fn new() -> Self {
        Self {
            seq: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    /// Bumps the generation and wakes every waiter.
    pub(super) fn bump(&self) {
        let mut seq = lock(&self.seq);
        *seq = seq.wrapping_add(1);
        self.cv.notify_all();
    }

    /// The current generation, for the wait's change check.
    pub(super) fn generation(&self) -> u64 {
        *lock(&self.seq)
    }

    /// Parks until `check` reports an end, or `cancel` fires. `until` of
    /// `None` waits without a deadline. `wake` is held for the whole park,
    /// so a cancel subscribed here can reach its registry; `check` reads
    /// the registry's own state and reports `Some` once the wait is over.
    /// The generation is snapshotted before the check runs: a bump that
    /// lands between them is seen by the check, and a bump after it trips
    /// the wait below, so no wake is ever slept through. The loop reads
    /// why it woke.
    pub(super) fn park_until(
        &self,
        clock: &dyn Clock,
        cancel: &dyn Cancel,
        until: Option<Instant>,
        wake: &Arc<dyn Wake>,
        check: impl Fn() -> Option<Parked>,
    ) -> Parked {
        // Held until this wait returns, so the cancel's weak can upgrade
        // for the whole park. The clock was subscribed in `new`.
        let _hold = Arc::clone(wake);
        cancel.subscribe(Arc::downgrade(wake));
        if cancel.is_cancelled() {
            return Parked::Cancelled;
        }
        loop {
            let seen = self.generation();
            if let Some(parked) = check() {
                return parked;
            }
            #[cfg(test)]
            BEFORE_PARK.with(|slot| {
                if let Some(hook) = slot.borrow_mut().take() {
                    hook();
                }
            });
            self.park(clock, until, seen);
        }
    }

    /// One wait on the clock. A bump between the generation read and this
    /// wait is still visible: the generation is checked under the lock
    /// before sleeping, so the wait does not sleep through it. The loop
    /// reads why it woke.
    pub(super) fn park(&self, clock: &dyn Clock, until: Option<Instant>, seen: u64) {
        let mut slot = Some(lock(&self.seq));
        clock.wait_until(until, &mut |bound| {
            let Some(seq) = slot.take() else {
                return;
            };
            if *seq != seen {
                slot = Some(seq);
                return;
            }
            slot = Some(match bound {
                Some(timeout) => {
                    self.cv
                        .wait_timeout(seq, timeout)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.cv.wait(seq).unwrap_or_else(PoisonError::into_inner),
            });
        });
    }
}

fn lock(seq: &Mutex<u64>) -> MutexGuard<'_, u64> {
    seq.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) enum Parked {
    Ended,
    Timeout,
    Cancelled,
}

#[cfg(test)]
#[path = "park_tests.rs"]
mod tests;

// One shot on the waiter, after it has read its state and before it waits.
// No registry lock is held. `wait_until` cannot host this: the parking
// lock is already taken there, so ending the job from the clock would
// deadlock.
#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_PARK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}
