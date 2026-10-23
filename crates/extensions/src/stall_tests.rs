#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock as _;
use fakes::Deadline;
use fakes::MUST_SUCCEED_WITHIN;
use fakes::clock::FakeClock;

use super::{advance_past_grace_or_answer, await_grace_or_answer};

#[track_caller]
fn park(clock: &Arc<FakeClock>, until: Instant) -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
    let (release_tx, release_rx) = mpsc::channel();
    let (exited_tx, exited_rx) = mpsc::channel();
    let parked_clock = Arc::clone(clock);
    thread::spawn(move || {
        parked_clock.wait_until(Some(until), &mut |_bound| {
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&release_rx)
                .expect("waited for the test to release the parked thread");
        });
        let _sent = exited_tx.send(());
    });
    assert!(
        clock.await_parked(until, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the thread to park at the grace"
    );
    (release_tx, exited_rx)
}

#[track_caller]
fn release(release_tx: mpsc::Sender<()>, exited_rx: mpsc::Receiver<()>, wait: &Deadline) {
    release_tx
        .send(())
        .expect("the parked thread is waiting for release");
    wait.recv(&exited_rx)
        .expect("waited for the parked thread to exit");
}

#[test]
fn answer_before_the_park_returns_the_answer() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (answer_tx, answer_rx) = mpsc::channel();
    answer_tx.send("answer").expect("the answer is queued");

    assert_eq!(
        await_grace_or_answer(&clock, &answer_rx, until),
        Some("answer")
    );
}

#[test]
fn park_before_the_answer_returns_the_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (release_tx, exited_rx) = park(&clock, until);
    let (_answer_tx, answer_rx) = mpsc::channel::<()>();

    let outcome = await_grace_or_answer(&clock, &answer_rx, until);
    release(release_tx, exited_rx, &Deadline::after(MUST_SUCCEED_WITHIN));

    assert_eq!(outcome, None);
}

#[test]
fn answer_wins_when_park_and_answer_are_both_present() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (release_tx, exited_rx) = park(&clock, until);
    let (answer_tx, answer_rx) = mpsc::channel();
    answer_tx.send("answer").expect("the answer is queued");

    let outcome = await_grace_or_answer(&clock, &answer_rx, until);
    release(release_tx, exited_rx, &Deadline::after(MUST_SUCCEED_WITHIN));

    assert_eq!(outcome, Some("answer"));
}

#[track_caller]
fn run_that_answers_after_release(
    clock: &Arc<FakeClock>,
    kill_at: Instant,
) -> (
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
    mpsc::Receiver<&'static str>,
) {
    let (answer_tx, answer_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (left_tx, left_rx) = mpsc::channel::<()>();
    let worker = Arc::clone(clock);
    thread::spawn(move || {
        worker.wait_until(Some(kill_at), &mut |_bound| {
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&go_rx)
                .expect("waited for the test to release the parked run");
        });
        // The park entry drops before the answer is sent: the run
        // already left the grace park when the post-park step runs.
        answer_tx
            .send("answer")
            .expect("the test waits for the answer");
        left_tx
            .send(())
            .expect("the test waits for the run to leave");
    });
    (go_tx, left_rx, answer_rx)
}

#[test]
fn answer_after_the_sighting_returns_the_answer_without_advancing() {
    let clock = Arc::new(FakeClock::new());
    let kill_at = clock.now() + Duration::from_secs(30);
    let advance = Duration::from_secs(5);
    let (go_tx, left_rx, answer_rx) = run_that_answers_after_release(&clock, kill_at);
    // The sighting sees the held grace park and no answer yet.
    assert_eq!(await_grace_or_answer(&clock, &answer_rx, kill_at), None);
    go_tx
        .send(())
        .expect("the run is parked waiting for release");
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&left_rx)
        .expect("waited for the run to leave the park and answer");
    let before = clock.now();
    let outcome = advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
    assert_eq!(outcome, Some("answer"));
    assert_eq!(clock.now(), before, "the answer arrives with no clock move");
}

