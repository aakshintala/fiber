//! The clock seam (`docs/testing.md`, "Values that change every run"):
//! monotonic time for deadlines, wall time for `ts` and expiries, and
//! waiting. Like [`crate::tool::Tool`] and [`crate::signing::Signer`], it
//! defines the seam and contains no behaviour.

use std::sync::Weak;
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
