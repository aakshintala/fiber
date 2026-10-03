use std::sync::mpsc;
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use contract::clock::{Clock, Wake};

use super::FakeClock;

fn wall_epoch() -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

#[test]
fn a_new_clock_starts_at_its_origin_and_the_fixed_epoch() {
    let clock = FakeClock::new();
    assert_eq!(clock.now(), clock.origin());
    assert_eq!(clock.wall(), wall_epoch());
    assert!(clock.parked().is_empty());
}

#[test]
fn advance_moves_now_and_wall_by_the_same_duration() {
    let clock = FakeClock::new();
    clock.advance(Duration::from_millis(5));
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    assert_eq!(clock.wall(), wall_epoch() + Duration::from_millis(5));
}

#[test]
fn sleep_advances_by_the_duration_and_returns() {
    let clock = FakeClock::new();
    // An hour: a sleep that blocked on real time would not return.
    clock.sleep(Duration::from_secs(3600));
    assert_eq!(clock.now(), clock.origin() + Duration::from_secs(3600));
    assert_eq!(clock.wall(), wall_epoch() + Duration::from_secs(3600));
}

#[test]
fn wait_until_at_or_before_now_hands_zero_and_does_not_park() {
    let clock = FakeClock::new();
    let mut seen = None;
    clock.wait_until(Some(clock.origin()), &mut |bound| seen = Some(bound));
    assert_eq!(seen, Some(Some(Duration::ZERO)));
    let earlier = clock.origin().checked_sub(Duration::from_secs(1)).unwrap();
    clock.wait_until(Some(earlier), &mut |bound| seen = Some(bound));
    assert_eq!(seen, Some(Some(Duration::ZERO)));
    assert!(clock.parked().is_empty());
}

/// Stands in for the hub: `wake` takes the mutex before it notifies, the
/// same order the hub's waiters depend on.
struct StandIn {
    mutex: Mutex<()>,
    cv: Condvar,
    clock: Mutex<Option<Arc<FakeClock>>>,
}

impl Wake for StandIn {
    fn wake(&self) {
        // Touch the clock while waking. This deadlocks if `advance` still
        // holds the clock's lock.
        if let Some(clock) = self.clock.lock().unwrap().as_ref() {
            let _ = clock.now();
        }
        let _guard = self.mutex.lock().unwrap();
        self.cv.notify_all();
    }
}

#[test]
fn wait_until_parks_and_advance_wakes_it() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(10);
    let barrier = Arc::new(Barrier::new(2));
    let stand = Arc::new(StandIn {
        mutex: Mutex::new(()),
        cv: Condvar::new(),
        clock: Mutex::new(Some(Arc::clone(&clock))),
    });
    let cloned = Arc::clone(&stand);
    let wake: Arc<dyn Wake> = cloned;
    clock.subscribe(Arc::downgrade(&wake));
    let (tx, rx) = mpsc::channel();
    let clock_t = Arc::clone(&clock);
    let barrier_t = Arc::clone(&barrier);
    let stand_t = Arc::clone(&stand);
    thread::spawn(move || {
        let guard = stand_t.mutex.lock().unwrap();
        barrier_t.wait();
        let mut guard = Some(guard);
        clock_t.wait_until(Some(until), &mut |bound| {
            assert_eq!(bound, None);
            tx.send("parked").unwrap();
            let held = guard.take().unwrap();
            let held = stand_t.cv.wait(held).unwrap();
            guard = Some(held);
        });
        tx.send("woke").unwrap();
    });
    barrier.wait();
    assert!(
        clock.await_parked(until, Duration::from_secs(2)),
        "waited for the thread to park at its deadline"
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2))
            .expect("waited for the thread to report it had parked"),
        "parked"
    );
    // The thread still holds the stand-in mutex. `advance` wakes it only
    // after `wait_until` has registered the park and the closure is in
    // `Condvar::wait`.
    clock.advance(Duration::from_secs(10));
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2))
            .expect("waited for advance to wake the parked thread"),
        "woke"
    );
    assert!(clock.parked().is_empty());
}

#[test]
fn await_parked_returns_once_a_thread_parks_at_the_deadline() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let barrier = Arc::new(Barrier::new(2));
    let (release_tx, release_rx) = mpsc::channel();
    let clock_t = Arc::clone(&clock);
    let barrier_t = Arc::clone(&barrier);
    let handle = thread::spawn(move || {
        barrier_t.wait();
        clock_t.wait_until(Some(until), &mut |bound| {
            assert_eq!(bound, None);
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("waited for the test to release the parked thread");
        });
    });
    barrier.wait();
    assert!(
        clock.await_parked(until, Duration::from_secs(2)),
        "waited for a thread to park at the deadline"
    );
    assert_eq!(clock.parked(), vec![Some(until)]);
    release_tx.send(()).unwrap();
    handle.join().unwrap();
    assert!(clock.parked().is_empty());
}

