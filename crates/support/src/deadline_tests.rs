//! Tests of [`Deadline`](super::Deadline) (`docs/testing.md`, "Waits and
//! timeouts"): the root deadline guards the test, and each wait gets its
//! own bound made when it starts. The arithmetic tests drive a fake clock
//! that the code under test reads and never waits on; the receives wait on
//! the wall clock with at most [`WAIT`](super::WAIT), and only at a zero or
//! short bound, so no test can hang on them.

use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use fakes::MUST_SUCCEED_WITHIN;

use crate::clock::{Clock, Manual as FakeClock};

use super::{BUDGET, CLEANUP, Deadline, WAIT, WAITS};

/// The span `after_ten_seconds_left_stays_within_its_span` asserts on: the
/// must-succeed floor, with the base's one-second slack kept below.
const SPAN: Duration = Duration::from_secs(10);

/// A fake clock that lives as long as the test process, as a deadline's
/// clock must.
fn fake_clock() -> &'static Arc<FakeClock> {
    Box::leak(Box::new(FakeClock::new()))
}

#[test]
fn left_is_zero_from_the_waits_on_and_never_renews() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    // A fresh root offers the full per-wait bound, not the whole guard.
    assert_eq!(deadline.left(), WAIT);
    // The guard still holds more than one wait: the per-wait bound caps.
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(49));
    assert_eq!(deadline.left(), WAIT);
    // The guard holds exactly one wait: still the full bound.
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(1));
    assert_eq!(deadline.left(), WAIT);
    // The guard holds less than one wait: the guard caps from here on.
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(1));
    assert_eq!(deadline.left(), Duration::from_secs(39));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(38) + Duration::from_millis(999));
    assert_eq!(deadline.left(), Duration::from_millis(1));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(1));
    assert_eq!(deadline.left(), Duration::ZERO);
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(1));
    assert_eq!(deadline.left(), Duration::ZERO);
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(u64::MAX / 4));
    assert_eq!(deadline.left(), Duration::ZERO);
}

#[test]
fn no_time_left_gives_zero_and_never_renews() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(WAITS);
    assert_eq!(deadline.left(), Duration::ZERO);
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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
    assert_eq!(deadline.cleanup(), Duration::from_secs(100));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(WAITS);
    assert_eq!(deadline.cleanup(), Duration::from_secs(10));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(10));
    assert_eq!(deadline.cleanup(), Duration::ZERO);
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(20));
    assert_eq!(deadline.cleanup(), Duration::ZERO);
}

#[test]
fn cleanup_phase_left_is_cleanup_at_three_instants() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    assert_eq!(deadline.cleanup_phase().left(), deadline.cleanup());
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(WAITS);
    assert_eq!(deadline.cleanup_phase().left(), deadline.cleanup());
    assert_eq!(deadline.cleanup(), Duration::from_secs(10));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(10));
    assert_eq!(deadline.cleanup_phase().left(), Duration::ZERO);
    assert_eq!(deadline.cleanup(), Duration::ZERO);
}

#[test]
fn cleanup_phase_after_a_started_wait_keeps_the_cleanup_bound() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let wait = deadline.start_wait();
    // Past one wait's fresh bound, inside the guard: the started wait is
    // spent, while cleanup still holds its reserve.
    let spent = WAIT + Duration::from_secs(10);
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(spent);
    assert_eq!(wait.left(), Duration::ZERO);
    // The cleanup phase offers the cleanup bound remaining from the original
    // guard, not the started wait's leftover.
    let phase = wait.cleanup_phase();
    assert_eq!(phase.left(), deadline.cleanup());
    assert_eq!(phase.left(), CLEANUP.saturating_sub(spent));
}

#[test]
fn after_the_longest_duration_never_panics_and_leaves_years() {
    let wait = Deadline::after(Duration::MAX);
    let thousand_years = Duration::from_secs(1_000 * 365 * 24 * 3_600);
    assert!(wait.left() > thousand_years);
    assert!(wait.cleanup() > thousand_years);
}

