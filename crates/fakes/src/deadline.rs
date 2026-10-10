//! One deadline for a test's waits (`docs/testing.md`, "Waits and timeouts"):
//! every wait takes what remains of it, so a second wait gets only what the
//! first left. Moved from `crates/main/tests/support/mod.rs`, so every
//! crate's tests share it.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use contract::clock::Clock;

use crate::clock::SystemClock;

/// nextest kills a test at 120 s (`.config/nextest.toml`): a test's
/// deadlines sum to half of that.
pub const BUDGET: Duration = Duration::from_secs(60);
/// From the test's start, when its success-path waits must be over.
pub const WAITS: Duration = Duration::from_secs(40);
/// From the test's start, when cleanup waits must be over. `BUDGET -
/// CLEANUP` holds the fixed bounds inside `fakes` that take no deadline.
pub const CLEANUP: Duration = Duration::from_secs(50);

static PROCESS_CLOCK: SystemClock = SystemClock;

/// The test's one deadline, started at its first operation. Every wait
/// takes what remains of it, so a second wait gets only what the first
/// left, and the waits of one test sum to [`BUDGET`].
#[derive(Clone, Copy)]
pub struct Deadline {
    start: Instant,
    waits: Duration,
    cleanup: Duration,
    clock: &'static dyn Clock,
}

impl Deadline {
    /// A deadline on the process clock, starting now.
    pub fn start() -> Self {
        Self::on(&PROCESS_CLOCK)
    }

    /// A deadline on `clock`, starting at its `now()`. For arithmetic tests
    /// only: a receive through it waits the fake remaining time on the wall
    /// clock, at most [`WAITS`]. A clock whose `now()` moves backwards
    /// renews it; [`crate::clock::FakeClock`] moves backwards only past
    /// offset overflow.
    pub fn on(clock: &'static dyn Clock) -> Self {
        Self {
            start: clock.now(),
            waits: WAITS,
            cleanup: CLEANUP,
            clock,
        }
    }

    /// A deadline on the process clock with `wait` for both its waits. For
    /// one wait outside a test's own deadline, such as a fixed bound a fake
    /// keeps.
    pub fn after(wait: Duration) -> Self {
        Self {
            start: PROCESS_CLOCK.now(),
            waits: wait,
            cleanup: wait,
            clock: &PROCESS_CLOCK,
        }
    }

    /// What remains for success-path waits: zero from [`WAITS`] after the
    /// start on, every time. It never panics; a wait handed zero takes only
    /// an already-arrived result, then runs its own timeout branch. No
    /// `Instant + Duration` is computed anywhere, so no constructor or
    /// accessor panics for any [`Duration`], including [`Duration::MAX`].
    pub fn left(&self) -> Duration {
        self.waits
            .saturating_sub(self.clock.now().saturating_duration_since(self.start))
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
            waits: self.cleanup,
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