/// A thread is already parked. `await_parked` on a helper must answer at once.
/// Deleting the `!` in its wait would block for the 30 s `within`, and this
/// test's 5 s wait would fail.
#[test]
fn await_parked_returns_at_once_when_a_thread_is_already_parked() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (release_tx, release_rx) = mpsc::channel();
    let clock_t = Arc::clone(&clock);
    let parked = thread::spawn(move || {
        clock_t.wait_until(Some(until), &mut |_bound| {
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("waited for the test to release the parked thread");
        });
    });
    assert!(
        clock.await_parked(until, Duration::from_secs(5)),
        "waited for a thread to park at the deadline"
    );
    let clock_t = Arc::clone(&clock);
    let (tx, rx) = mpsc::channel();
    thread::spawn(
        move || match tx.send(clock_t.await_parked(until, Duration::from_secs(30))) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        },
    );
    assert!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("waited for await_parked to see the thread already parked")
    );
    release_tx.send(()).unwrap();
    parked.join().unwrap();
}

/// `await_parked_count` on a helper. A mutant that waits out its 30 s `within`
/// fails this 5 s wait.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn await_count(clock: &Arc<FakeClock>, until: std::time::Instant, count: usize) -> bool {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    thread::spawn(move || {
        match tx.send(clock.await_parked_count(until, count, Duration::from_secs(30))) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("waited for await_parked_count")
}

/// Parks `n` threads at `until`. Each reports from inside `wait_until`, which
/// is past the park, then waits at most 5 s to be released. The returned
/// lock is `false` until the test sets it and notifies.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn park_n(
    clock: &Arc<FakeClock>,
    until: std::time::Instant,
    n: usize,
) -> Arc<(Mutex<bool>, Condvar)> {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (parked_tx, parked_rx) = mpsc::channel();
    for _ in 0..n {
        let clock = Arc::clone(clock);
        let release = Arc::clone(&release);
        let parked_tx = parked_tx.clone();
        thread::spawn(move || {
            clock.wait_until(Some(until), &mut |_bound| {
                match parked_tx.send(()) {
                    Ok(()) | Err(mpsc::SendError(())) => {}
                }
                let (lock, cv) = &*release;
                let guard = lock.lock().unwrap();
                drop(cv.wait_timeout_while(guard, Duration::from_secs(5), |go| !*go));
            });
        });
    }
    for _ in 0..n {
        parked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("waited for a thread to park");
    }
    release
}

#[test]
fn await_parked_count_is_false_before_any_thread_parks() {
    let clock = FakeClock::new();
    assert!(!clock.await_parked_count(clock.origin() + Duration::from_secs(5), 1, Duration::ZERO));
}

#[test]
fn await_parked_count_is_true_when_the_count_is_already_parked() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let release = park_n(&clock, until, 1);
    assert!(await_count(&clock, until, 1));
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
}

#[test]
fn await_parked_count_is_true_when_more_than_the_count_are_parked() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let release = park_n(&clock, until, 2);
    assert!(await_count(&clock, until, 1));
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
}

#[test]
fn await_parked_is_false_when_nobody_parks() {
    let clock = FakeClock::new();
    assert!(!clock.await_parked(
        clock.origin() + Duration::from_secs(5),
        Duration::from_millis(30)
    ));
}

#[test]
fn advance_skips_a_waker_whose_owner_was_dropped() {
    let clock = FakeClock::new();
    let stand = Arc::new(StandIn {
        mutex: Mutex::new(()),
        cv: Condvar::new(),
        clock: Mutex::new(None),
    });
    let wake: Arc<dyn Wake> = stand;
    clock.subscribe(Arc::downgrade(&wake));
    drop(wake);
    clock.advance(Duration::from_secs(1));
    assert_eq!(clock.now(), clock.origin() + Duration::from_secs(1));
}

#[test]
fn two_advances_both_move_the_clock() {
    let clock = FakeClock::new();
    let barrier = Arc::new(Barrier::new(3));
    let spawn = |clock: Arc<FakeClock>, barrier: Arc<Barrier>| {
        thread::spawn(move || {
            barrier.wait();
            clock.advance(Duration::from_millis(1));
        })
    };
    let first = spawn(Arc::clone(&clock), Arc::clone(&barrier));
    let second = spawn(Arc::clone(&clock), Arc::clone(&barrier));
    barrier.wait();
    first.join().unwrap();
    second.join().unwrap();
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(2));
}