#[test]
fn after_ten_seconds_left_stays_within_its_span() {
    let wait = Deadline::after(SPAN);
    let left = wait.left();
    assert!(left <= SPAN, "no more than it started with");
    assert!(left > Duration::from_secs(9), "all but an instant of it");
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
fn the_bounds_keep_their_order_and_fit_nextests_kill() {
    assert!(WAIT <= WAITS && WAITS < CLEANUP && CLEANUP < BUDGET);
    assert!(BUDGET < Duration::from_secs(120));
    assert!(WAIT * 2 <= Duration::from_secs(120));
    assert!(BUDGET.saturating_sub(CLEANUP) >= Duration::from_secs(10));
}

#[test]
fn a_slow_first_wait_leaves_the_next_wait_its_full_bound() {
    let clock = fake_clock();
    let guard = Deadline::on(&**clock);
    let first = guard.start_wait();
    thread::scope(|scope| {
        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<Instant>();
        scope.spawn(move || {
            let until = clock
                .now()
                .checked_add(WAIT)
                .expect("one wait fits the clock");
            clock.wait_until(Some(until), &mut |_bound| {
                let _sent = parked_tx.send(());
                // Hang guard only: the test releases this after advancing.
                let _arrived = Deadline::after(MUST_SUCCEED_WITHIN).recv(&release_rx);
            });
            let _sent = done_tx.send(clock.now());
        });
        // Hang guard only: the first wait parks before time may move.
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&parked_rx)
            .expect("waited for the first wait to park");
        // The first wait sits parked a full WAIT; only now may time move.
        clock
            .advance_when_parked_in(WAIT, MUST_SUCCEED_WITHIN)
            .expect("waited for the park at now + WAIT");
        let _sent = release_tx.send(());
        let answered = Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&done_rx)
            .expect("waited for the waiter to leave its park");
        let origin = clock.origin().checked_add(WAIT).expect("one wait fits");
        assert_eq!(answered, origin);
        // The first wait spent its whole bound.
        assert_eq!(first.left(), Duration::ZERO);
        // The next wait still gets its full fresh bound, not the remainder
        // (`docs/testing.md:243`: one slow step does not use up a later
        // step's time).
        assert_eq!(guard.left(), WAIT);
        assert_eq!(guard.start_wait().left(), WAIT);
    });
}

#[test]
fn a_started_wait_does_not_refresh_per_receive() {
    let clock = fake_clock();
    let guard = Deadline::on(&**clock);
    let drained = Duration::from_secs(10);
    let (tx, rx) = mpsc::channel();
    tx.send(1).unwrap();
    tx.send(2).unwrap();
    let wait = guard.start_wait();
    assert_eq!(wait.recv(&rx), Ok(1));
    thread::scope(|scope| {
        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        scope.spawn(move || {
            let until = clock
                .now()
                .checked_add(drained)
                .expect("the drain fits the clock");
            clock.wait_until(Some(until), &mut |_bound| {
                let _sent = parked_tx.send(());
                // Hang guard only: the test releases this after advancing.
                let _arrived = Deadline::after(MUST_SUCCEED_WITHIN).recv(&release_rx);
            });
            let _sent = done_tx.send(());
        });
        // Hang guard only: the waiter parks before time may move.
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&parked_rx)
            .expect("waited for the waiter to park");
        // Mid-drain time moves; only now may the bound shrink.
        clock
            .advance_when_parked_in(drained, MUST_SUCCEED_WITHIN)
            .expect("waited for the park at now + the drain");
        let _sent = release_tx.send(());
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&done_rx)
            .expect("waited for the waiter to leave its park");
    });
    // The started wait's bound fell by the advanced span: no receive
    // renewed it (`docs/testing.md:243`: the parts of one wait take that one
    // deadline, never a fresh one per receive). The root still offers a
    // fresh bound.
    assert_eq!(
        wait.left(),
        WAIT.checked_sub(drained)
            .expect("the drain is shorter than a wait")
    );
    assert_eq!(guard.left(), WAIT);
    assert_eq!(wait.recv(&rx), Ok(2));
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
