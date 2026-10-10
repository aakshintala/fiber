//! Tests of [`Deadline`](super::Deadline) (`docs/testing.md`, "Waits and
//! timeouts"): one deadline from the test's start, each wait taking only
//! what remains of it. The arithmetic tests drive a fake clock that the
//! code under test reads and never waits on; the receives wait on the wall
//! clock with at most [`WAITS`](super::WAITS), and only at a zero or short
//! bound, so no test can hang on them.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::clock::FakeClock;

use super::{Deadline, WAITS};

/// A fake clock that lives as long as the test process, as a deadline's
/// clock must.
fn fake_clock() -> &'static Arc<FakeClock> {
    Box::leak(Box::new(FakeClock::new()))
}

#[test]
fn a_second_wait_gets_only_what_the_first_left() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    assert_eq!(deadline.left(), Duration::from_secs(40));
    clock.advance(Duration::from_secs(15));
    assert_eq!(deadline.left(), Duration::from_secs(25));
    clock.advance(Duration::from_millis(24_999));
    assert_eq!(deadline.left(), Duration::from_millis(1));
}

#[test]
fn left_is_zero_from_the_waits_on_and_never_renews() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    assert_eq!(deadline.left(), WAITS);
    clock.advance(Duration::from_secs(15));
    assert_eq!(deadline.left(), Duration::from_secs(25));
    clock.advance(Duration::from_secs(24) + Duration::from_millis(999));
    assert_eq!(deadline.left(), Duration::from_millis(1));
    clock.advance(Duration::from_millis(1));
    assert_eq!(deadline.left(), Duration::ZERO);
    clock.advance(Duration::from_secs(1));
    assert_eq!(deadline.left(), Duration::ZERO);
    clock.advance(Duration::from_secs(u64::MAX / 4));
    assert_eq!(deadline.left(), Duration::ZERO);
}

#[test]
fn no_time_left_gives_zero_and_never_renews() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    clock.advance(Duration::from_secs(40));
    assert_eq!(deadline.left(), Duration::ZERO);
    clock.advance(Duration::from_secs(1));
    assert_eq!(deadline.left(), Duration::ZERO);
    let (tx, rx) = mpsc::channel();
    tx.send(7).unwrap();
    assert_eq!(deadline.recv(&rx), Ok(7));
    assert_eq!(deadline.recv(&rx), Err(mpsc::RecvTimeoutError::Timeout));
}

#[test]
fn cleanup_keeps_its_reserve_after_the_waits_end() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    assert_eq!(deadline.cleanup(), Duration::from_secs(50));
    clock.advance(Duration::from_secs(40));
    assert_eq!(deadline.cleanup(), Duration::from_secs(10));
    clock.advance(Duration::from_secs(10));
    assert_eq!(deadline.cleanup(), Duration::ZERO);
    clock.advance(Duration::from_secs(20));
    assert_eq!(deadline.cleanup(), Duration::ZERO);
}

#[test]
fn cleanup_phase_left_is_cleanup_at_three_instants() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    assert_eq!(deadline.cleanup_phase().left(), deadline.cleanup());
    clock.advance(WAITS);
    assert_eq!(deadline.cleanup_phase().left(), deadline.cleanup());
    assert_eq!(deadline.cleanup(), Duration::from_secs(10));
    clock.advance(Duration::from_secs(10));
    assert_eq!(deadline.cleanup_phase().left(), Duration::ZERO);
    assert_eq!(deadline.cleanup(), Duration::ZERO);
}

#[test]
fn after_the_longest_duration_never_panics_and_leaves_years() {
    let wait = Deadline::after(Duration::MAX);
    let thousand_years = Duration::from_secs(1_000 * 365 * 24 * 3_600);
    assert!(wait.left() > thousand_years);
    assert!(wait.cleanup() > thousand_years);
}

#[test]
fn after_five_seconds_left_stays_within_its_span() {
    let wait = Deadline::after(Duration::from_secs(5));
    let left = wait.left();
    assert!(
        left <= Duration::from_secs(5),
        "no more than it started with"
    );
    assert!(left > Duration::from_secs(4), "all but an instant of it");
}

#[test]
fn recv_at_zero_takes_what_arrived_and_nothing_else() {
    let wait = Deadline::after(Duration::ZERO);
    let (tx, rx) = mpsc::channel();
    tx.send(7).unwrap();
    assert_eq!(wait.recv(&rx), Ok(7));
    assert_eq!(wait.recv(&rx), Err(mpsc::RecvTimeoutError::Timeout));
    drop(tx);
    assert_eq!(wait.recv(&rx), Err(mpsc::RecvTimeoutError::Disconnected));
}

#[test]
fn recv_or_fail_names_the_wait_at_the_callers_line() {
    // nextest runs one test per process, so the hook below sees only this
    // test's panic.
    let hook = std::panic::take_hook();
    let seen = Arc::new(Mutex::new(None));
    let seen_at = Arc::clone(&seen);
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| (location.file().to_owned(), location.line()));
        *seen_at.lock().unwrap() = Some((location, info.to_string()));
    }));
    let wait = Deadline::after(Duration::ZERO);
    let (_tx, rx) = mpsc::channel::<()>();
    let line = line!() + 2;
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait.recv_or_fail(&rx, "a late reply")
    }));
    std::panic::set_hook(hook);
    failed.unwrap_err();
    let (location, message) = seen.lock().unwrap().take().unwrap();
    assert_eq!(location, Some((file!().to_owned(), line)));
    assert!(message.contains("a late reply"), "{message}");
    assert!(
        message.contains("waited until the deadline for"),
        "{message}"
    );
}

#[test]
fn recv_or_fail_says_the_sender_hung_up() {
    let wait = Deadline::after(Duration::ZERO);
    let (tx, rx) = mpsc::channel::<()>();
    drop(tx);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait.recv_or_fail(&rx, "a late reply")
    }));
    let message = failed
        .unwrap_err()
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        message.contains("the sender for a late reply hung up before sending"),
        "{message}"
    );
}
