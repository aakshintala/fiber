//! A polling wait for a predicate under one deadline (`docs/testing.md`,
//! "Waits and timeouts"): every wait carries the test's deadline on the
//! wall clock, and the poll pauses between probes instead of spinning.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::deadline::Deadline;
use crate::within::within;

/// How long each poll waits between predicate probes: one bounded wait
/// against the stop channel, so the poll holds no core between probes.
const POLL: Duration = Duration::from_millis(10);

/// True once `stop` is gone: the one bounded wait the poll loop shares,
/// holding no lock between probes.
fn stop_closed(stop: &Receiver<()>) -> bool {
    !matches!(
        Deadline::after(POLL).recv(stop),
        Err(RecvTimeoutError::Timeout)
    )
}

/// Waits up to what remains of `deadline` on the wall clock for `pred` to
/// hold, probing it on a thread once per poll pause. Fails the test naming
/// `what` when the deadline expires or `pred` panics. The poll thread ends
/// within one pause of the return and drops `pred` with it, so no thread
/// outlives the wait; the deadline still holds when `pred` is slow, because
/// the wait itself runs on the calling thread. `pred` must not block: it
/// runs on the poll thread, which only ends once `pred` returns.
#[track_caller]
pub fn wait_until(what: &str, deadline: Deadline, pred: impl Fn() -> bool + Send + 'static) {
    let (_stop, stopped) = std::sync::mpsc::channel::<()>();
    within(what, deadline, move || {
        while !pred() {
            if stop_closed(&stopped) {
                return;
            }
        }
    });
}

#[cfg(test)]
#[path = "wait_until_tests.rs"]
mod tests;
