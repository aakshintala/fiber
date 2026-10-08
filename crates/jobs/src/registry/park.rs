//! What `wait` and `stop` block on: the `seq` counter and condvar a job's
//! end, a cancel and a clock move all bump
//! (`docs/tools.md`, "Background jobs"). This is the state boundary the
//! waits sleep on, split from the registry that owns the jobs.

use std::sync::{Arc, Condvar, PoisonError};
use std::time::Instant;

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;

use super::{Inner, Phase, Registry, lock};

impl Registry {
    /// Parks until `id` ends, `until` passes, or `cancel` fires. `until`
    /// of `None` waits without a deadline. The `Arc` stays alive for the
    /// park: a cancel subscribed here upgrades it.
    pub(super) fn park_until(
        self: &Arc<Self>,
        id: &str,
        until: Option<Instant>,
        cancel: &dyn Cancel,
    ) -> Parked {
        // Held until this wait returns, so the cancel's weak can upgrade
        // for the whole park. The clock was subscribed in `new`.
        let registry = Arc::clone(self);
        let wake: Arc<dyn Wake> = registry;
        cancel.subscribe(Arc::downgrade(&wake));
        let _wake = wake;
        if cancel.is_cancelled() {
            return Parked::Cancelled;
        }
        loop {
            let seen = {
                let inner = lock(&self.inner);
                if ended(&inner, id) {
                    return Parked::Ended;
                }
                if cancel.is_cancelled() {
                    return Parked::Cancelled;
                }
                if timed_out(self.clock.as_ref(), until) {
                    return Parked::Timeout;
                }
                inner.seq
            };
            #[cfg(test)]
            BEFORE_PARK.with(|slot| {
                if let Some(hook) = slot.borrow_mut().take() {
                    hook();
                }
            });
            self.park_once(until, seen);
        }
    }

    /// One wait on the clock. The mutex is taken before `wait_until` and
    /// held until the condvar wait, so a wake blocks on it instead of
    /// notifying nobody.
    fn park_once(&self, until: Option<Instant>, seen: u64) {
        let mut slot = Some(lock(&self.inner));
        self.clock.wait_until(until, &mut |bound| {
            let Some(guard) = slot.take() else {
                return;
            };
            // An end, a cancel and a clock wake all bump `seq` under this
            // lock before they notify. A change after `seen` was read is
            // still visible, so the wait does not sleep through it. The
            // loop reads why it woke.
            if guard.seq != seen {
                slot = Some(guard);
                return;
            }
            slot = Some(match bound {
                Some(timeout) => {
                    self.cv
                        .wait_timeout(guard, timeout)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.cv.wait(guard).unwrap_or_else(PoisonError::into_inner),
            });
        });
    }
}

impl Wake for Registry {
    fn wake(&self) {
        let mut inner = lock(&self.inner);
        bump(&mut inner, &self.cv);
    }
}

pub(super) enum Parked {
    Ended,
    Timeout,
    Cancelled,
}

// One shot on the waiter, after it has read `seq` and before it waits.
// The registry lock is not held. `wait_until` cannot host this: the lock
// is already taken there, so ending the job from the clock would deadlock.
#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_PARK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn bump(inner: &mut Inner, cv: &Condvar) {
    inner.seq = inner.seq.wrapping_add(1);
    cv.notify_all();
}

fn ended(inner: &Inner, id: &str) -> bool {
    inner
        .jobs
        .iter()
        .any(|job| job.started.job_id.0 == id && matches!(job.phase, Phase::Ended(_)))
}

fn timed_out(clock: &dyn Clock, until: Option<Instant>) -> bool {
    until.is_some_and(|until| clock.now() >= until)
}