#[test]
fn held_park_advances_and_returns_no_answer() {
    let clock = Arc::new(FakeClock::new());
    let kill_at = clock.now() + Duration::from_secs(30);
    let advance = Duration::from_secs(5);
    let (release_tx, exited_rx) = park(&clock, kill_at);
    let (_answer_tx, answer_rx) = mpsc::channel::<()>();

    let outcome = advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
    release(release_tx, exited_rx, &Deadline::after(MUST_SUCCEED_WITHIN));

    assert_eq!(outcome, None);
    assert_eq!(
        clock.now(),
        clock.origin() + advance,
        "the held park moves the clock past the grace"
    );
}

#[test]
fn sighted_answer_returns_without_advancing() {
    let clock = FakeClock::new();
    let kill_at = clock.origin() + Duration::from_secs(30);
    let advance = Duration::from_secs(5);
    let before = clock.now();
    let (_answer_tx, answer_rx) = mpsc::channel::<&str>();

    let outcome =
        advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, Some("answer"));

    assert_eq!(outcome, Some("answer"));
    assert_eq!(clock.now(), before, "the sighted answer moves no clock");
}

#[test]
fn wrong_park_with_an_answer_returns_the_answer() {
    let clock = Arc::new(FakeClock::new());
    let kill_at = clock.now() + Duration::from_secs(30);
    let elsewhere = clock.now() + Duration::from_secs(10);
    let advance = Duration::from_secs(5);
    let (release_tx, exited_rx) = park(&clock, elsewhere);
    let (answer_tx, answer_rx) = mpsc::channel();
    answer_tx.send("answer").expect("the answer is queued");

    let outcome = advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
    release(release_tx, exited_rx, &Deadline::after(MUST_SUCCEED_WITHIN));

    assert_eq!(outcome, Some("answer"));
}

/// Releases a parked thread when a `should_panic` test unwinds past it, so
/// the thread never waits out its bound.
struct ReleaseOnDrop(mpsc::Sender<()>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _sent = self.0.send(());
    }
}

#[test]
#[should_panic(expected = "watched park")]
fn wrong_park_without_an_answer_panics() {
    let clock = Arc::new(FakeClock::new());
    let kill_at = clock.now() + Duration::from_secs(30);
    let elsewhere = clock.now() + Duration::from_secs(10);
    let advance = Duration::from_secs(5);
    let (release_tx, _exited_rx) = park(&clock, elsewhere);
    // The panic unwinds past this test: release the parked thread on
    // the way out so it never waits out its bound.
    let _release = ReleaseOnDrop(release_tx);
    let (_answer_tx, answer_rx) = mpsc::channel::<()>();

    advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
}

#[test]
#[should_panic(expected = "the stalled run neither parks for the grace nor answers")]
fn post_park_neither_park_nor_answer_hits_the_hang_guard() {
    // The negative proves itself only by exhausting the shared
    // ROUNDS x SIGHT bound: no park and no answer ever arrive, so
    // no signal can release the wait early. The clock's pause
    // points are #[cfg(test)]-private inside fakes
    // (crates/fakes/src/clock.rs:120-127, 555-589) and unreachable
    // from extensions, and ROUNDS/SIGHT are shared with the
    // sighting helper, so shortening them would loosen its guard
    // too. Same 8 x 200 ms wait as the pre-existing sighting
    // hang-guard test below.
    let clock = FakeClock::new();
    let kill_at = clock.origin() + Duration::from_secs(30);
    let advance = Duration::from_secs(5);
    let (_answer_tx, answer_rx) = mpsc::channel::<()>();

    advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
}

#[test]
#[should_panic(expected = "the stalled run neither parks for the grace nor answers")]
fn neither_park_nor_answer_hits_the_hang_guard() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (_answer_tx, answer_rx) = mpsc::channel::<()>();

    await_grace_or_answer(&clock, &answer_rx, until);
}

/// Wakes the parked run below on every clock move: the run's second park
/// ends when the helper's follow moves the clock to it.
struct MoveWaker(mpsc::Sender<()>);

impl contract::clock::Wake for MoveWaker {
    fn wake(&self) {
        let _sent = self.0.send(());
    }
}

