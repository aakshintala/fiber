//! A test's hang guard and each wait's own bound (`docs/testing.md`, "Waits
//! and timeouts"): the root deadline caps every wait, and each wait gets a
//! fresh bound made when it starts, so one slow step does not use up a
//! later step's time.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::clock::Clock;

use crate::clock::System;

/// nextest kills a test at 120 s (`.config/nextest.toml`): one wait's
/// longest single bound doubled still fits. The relations between the four
/// bounds are pinned in `deadline_tests.rs`.
pub const BUDGET: Duration = Duration::from_secs(110); // ruling: #3662
/// One wait's fresh bound, measured from the wait's start.
pub const WAIT: Duration = Duration::from_secs(40); // ruling: #3662
/// From the test's start, when its success-path waits must be over: the
/// hang guard every wait is capped by.
pub const WAITS: Duration = Duration::from_secs(90); // ruling: #3662
/// From the test's start, when cleanup waits must be over. `BUDGET -
/// CLEANUP` holds the fixed bounds inside `fakes` that take no deadline.
pub const CLEANUP: Duration = Duration::from_secs(100); // ruling: #3662

static PROCESS_CLOCK: System = System;

/// A test's hang guard, started at its first operation, and the per-wait
/// bounds it mints. The root's [`left`](Deadline::left) is a fresh
/// [`WAIT`] measured at the call, capped by what remains of [`WAITS`];
/// [`start_wait`](Deadline::start_wait) fixes one wait's bound at the call,
/// still capped by the guard, and that bound never refreshes. The waits of
/// one test end by [`BUDGET`].
#[derive(Clone, Copy)]
pub struct Deadline {
    start: Instant,
    wait: Duration,
    waits: Duration,
    cleanup: Duration,
    clock: &'static dyn Clock,
    wait_start: Option<Instant>,
}

impl Deadline {
    /// A deadline on the process clock, starting now.
    pub fn start() -> Self {
        Self::on(&PROCESS_CLOCK)
    }

    /// A deadline on `clock`, starting at its `now()`. For arithmetic tests
    /// only: a receive through it waits the fake remaining time on the wall
    /// clock, at most [`WAIT`]. A clock whose `now()` moves backwards
    /// renews it; [`crate::clock::Manual`] moves backwards only past
    /// offset overflow.
    pub fn on(clock: &'static dyn Clock) -> Self {
        Self {
            start: clock.now(),
            wait: WAIT,
            waits: WAITS,
            cleanup: CLEANUP,
            clock,
            wait_start: None,
        }
    }

    /// A deadline on the process clock with `wait` for its wait and its
    /// guard. For one wait outside a test's own deadline, such as a fixed
    /// bound a fake keeps.
    pub fn after(wait: Duration) -> Self {
        Self {
            start: PROCESS_CLOCK.now(),
            wait,
            waits: wait,
            cleanup: wait,
            clock: &PROCESS_CLOCK,
            wait_start: None,
        }
    }

    /// Starts one wait now: the returned deadline's
    /// [`left`](Deadline::left) is [`WAIT`] minus the span since this call
    /// (`wait` for an [`after`](Deadline::after) deadline), still capped by
    /// what remains of [`WAITS`], and never refreshes. Call it once at a
    /// wait's entry, before its first receive, and take every part of that
    /// wait through the result; one-off receives keep using the root.
    pub fn start_wait(&self) -> Self {
        Self {
            wait_start: Some(self.clock.now()),
            ..*self
        }
    }

    /// What remains for success-path waits: on the root, [`WAIT`] fresh at
    /// the call, capped by what remains of [`WAITS`]; on a started wait,
    /// what remains of the bound fixed when the wait started, still capped
    /// by the guard. Zero once either end passes. It never panics; a wait
    /// handed zero takes only an already-arrived result, then runs its own
    /// timeout branch. No `Instant + Duration` is computed anywhere, so no
    /// constructor or accessor panics for any [`Duration`], including
    /// [`Duration::MAX`].
    pub fn left(&self) -> Duration {
        let elapsed = self.clock.now().saturating_duration_since(self.start);
        let guard = self.waits.saturating_sub(elapsed);
        match self.wait_start {
            None => self.wait.min(guard),
            Some(fixed) => {
                let spent = self.clock.now().saturating_duration_since(fixed);
                self.wait.saturating_sub(spent).min(guard)
            }
        }
    }

    /// What remains for cleanup waits (a reap or group check after a wait
    /// expired, a watchdog's stand-down): zero from [`CLEANUP`] on.
    pub fn cleanup(&self) -> Duration {
        self.cleanup
            .saturating_sub(self.clock.now().saturating_duration_since(self.start))
    }

    /// A deadline whose [`left`](Deadline::left) is this one's
    /// [`cleanup`](Deadline::cleanup): a cleanup wait's own deadline.
    pub fn cleanup_phase(&self) -> Self {
        Self {
            wait: self.cleanup,
            waits: self.cleanup,
            wait_start: None,
            ..*self
        }
    }

    /// What remains of the deadline from `rx`: exactly one
    /// [`Receiver::recv_timeout`] call with [`left`](Deadline::left). At
    /// zero it still returns a value that has already arrived, and
    /// `Err(Timeout)` otherwise. It never panics.
    #[allow(
        clippy::disallowed_methods,
        reason = "the one receive every test wait goes through"
    )]
    pub fn recv<T>(&self, rx: &Receiver<T>) -> Result<T, RecvTimeoutError> {
        rx.recv_timeout(self.left())
    }

    /// [`recv`](Deadline::recv), failing the test on expiry: `Timeout`
    /// panics naming what the test waited for, and `Disconnected` panics
    /// saying the sender hung up. Through [`#[track_caller]`](track_caller)
    /// the panic's `Location` is the caller's line.
    #[track_caller]
    #[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
    pub fn recv_or_fail<T>(&self, rx: &Receiver<T>, what: &str) -> T {
        match self.recv(rx) {
            Ok(value) => value,
            Err(RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for {what}")
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the sender for {what} hung up before sending")
            }
        }
    }
}

#[cfg(test)]
#[path = "deadline_tests.rs"]
mod tests;
