//! The clock seam and the clock-parked wait (`docs/testing.md`, "Values that
//! change every run"): monotonic time for deadlines, wall time for `ts` and
//! expiries, and waiting. Callers take `&dyn Clock` or `Arc<dyn Clock>` and
//! never read the process clock themselves (`docs/code-quality.md`, "Lints").

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

/// A source of time. The real clock and a fake both implement it; callers
/// take `Arc<dyn Clock>` or `&dyn Clock` and never read the process clock
/// themselves.
pub trait Clock: Send + Sync {
    /// Monotonic time, for deadlines.
    fn now(&self) -> Instant;

    /// Wall-clock time, for `ts` and expiries a server states.
    fn wall(&self) -> SystemTime;

    /// Pauses the caller for `d` of this clock's time.
    fn sleep(&self, d: Duration);

    /// Blocks the caller once, via `wait`, until it is woken or `until`
    /// passes on this clock.
    ///
    /// `wait` receives the real-time bound to block for. `until` at or
    /// before [`Clock::now`]: `Some(Duration::ZERO)` on both clocks. Later:
    /// `Some(until - now)` on the real clock, `None` (block until woken) on
    /// a fake. `until == None`: `None` on both.
    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>));

    /// Registers a waiter to wake whenever this clock's time moves. The real
    /// clock ignores it.
    fn subscribe(&self, waker: Weak<dyn Wake>);
}

/// Wakes whoever is blocked in [`Clock::wait_until`] when the clock moves.
pub trait Wake: Send + Sync {
    /// Wakes every waiter this value stands for.
    fn wake(&self);
}

/// The operating system's clock: the only production [`Clock`] that reads
/// real time.
pub struct System;

impl Clock for System {
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::now"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::wall"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    // A wall-clock sleep; docs/testing.md, "Values that change every run",
    // permits only main's real-clock sleep test.
    #[cfg_attr(false, mutants::skip)]
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::sleep"
    )]
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        // `saturating_duration_since` is zero when `until` is at or before now,
        // so that branch needs no separate arm.
        let bound = until.map(|until| until.saturating_duration_since(self.now()));
        wait(bound);
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

/// Parks the caller on `changed` until `done` holds, handing back the guard
/// it was given. One [`Clock::wait_until`] call: when `done` already holds,
/// the guard goes back untouched; otherwise one condvar wait, bounded by
/// the shorter of the clock's bound and `poll` (`None` waits until
/// notified). For example:
/// `park(clock, Some(deadline), None, &shared.cv, guard, |s| s.seq != seen)`.
///
/// The caller's guard is held from its own check until the condvar wait
/// releases it, so a notify made under the same mutex after that check is
/// never missed. It waits at most once and never loops; the caller re-reads
/// its state and the clock after it returns. `None` only when the clock
/// never invokes the wait; both clocks invoke it exactly once.
pub fn park<'a, T>(
    clock: &dyn Clock,
    until: Option<Instant>,
    poll: Option<Duration>,
    changed: &Condvar,
    guard: MutexGuard<'a, T>,
    mut done: impl FnMut(&mut T) -> bool,
) -> Option<MutexGuard<'a, T>> {
    let mut slot = Some(guard);
    clock.wait_until(until, &mut |bound| {
        let Some(mut guard) = slot.take() else {
            return;
        };
        if done(&mut guard) {
            slot = Some(guard);
            return;
        }
        let guard = match bounded(bound, poll) {
            Some(limit) => changed
                .wait_timeout(guard, limit)
                .unwrap_or_else(PoisonError::into_inner)
                .0,
            None => changed.wait(guard).unwrap_or_else(PoisonError::into_inner),
        };
        slot = Some(guard);
    });
    slot
}

/// The shorter of the clock's bound and the caller's poll cap, either when
/// one is set. A site without polling passes `None` and still blocks until
/// notified.
fn bounded(bound: Option<Duration>, poll: Option<Duration>) -> Option<Duration> {
    match (bound, poll) {
        (Some(bound), Some(poll)) => Some(bound.min(poll)),
        (bound, None) => bound,
        (None, poll) => poll,
    }
}

/// A generation counter with a condvar that is itself a [`Wake`]: sites
/// that own a bare counter park on it instead of threading a guard through
/// [`park`]. A caller reads `seen = generation()` before its own checks;
/// any bump after that read makes [`Parker::park`] return at once.
#[derive(Default)]
pub struct Parker {
    seq: Mutex<u64>,
    changed: Condvar,
}

impl Parker {
    /// A parker no thread has bumped.
    pub fn new() -> Self {
        Self {
            seq: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    /// Counts one wake and wakes every parked thread.
    pub fn bump(&self) {
        let mut seq = crate::lock(&self.seq);
        *seq = seq.wrapping_add(1);
        self.changed.notify_all();
    }

    /// How many bumps have landed.
    pub fn generation(&self) -> u64 {
        *crate::lock(&self.seq)
    }

    /// Parks on the parker's own counter until a bump past `seen`, `until`
    /// passes on `clock`, or the clock wakes it: [`park`] over the counter.
    pub fn park(&self, clock: &dyn Clock, until: Option<Instant>, seen: u64) {
        let _guard = park(
            clock,
            until,
            None,
            &self.changed,
            crate::lock(&self.seq),
            |seq| *seq != seen,
        );
    }
}

impl Wake for Parker {
    fn wake(&self) {
        self.bump();
    }
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
