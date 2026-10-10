//! The hub's shared wake: one [`Tick`] behind every hub wait. It is woken
//! on every clock move, every change to what a waiter checks (the open
//! connections, the feed's state) and every signal. A wake takes the tick
//! lock before notifying, so a waiter that has checked and not yet parked
//! blocks the waker until it parks instead of missing the wake: every hub
//! wait takes [`Tick::hold`] before reading what it checks and keeps that
//! guard into [`Tick::park`].

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
#[cfg(test)]
use std::sync::mpsc::Sender;
use std::time::Instant;

use contract::clock::{Clock, Wake};

/// Woken on every clock move, every change to what a waiter checks and
/// every signal.
#[derive(Default)]
pub(crate) struct Tick {
    held: Mutex<()>,
    moved: Condvar,
    /// Told once of the next wake: when it finds `held` taken, or else
    /// once its notify has returned.
    #[cfg(test)]
    pub(crate) attempt: Mutex<Option<Sender<()>>>,
}

impl Wake for Tick {
    fn wake(&self) {
        #[cfg(test)]
        let attempt = self.tell_if_contended();
        // Taken before the notify, so a waiter that has checked and not
        // yet parked cannot miss it.
        let held = lock(&self.held);
        self.moved.notify_all();
        drop(held);
        #[cfg(test)]
        if let Some(attempt) = attempt {
            attempt.send(()).unwrap_or(());
        }
    }
}

#[cfg(test)]
impl Tick {
    /// Tells the armed sender at once when `held` is taken, and otherwise
    /// returns it to be told after the notify.
    fn tell_if_contended(&self) -> Option<Sender<()>> {
        let attempt = lock(&self.attempt).take()?;
        if matches!(
            self.held.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ) {
            attempt.send(()).unwrap_or(());
            return None;
        }
        Some(attempt)
    }
}

/// What one check of [`Tick::wait_for`] found.
pub(crate) enum Wait {
    /// The wait is over.
    Done,
    /// Park until this instant (`None`: no deadline) or a wake.
    Until(Option<Instant>),
}

impl Tick {
    /// Returns once `clock` reads `until` or later.
    pub(crate) fn until(&self, clock: &dyn Clock, until: Instant) {
        self.wait_for(clock, &mut |now| {
            if now >= until {
                Wait::Done
            } else {
                Wait::Until(Some(until))
            }
        });
    }

    /// Calls `check` with the clock's reading, under the tick lock, until it
    /// returns [`Wait::Done`], parking on the clock between calls as it
    /// says. A wake after a change to what `check` reads is never missed:
    /// the change's `wake` takes the tick lock, so it waits until this
    /// thread parks.
    pub(crate) fn wait_for(&self, clock: &dyn Clock, check: &mut dyn FnMut(Instant) -> Wait) {
        loop {
            let guard = lock(&self.held);
            let Wait::Until(until) = check(clock.now()) else {
                return;
            };
            // The guard rides back unused: the check runs again under a
            // fresh lock. `None` only when the clock never invokes the
            // wait, which neither clock does.
            drop(self.park(clock, guard, until));
        }
    }

    /// Takes the tick lock: every hub wait takes it before reading what it
    /// checks and keeps the guard into [`Tick::park`], so a change whose
    /// wake takes `held` is never missed.
    pub(crate) fn hold(&self) -> MutexGuard<'_, ()> {
        lock(&self.held)
    }

    /// Parks the [`Tick::hold`] guard on the clock until `until` or a wake:
    /// one `support::clock::park` call over the tick's own condvar, with no
    /// early return of its own. `None` only when the clock never invokes
    /// the wait.
    pub(crate) fn park<'a>(
        &'a self,
        clock: &dyn Clock,
        guard: MutexGuard<'a, ()>,
        until: Option<Instant>,
    ) -> Option<MutexGuard<'a, ()>> {
        support::clock::park(clock, until, None, &self.moved, guard, |_| false)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