/// A run that parks at `kill_at` until the test releases it, then re-parks
/// at `park_at` until the clock moves, then answers. Every wait is a park
/// or a channel signal: no real-clock hold.
#[track_caller]
fn run_that_reparks(
    clock: &Arc<FakeClock>,
    kill_at: Instant,
    park_at: Instant,
) -> (
    mpsc::Sender<()>,
    mpsc::Receiver<&'static str>,
    Arc<dyn contract::clock::Wake>,
) {
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let (answer_tx, answer_rx) = mpsc::channel();
    let waker: Arc<dyn contract::clock::Wake> = Arc::new(MoveWaker(wake_tx));
    clock.subscribe(Arc::downgrade(&waker));
    let worker = Arc::clone(clock);
    thread::spawn(move || {
        worker.wait_until(Some(kill_at), &mut |_bound| {
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&go_rx)
                .expect("waited for the test to release the parked run");
        });
        worker.wait_until(Some(park_at), &mut |_bound| {
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&wake_rx)
                .expect("waited for the clock to move to the re-park");
        });
        answer_tx
            .send("answer")
            .expect("the test waits for the answer");
    });
    (go_tx, answer_rx, waker)
}

/// A run that leaves the grace park and re-parks at `kill_at + offset` is
/// followed there: the advance moves the clock to the re-park, not past
/// the grace, and the run's answer still arrives.
#[track_caller]
fn assert_followed_at(offset: Duration) {
    let clock = Arc::new(FakeClock::new());
    let kill_at = clock.now() + Duration::from_secs(30);
    let repark = kill_at + offset;
    let advance = Duration::from_secs(5);
    let (go_tx, answer_rx, _waker) = run_that_reparks(&clock, kill_at, repark);

    // The sighting sees the held grace park and no answer yet.
    assert_eq!(await_grace_or_answer(&clock, &answer_rx, kill_at), None);
    go_tx
        .send(())
        .expect("the run is parked waiting for release");
    assert!(
        clock.await_parked(repark, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the run to park at the re-park"
    );
    let outcome = advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
    assert_eq!(outcome, None);
    assert_eq!(
        clock.now(),
        repark,
        "the clock moved to the watched instant, not kill_at"
    );
    let answer = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&answer_rx)
        .expect("waited for the run to answer after the follow");
    assert_eq!(answer, "answer");
}

#[test]
fn a_run_that_reparks_past_the_grace_is_followed_to_its_park() {
    assert_followed_at(Duration::from_secs(1));
}

#[test]
fn a_run_that_reparks_at_the_drain_bound_is_followed() {
    assert_followed_at(support::group::DRAIN);
}

#[test]
#[should_panic(expected = "watched park")]
fn a_park_past_the_drain_bound_still_panics() {
    let clock = Arc::new(FakeClock::new());
    let kill_at = clock.now() + Duration::from_secs(30);
    let past = kill_at + support::group::DRAIN + Duration::from_secs(1);
    let advance = Duration::from_secs(5);
    let (release_tx, _exited_rx) = park(&clock, past);
    // The panic unwinds past this test: release the parked thread on
    // the way out so it never waits out its bound.
    let _release = ReleaseOnDrop(release_tx);
    let (_answer_tx, answer_rx) = mpsc::channel::<()>();

    advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
}

#[test]
#[should_panic(expected = "watched park")]
fn a_park_without_an_instant_still_panics() {
    let clock = Arc::new(FakeClock::new());
    let kill_at = clock.now() + Duration::from_secs(30);
    let advance = Duration::from_secs(5);
    let (release_tx, release_rx) = mpsc::channel();
    let parked_clock = Arc::clone(&clock);
    thread::spawn(move || {
        parked_clock.wait_until(None, &mut |_bound| {
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&release_rx)
                .expect("waited for the test to release the parked thread");
        });
    });
    assert!(
        clock.await_parked_unbounded(Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the thread to park without an instant"
    );
    // The panic unwinds past this test: release the parked thread on
    // the way out so it never waits out its bound.
    let _release = ReleaseOnDrop(release_tx);
    let (_answer_tx, answer_rx) = mpsc::channel::<()>();

    advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
}
