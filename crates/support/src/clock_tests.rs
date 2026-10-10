//! Tests for [`super`]: the process clock, the clock-parked wait and the
//! generation parker. Every wait here runs on the process clock through a
//! test-local [`TestClock`]; every result receive goes through
//! `fakes::Deadline::after(..).recv(..)`, and no test sleeps.

use std::sync::{Arc, Condvar, Mutex, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fakes::Deadline;

use super::{Clock, Parker, System, Wake, park};
use crate::lock;

/// 2020-01-01T00:00:00Z. `wall()` is after this; the test asserts no duration.
const YEAR_2020: Duration = Duration::from_secs(1_577_836_800);

#[test]
fn wall_is_after_the_2020_epoch() {
    assert!(System.wall() > UNIX_EPOCH + YEAR_2020);
}

#[test]
fn now_does_not_go_backwards() {
    let first = System.now();
    let second = System.now();
    assert!(second >= first);
}

#[test]
fn wait_until_hands_no_bound_for_no_deadline() {
    let mut seen = None;
    System.wait_until(None, &mut |bound| seen = Some(bound));
    assert_eq!(seen, Some(None));
}

#[test]
fn wait_until_hands_zero_for_a_deadline_at_or_before_now() {
    let now = System.now();
    let mut seen = None;
    System.wait_until(Some(now), &mut |bound| seen = Some(bound));
    assert_eq!(seen, Some(Some(Duration::ZERO)));
    let earlier = now.checked_sub(Duration::from_secs(1)).unwrap();
    System.wait_until(Some(earlier), &mut |bound| seen = Some(bound));
    assert_eq!(seen, Some(Some(Duration::ZERO)));
}

#[test]
fn wait_until_hands_at_most_the_span_to_a_future_deadline() {
    let until = System.now() + Duration::from_secs(1);
    let mut seen = None;
    System.wait_until(Some(until), &mut |bound| seen = Some(bound));
    let bound = seen.unwrap().unwrap();
    assert!(bound <= Duration::from_secs(1), "{bound:?}");
}

/// A clock for [`park`] and [`Parker`]: monotonic and wall time come from
/// the process clock, while `wait_until` reports "entered" past its own
/// clock check, then hands `wait` the bound the test configured (`None` or
/// `Some(ZERO)`), never blocking itself. The block under test is the
/// condvar wait `park` runs inside `wait`.
struct TestClock {
    entered: mpsc::Sender<&'static str>,
    bound: Option<Duration>,
}

impl TestClock {
    fn named(bound: Option<Duration>) -> (Self, mpsc::Receiver<&'static str>) {
        let (entered, rx) = mpsc::channel();
        (Self { entered, bound }, rx)
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        System.now()
    }

    fn wall(&self) -> SystemTime {
        System.wall()
    }

    fn sleep(&self, d: Duration) {
        System.sleep(d);
    }

    fn wait_until(&self, _until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        let _sent = self.entered.send("entered");
        wait(self.bound);
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

#[test]
fn a_done_predicate_returns_at_once_with_the_guard() {
    let (clock, entered) = TestClock::named(None);
    let mutex = Arc::new(Mutex::new(0_u64));
    let cv = Arc::new(Condvar::new());
    let (result_tx, result_rx) = mpsc::channel();
    // On a helper thread: a `done` check replaced with `false` would park
    // on the condvar with no bound and never answer.
    thread::spawn(move || {
        let guard = park(&clock, None, None, &cv, lock(&mutex), |_| true);
        let _sent = result_tx.send(guard.map(|guard| *guard));
    });
    assert_eq!(
        Deadline::after(Duration::from_secs(2)).recv(&entered),
        Ok("entered")
    );
    assert_eq!(
        Deadline::after(Duration::from_secs(5)).recv(&result_rx),
        Ok(Some(0))
    );
}

#[test]
fn a_park_that_is_not_done_blocks_until_a_notify() {
    let (clock, entered) = TestClock::named(None);
    let mutex = Arc::new(Mutex::new(0_u64));
    let cv = Arc::new(Condvar::new());
    let (result_tx, result_rx) = mpsc::channel();
    let cv_t = Arc::clone(&cv);
    let mutex_t = Arc::clone(&mutex);
    thread::spawn(move || {
        let guard = park(&clock, None, None, &cv_t, lock(&mutex_t), |state| {
            *state == 1
        });
        let _sent = result_tx.send(guard.map(|guard| *guard));
    });
    assert_eq!(
        Deadline::after(Duration::from_secs(2)).recv(&entered),
        Ok("entered")
    );
    // The park has not answered yet. A park replaced with `None` answers
    // at once, failing this fast.
    assert!(
        matches!(
            Deadline::after(Duration::from_millis(50)).recv(&result_rx),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the park is still waiting on its condvar"
    );
    // Taking the mutex blocks until the waiter is inside its condvar wait,
    // so this notify cannot be lost.
    {
        let mut state = lock(&mutex);
        *state = 1;
        cv.notify_all();
    }
    assert_eq!(
        Deadline::after(Duration::from_secs(5)).recv(&result_rx),
        Ok(Some(1))
    );
}

#[test]
fn a_zero_bound_returns_without_a_notify() {
    let (clock, _entered) = TestClock::named(Some(Duration::ZERO));
    let mutex = Arc::new(Mutex::new(0_u64));
    let cv = Arc::new(Condvar::new());
    let (result_tx, result_rx) = mpsc::channel();
    let cv_t = Arc::clone(&cv);
    let mutex_t = Arc::clone(&mutex);
    // On a helper thread: a bound that waited for a notify would never answer.
    thread::spawn(move || {
        let guard = park(&clock, None, None, &cv_t, lock(&mutex_t), |_| false);
        let _sent = result_tx.send(guard.map(|guard| *guard));
    });
    assert_eq!(
        Deadline::after(Duration::from_secs(5)).recv(&result_rx),
        Ok(Some(0)),
        "a zero bound returns its guard with no notify"
    );
}

#[test]
fn done_writes_to_the_state_are_visible_in_the_returned_guard() {
    let (clock, entered) = TestClock::named(Some(Duration::ZERO));
    let mutex = Arc::new(Mutex::new(0_u64));
    let cv = Arc::new(Condvar::new());
    let (result_tx, result_rx) = mpsc::channel();
    let cv_t = Arc::clone(&cv);
    let mutex_t = Arc::clone(&mutex);
    // On a helper thread. A `done` check replaced with `false` parks on a
    // zero bound and hands back the unwritten state, failing the assertion
    // below.
    thread::spawn(move || {
        let guard = park(&clock, None, None, &cv_t, lock(&mutex_t), |state| {
            *state = 7;
            true
        });
        let _sent = result_tx.send(guard.map(|guard| *guard));
    });
    assert_eq!(
        Deadline::after(Duration::from_secs(5)).recv(&result_rx),
        Ok(Some(7))
    );
    assert_eq!(
        Deadline::after(Duration::from_secs(2)).recv(&entered),
        Ok("entered")
    );
}

#[test]
fn bounded_hands_the_shorter_of_its_bound_and_poll() {
    let second = Duration::from_secs(1);
    let two_seconds = Duration::from_secs(2);
    let three_seconds = Duration::from_secs(3);
    let cases = [
        (None, None, None),
        (Some(three_seconds), None, Some(three_seconds)),
        (None, Some(two_seconds), Some(two_seconds)),
        (Some(second), Some(second), Some(second)),
        (Some(second), Some(two_seconds), Some(second)),
        (Some(three_seconds), Some(two_seconds), Some(two_seconds)),
    ];
    for (bound, poll, expected) in cases {
        assert_eq!(super::bounded(bound, poll), expected, "{bound:?} {poll:?}");
    }
}

#[test]
fn generation_counts_every_bump() {
    let parker = Parker::new();
    assert_eq!(parker.generation(), 0);
    parker.bump();
    parker.bump();
    assert_eq!(parker.generation(), 2);
}

#[test]
fn a_bump_between_generation_and_park_returns_at_once() {
    let (clock, entered) = TestClock::named(None);
    let parker = Parker::new();
    let seen = parker.generation();
    parker.bump();
    // On a helper thread: a `park` that waited despite the bump would never
    // answer.
    let (result_tx, result_rx) = mpsc::channel();
    thread::spawn(move || {
        parker.park(&clock, None, seen);
        let _sent = result_tx.send(());
    });
    assert_eq!(
        Deadline::after(Duration::from_secs(2)).recv(&entered),
        Ok("entered")
    );
    assert!(
        Deadline::after(Duration::from_secs(5))
            .recv(&result_rx)
            .is_ok(),
        "the bump before the park wakes it at once"
    );
}

#[test]
fn a_parked_thread_returns_after_a_bump() {
    let (clock, entered) = TestClock::named(None);
    let parker = Arc::new(Parker::new());
    let seen = parker.generation();
    let (result_tx, result_rx) = mpsc::channel();
    let parked = Arc::clone(&parker);
    thread::spawn(move || {
        parked.park(&clock, None, seen);
        let _sent = result_tx.send(());
    });
    assert_eq!(
        Deadline::after(Duration::from_secs(2)).recv(&entered),
        Ok("entered")
    );
    // The park has not answered yet. A `park` replaced with `()` answers at
    // once, failing this fast.
    assert!(
        matches!(
            Deadline::after(Duration::from_millis(50)).recv(&result_rx),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the park is still waiting on its generation"
    );
    parker.bump();
    assert!(
        Deadline::after(Duration::from_secs(5))
            .recv(&result_rx)
            .is_ok(),
        "the bump wakes the parked thread"
    );
}

#[test]
fn wake_on_a_parker_bumps_its_generation() {
    let parker = Arc::new(Parker::new());
    let wake: Arc<dyn Wake> = Arc::clone(&parker) as Arc<dyn Wake>;
    wake.wake();
    assert_eq!(parker.generation(), 1);
}
