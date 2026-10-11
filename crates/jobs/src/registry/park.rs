//! What `wait` and `stop` block on: the generation a job's end, a cancel
//! and a clock move all bump (`docs/tools.md`, "Background jobs"). The
//! registry holds one shared parker as a field and calls into it with this
//! free function, which never reaches into the registry's jobs.

use std::sync::Arc;
use std::time::Instant;

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;
use support::clock::Parker;

/// How a [`park_until`] wait ended.
pub(super) enum Parked {
    /// The job ended.
    Ended,
    /// The deadline passed.
    Timeout,
    /// The cancel fired.
    Cancelled,
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
    parker: &Parker,
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
        let seen = parker.generation();
        if let Some(parked) = check() {
            return parked;
        }
        #[cfg(test)]
        BEFORE_PARK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook();
            }
        });
        parker.park(clock, until, seen);
    }
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
