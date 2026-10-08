//! Tests for the parking generation: a wake that lands anywhere around
//! the state check is never slept through.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fakes::clock::FakeClock;
use fakes::{CancelToken, within};

use super::{Parked, Parker};

/// How long a test waits on the wall clock before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// A wake the waiting test never fires: `park_until` only holds it for
/// the cancel subscription.
struct NoopWake;

impl contract::clock::Wake for NoopWake {
    fn wake(&self) {}
}

#[test]
fn the_generation_counts_every_bump() {
    let parker = Parker::new();
    assert_eq!(parker.generation(), 0);
    parker.bump();
    parker.bump();
    parker.bump();
    assert_eq!(parker.generation(), 3);
}

#[test]
fn a_bump_between_check_and_park_returns_at_once() {
    let parker = Arc::new(Parker::new());
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let wake: Arc<dyn contract::clock::Wake> = Arc::new(NoopWake);
    // The check bumps the parker itself on its first call, as a job's end
    // landing between the state read and the wait below would. The wait
    // must see the new generation and return at once instead of blocking
    // with no deadline; the next check then ends the wait.
    let ended = within("the wait ends", DEADLINE, {
        let parker = Arc::clone(&parker);
        let clock = Arc::clone(&clock);
        move || {
            let cancel = CancelToken::new();
            let checks = AtomicUsize::new(0);
            parker.park_until(clock.as_ref(), &cancel, None, &wake, || {
                if checks.fetch_add(1, Ordering::SeqCst) == 0 {
                    parker.bump();
                    return None;
                }
                Some(Parked::Ended)
            })
        }
    });
    assert!(matches!(ended, Parked::Ended));
}
