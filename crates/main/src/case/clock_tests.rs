//! The case clock's manual-time contract (`docs/testing.md`, "Waits and timeouts").

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};

use super::CaseClock;

const WAIT: Duration = Duration::from_secs(5);
const NO_MATCH: Duration = Duration::from_millis(30);

type ParkEvent = (Option<Instant>, Option<Duration>);
type Waiter = (mpsc::Receiver<ParkEvent>, mpsc::Receiver<Instant>);

#[derive(Default)]
struct WakeSignal {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl WakeSignal {
    fn generation(&self) -> u64 {
        *lock(&self.generation)
    }
}

impl Wake for WakeSignal {
    fn wake(&self) {
        let mut generation = lock(&self.generation);
        *generation = generation.wrapping_add(1);
        drop(generation);
        self.changed.notify_all();
    }
}

#[derive(Default)]
struct CountWake(AtomicUsize);

impl Wake for CountWake {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn subscribe(clock: &CaseClock, wake: &Arc<dyn Wake>) {
    clock.subscribe(Arc::downgrade(wake));
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Parks a thread until `until` passes on the case clock and reports each
/// clock wait after the clock has registered it.
fn waiter<'scope, 'env: 'scope>(
    scope: &'scope thread::Scope<'scope, 'env>,
    clock: Arc<CaseClock>,
    until: Instant,
) -> Waiter {
    let signal = Arc::new(WakeSignal::default());
    let wake: Arc<dyn Wake> = signal.clone();
    subscribe(&clock, &wake);
    let (parked_tx, parked_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    scope.spawn(move || {
        loop {
            let mut observed = signal.generation();
            let mut timed_out = false;
            let signal_for_wait = Arc::clone(&signal);
            let parked = parked_tx.clone();
            let mut wait = |bound| {
                let _sent = parked.send((Some(until), bound));
                let state = lock(&signal_for_wait.generation);
                let (state, timeout) = signal_for_wait
                    .changed
                    .wait_timeout_while(state, WAIT, |generation| *generation == observed)
                    .unwrap_or_else(PoisonError::into_inner);
                observed = *state;
                timed_out = timeout.timed_out();
            };
            clock.wait_until(Some(until), &mut wait);
            if clock.now() >= until {
                break;
            }
            if timed_out {
                break;
            }
        }
        let _sent = done_tx.send(clock.now());
    });
    (parked_rx, done_rx)
}

#[test]
fn now_and_wall_move_only_when_the_case_advances() {
    let clock = CaseClock::new();
    let now = clock.now();
    let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(clock.now(), now);
    assert_eq!(clock.wall(), wall);

    let advance = Duration::from_millis(25);
    clock.advance(advance);
    assert_eq!(clock.now(), now + advance);
    assert_eq!(clock.wall(), wall + advance);
}

#[test]
fn wall_starts_at_the_case_epoch() {
    let clock = CaseClock::new();
    assert_eq!(
        clock.wall(),
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    );
}

#[test]
fn a_past_wait_returns_zero_and_does_not_park() {
    let clock = CaseClock::new();
    let past = clock.now();
    clock.advance(Duration::from_millis(1));
    let mut bound = None;
    clock.wait_until(Some(past), &mut |next| bound = next);
    assert_eq!(bound, Some(Duration::ZERO));
    assert!(clock.parked().is_empty());
}

#[test]
fn a_wait_at_now_returns_zero_and_does_not_park() {
    let clock = CaseClock::new();
    let now = clock.now();
    let mut bound = None;
    clock.wait_until(Some(now), &mut |next| bound = next);
    assert_eq!(bound, Some(Duration::ZERO));
    assert!(clock.parked().is_empty());
}

#[test]
fn a_future_wait_is_visible_and_released_by_an_advance() {
    let clock = CaseClock::new();
    let until = clock.now() + Duration::from_secs(2);
    thread::scope(|scope| {
        let (parked, done) = waiter(scope, Arc::clone(&clock), until);
        assert_eq!(
            parked
                .recv_timeout(WAIT)
                .expect("waited for the clock park"),
            (Some(until), None)
        );
        assert!(clock.parked().contains(&Some(until)));

        clock.advance(Duration::from_secs(3));
        assert_eq!(
            done.recv_timeout(WAIT).expect("waited for the clock wake"),
            clock.now()
        );
        assert!(clock.parked().is_empty());
    });
}

#[test]
fn subscribed_wakers_fire_on_each_move_and_dead_wakers_are_dropped() {
    let clock = CaseClock::new();
    let counted = Arc::new(CountWake::default());
    let wake: Arc<dyn Wake> = counted.clone();
    subscribe(&clock, &wake);
    drop(wake);

    clock.advance(Duration::from_millis(1));
    assert_eq!(counted.0.load(Ordering::SeqCst), 1);
    clock.advance(Duration::from_millis(1));
    assert_eq!(counted.0.load(Ordering::SeqCst), 2);

    drop(counted);
    clock.advance(Duration::from_millis(1));
    assert!(lock(&clock.state).wakers.is_empty());
}

#[test]
fn sleep_parks_until_its_deadline_is_advanced() {
    let clock = CaseClock::new();
    let before = clock.now();
    let delay = Duration::from_millis(25);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let sleeping = Arc::clone(&clock);
    thread::spawn(move || {
        let _sent = started_tx.send(());
        sleeping.sleep(delay);
        let _sent = done_tx.send(sleeping.now());
    });
    started_rx
        .recv_timeout(WAIT)
        .expect("waited for sleep to start");

    assert_eq!(clock.advance_when_parked(delay, WAIT), Ok(()));
    assert_eq!(
        done_rx
            .recv_timeout(WAIT)
            .expect("waited for sleep to finish"),
        before + delay
    );
}

#[test]
fn an_event_advance_fast_fails_when_it_would_skip_a_parked_wait() {
    let clock = CaseClock::new();
    let before = clock.now();
    let deadline = before + Duration::from_millis(200);
    let mut refusal = None;
    let mut wait = |_| {
        refusal = Some(clock.advance_when_parked_after_event(Duration::from_millis(400), WAIT));
    };
    clock.wait_until(Some(deadline), &mut wait);

    let error = refusal
        .expect("the parked waiter was checked")
        .expect_err("a 400 ms advance must not skip the 200 ms waiter");
    assert!(
        error.contains("would skip the only parked deadline"),
        "{error}"
    );
    assert_eq!(clock.now(), before);
}

#[test]
fn only_an_exactly_parked_deadline_authorises_an_advance() {
    let clock = CaseClock::new();
    let before = clock.now();
    let delay = Duration::from_millis(10);
    let early = before + Duration::from_millis(9);
    let late = before + Duration::from_millis(11);
    thread::scope(|scope| {
        let (early_parked, early_done) = waiter(scope, Arc::clone(&clock), early);
        let (late_parked, late_done) = waiter(scope, Arc::clone(&clock), late);
        assert_eq!(
            early_parked
                .recv_timeout(WAIT)
                .expect("waited for the early deadline to park"),
            (Some(early), None)
        );
        assert_eq!(
            late_parked
                .recv_timeout(WAIT)
                .expect("waited for the late deadline to park"),
            (Some(late), None)
        );

        let result = clock.advance_when_parked(delay, NO_MATCH);
        let error = result.as_ref().err().cloned().unwrap_or_default();
        let before_cleanup = clock.now();
        let parked_before_cleanup = clock.parked();
        clock.advance(Duration::from_millis(9));
        let early_finished = early_done.recv_timeout(WAIT);
        let late_reparked = late_parked.recv_timeout(WAIT);
        let late_still_parked = clock.parked().contains(&Some(late));
        clock.advance(Duration::from_millis(2));
        let late_finished = late_done.recv_timeout(WAIT);

        assert!(
            result.is_err(),
            "nearby deadline unexpectedly advanced time"
        );
        assert_eq!(before_cleanup, before);
        assert!(parked_before_cleanup.contains(&Some(early)));
        assert!(parked_before_cleanup.contains(&Some(late)));
        assert!(error.contains(&format!("{early:?}")), "{error}");
        assert!(error.contains(&format!("{late:?}")), "{error}");
        assert_eq!(
            early_finished.expect("waited for the early deadline"),
            early
        );
        assert_eq!(
            late_reparked.expect("waited for the late waiter to re-park"),
            (Some(late), None)
        );
        assert!(late_still_parked);
        assert_eq!(late_finished.expect("waited for the late deadline"), late);
    });
}
