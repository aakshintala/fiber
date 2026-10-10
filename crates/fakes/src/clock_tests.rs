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
    let (exit_tx, exit_rx) = mpsc::channel();
    let clock_t = Arc::clone(&clock);
    let barrier_t = Arc::clone(&barrier);
    thread::spawn(move || {
        barrier_t.wait();
        clock_t.wait_until(Some(until), &mut |bound| {
            assert_eq!(bound, None);
            release_rx
                .recv()
                .expect("waited for the test to release the parked thread");
        });
        if let Ok(()) = exit_tx.send(()) {}
    });
    barrier.wait();
    assert!(
        clock.await_parked(until, Duration::from_secs(2)),
        "waited for a thread to park at the deadline"
    );
    assert_eq!(clock.parked(), vec![Some(until)]);
    release_tx.send(()).unwrap();
    exit_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
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
    let (exit_tx, exit_rx) = mpsc::channel();
    let clock_t = Arc::clone(&clock);
    thread::spawn(move || {
        clock_t.wait_until(Some(until), &mut |_bound| {
            release_rx
                .recv()
                .expect("waited for the test to release the parked thread");
        });
        if let Ok(()) = exit_tx.send(()) {}
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
    exit_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
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
    until: Option<std::time::Instant>,
    n: usize,
) -> Arc<(Mutex<bool>, Condvar)> {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (parked_tx, parked_rx) = mpsc::channel();
    for _ in 0..n {
        let clock = Arc::clone(clock);
        let release = Arc::clone(&release);
        let parked_tx = parked_tx.clone();
        thread::spawn(move || {
            clock.wait_until(until, &mut |_bound| {
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
    let release = park_n(&clock, Some(until), 1);
    assert!(await_count(&clock, until, 1));
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
}

#[test]
fn await_parked_count_is_true_when_more_than_the_count_are_parked() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let release = park_n(&clock, Some(until), 2);
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
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            barrier.wait();
            clock.advance(Duration::from_millis(1));
            if let Ok(()) = tx.send(()) {}
        });
        rx
    };
    let first = spawn(Arc::clone(&clock), Arc::clone(&barrier));
    let second = spawn(Arc::clone(&clock), Arc::clone(&barrier));
    barrier.wait();
    first
        .recv_timeout(Duration::from_secs(2))
        .expect("the first advance returns");
    second
        .recv_timeout(Duration::from_secs(2))
        .expect("the second advance returns");
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(2));
}

/// An await expected true runs on a helper thread with a 5 s `within`,
/// so a wait that never matches fails the test instead of blocking it.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn await_since(
    clock: &Arc<FakeClock>,
    mark: &super::Mark,
    until: Option<std::time::Instant>,
) -> bool {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    let mark = mark.clone();
    thread::spawn(move || {
        match tx.send(clock.await_parked_since(&mark, until, Duration::from_secs(5))) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("waited for await_parked_since")
}

#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn await_unbounded(clock: &Arc<FakeClock>) -> bool {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    thread::spawn(
        move || match tx.send(clock.await_parked_unbounded(Duration::from_secs(5))) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        },
    );
    rx.recv_timeout(Duration::from_secs(5))
        .expect("waited for await_parked_unbounded")
}

/// Parks one thread at `until`; it reports from inside `wait_until`, then
/// waits at most 5 s to be released. Returns the parked thread's id with
/// the report and the release.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn park_once(
    clock: &Arc<FakeClock>,
    until: Option<std::time::Instant>,
) -> (
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
    std::thread::ThreadId,
) {
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (exit_tx, exit_rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    let handle = thread::spawn(move || {
        clock.wait_until(until, &mut |_bound| {
            match parked_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for the test to release the parked thread");
        });
        if let Ok(()) = exit_tx.send(()) {}
    });
    (parked_rx, release_tx, exit_rx, handle.thread().id())
}

#[test]
fn a_park_present_at_the_advance_does_not_match() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    parked
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    assert!(
        clock.await_parked(until, Duration::from_secs(5)),
        "waited for the thread to park at the deadline"
    );
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        !clock.await_parked_since(&mark, Some(until), Duration::ZERO),
        "the park present at the advance is not a later park"
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

#[test]
fn a_later_park_at_any_deadline_by_a_thread_in_the_mark_matches() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let later = clock.origin() + Duration::from_secs(60);
    let (first_tx, first_rx) = mpsc::channel();
    let (second_tx, second_rx) = mpsc::channel();
    let (third_tx, third_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (exit_tx, exit_rx) = mpsc::channel();
    let clock_t = Arc::clone(&clock);
    thread::spawn(move || {
        clock_t.wait_until(Some(until), &mut |_bound| {
            match first_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the first park");
        });
        clock_t.wait_until(Some(until), &mut |_bound| {
            match second_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the second park");
        });
        clock_t.wait_until(Some(later), &mut |_bound| {
            match third_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the third park");
        });
        if let Ok(()) = exit_tx.send(()) {}
    });
    first_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the first park");
    release_tx.send(()).unwrap();
    second_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the second park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (awaited_tx, awaited_rx) = mpsc::channel();
    let clock_wait = Arc::clone(&clock);
    let mark_wait = mark.clone();
    thread::spawn(move || {
        let result = clock_wait.await_any_parked_since(&mark_wait, Duration::from_secs(5));
        if let Ok(()) = awaited_tx.send(result) {}
    });
    release_tx.send(()).unwrap();
    third_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the third park");
    assert!(
        awaited_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("waited for a later park at any deadline"),
        "the third park is later than the mark despite its different deadline"
    );
    release_tx.send(()).unwrap();
    exit_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

#[test]
fn a_park_by_a_thread_absent_from_the_mark_does_not_match() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    parked_a
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the first thread to park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    parked_b
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the second thread to park");
    assert!(
        !clock.await_parked_since(&mark, Some(until), Duration::from_millis(30)),
        "a park by a thread absent from the mark is not a later park"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    exit_a
        .recv_timeout(Duration::from_secs(5))
        .expect("the first thread exits");
    exit_b
        .recv_timeout(Duration::from_secs(5))
        .expect("the second thread exits");
}

#[test]
fn only_the_park_with_the_awaited_until_matches() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let later = clock.origin() + Duration::from_secs(60);
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (exit_tx, exit_rx) = mpsc::channel();
    let clock_t = Arc::clone(&clock);
    thread::spawn(move || {
        clock_t.wait_until(Some(until), &mut |_bound| {
            match parked_tx.send("first") {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the first park");
        });
        clock_t.wait_until(Some(later), &mut |_bound| {
            match parked_tx.send("second") {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the second park");
        });
        clock_t.wait_until(None, &mut |_bound| {
            match parked_tx.send("third") {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the third park");
        });
        if let Ok(()) = exit_tx.send(()) {}
    });
    assert_eq!(
        parked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("waited for the first park"),
        "first"
    );
    let mark = clock.advance_marked(Duration::from_millis(1));
    release_tx.send(()).unwrap();
    assert_eq!(
        parked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("waited for the second park"),
        "second"
    );
    assert!(
        !clock.await_parked_since(&mark, Some(until), Duration::ZERO),
        "the re-park is at another deadline"
    );
    assert!(
        !clock.await_parked_since(&mark, None, Duration::ZERO),
        "the re-park has a deadline"
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        parked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("waited for the third park"),
        "third"
    );
    assert!(
        await_since(&clock, &mark, None),
        "the re-park without a deadline matches"
    );
    release_tx.send(()).unwrap();
    exit_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

struct ChanWake(mpsc::Sender<()>);

impl Wake for ChanWake {
    fn wake(&self) {
        match self.0.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
    }
}

#[test]
fn advance_marked_moves_the_clock_and_wakes_subscribers() {
    let clock = FakeClock::new();
    let (tx, rx) = mpsc::channel();
    let wake: Arc<dyn Wake> = Arc::new(ChanWake(tx));
    clock.subscribe(Arc::downgrade(&wake));
    clock.advance_marked(Duration::from_millis(5));
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    assert_eq!(clock.wall(), wall_epoch() + Duration::from_millis(5));
    rx.recv_timeout(Duration::from_secs(2))
        .expect("advance_marked wakes subscribers");
    drop(wake);
}

#[test]
fn await_parked_unbounded_matches_only_a_park_without_a_deadline() {
    let clock = FakeClock::new();
    assert!(
        !clock.await_parked_unbounded(Duration::from_millis(30)),
        "nobody is parked"
    );
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    parked
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    assert!(
        !clock.await_parked_unbounded(Duration::ZERO),
        "the only park has a deadline"
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
    let (parked_none, release_none, exit_none, _) = park_once(&clock, None);
    parked_none
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park without a deadline");
    assert!(await_unbounded(&clock), "a park without a deadline matches");
    release_none.send(()).unwrap();
    exit_none
        .recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

#[test]
fn mark_parked_marks_only_the_threads_parked_at_until() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let later = clock.origin() + Duration::from_secs(60);
    let (parked_first, release_first, exit_first, _) = park_once(&clock, Some(until));
    parked_first
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the first thread to park");
    let (parked_second, release_second, exit_second, _) = park_once(&clock, Some(later));
    parked_second
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the second thread to park");
    let mark = clock
        .mark_parked(until, Duration::from_secs(5))
        .expect("waited for a thread to park at the deadline");
    assert_eq!(
        mark.0.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
        vec![0]
    );
    release_first.send(()).unwrap();
    release_second.send(()).unwrap();
    exit_first
        .recv_timeout(Duration::from_secs(5))
        .expect("the first thread exits");
    exit_second
        .recv_timeout(Duration::from_secs(5))
        .expect("the second thread exits");
}

#[test]
fn mark_parked_is_none_when_nobody_parks() {
    let clock = FakeClock::new();
    assert!(
        clock
            .mark_parked(
                clock.origin() + Duration::from_secs(5),
                Duration::from_millis(30)
            )
            .is_none()
    );
}

#[test]
fn a_later_park_after_mark_parked_matches() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (first_tx, first_rx) = mpsc::channel();
    let (second_tx, second_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (exit_tx, exit_rx) = mpsc::channel();
    let clock_t = Arc::clone(&clock);
    thread::spawn(move || {
        clock_t.wait_until(Some(until), &mut |_bound| {
            match first_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the first park");
        });
        clock_t.wait_until(Some(until), &mut |_bound| {
            match second_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the second park");
        });
        if let Ok(()) = exit_tx.send(()) {}
    });
    first_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the first park");
    let mark = clock
        .mark_parked(until, Duration::from_secs(5))
        .expect("waited for the thread to park at the deadline");
    release_tx.send(()).unwrap();
    second_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the second park");
    assert!(
        await_since(&clock, &mark, Some(until)),
        "the second park is later than the mark"
    );
    release_tx.send(()).unwrap();
    exit_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

/// Parks one thread twice at `until`: reports each park from inside
/// the wait, leaves the first on release, reports left, waits for the
/// gate, parks again, exits. The test's own waits bound the run at 5 s wall-clock.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
type Twice = (
    mpsc::Receiver<()>,
    mpsc::Receiver<()>,
    mpsc::Receiver<()>,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Sender<()>,
);
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn park_twice(clock: &Arc<FakeClock>, until: std::time::Instant) -> Twice {
    let (first_tx, first_rx) = mpsc::channel();
    let (left_tx, left_rx) = mpsc::channel();
    let (second_tx, second_rx) = mpsc::channel();
    let (exit_tx, exit_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let clock = Arc::clone(clock);
    thread::spawn(move || {
        clock.wait_until(Some(until), &mut |_bound| {
            match first_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the first park");
        });
        match left_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        gate_rx
            .recv()
            .expect("waited for the test to open the gate");
        clock.wait_until(Some(until), &mut |_bound| {
            match second_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the second park");
        });
        if let Ok(()) = exit_tx.send(()) {}
    });
    (first_rx, left_rx, second_rx, exit_rx, release_tx, gate_tx)
}

#[test]
fn a_thread_that_left_its_park_before_the_advance_matches_its_next_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (first, left, _second, exit, release, gate) = park_twice(&clock, until);
    first
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    left.recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to leave its park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    gate.send(()).unwrap();
    assert!(
        await_since(&clock, &mark, Some(until)),
        "the park after the advance is later than the mark"
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

#[test]
fn a_re_park_before_the_advance_does_not_match() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (first, left, second, exit, release, gate) = park_twice(&clock, until);
    first
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    left.recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to leave its park");
    gate.send(()).unwrap();
    second
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park again");
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        !clock.await_parked_since(&mark, Some(until), Duration::ZERO),
        "the park present at the advance is not a later park"
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

/// An await for any later park on a helper thread with a 5 s `within`,
/// so a wait that never matches fails the test instead of blocking it.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn await_any(clock: &Arc<FakeClock>, mark: &super::Mark) -> bool {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    let mark = mark.clone();
    thread::spawn(move || {
        match tx.send(clock.await_any_parked_since(&mark, Duration::from_secs(5))) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("waited for await_any_parked_since")
}

/// `await_any_parked_since` times out false when no thread parks again: a
/// body replaced with `true` (a surviving mutant) answers true at once.
#[test]
fn await_any_parked_since_is_false_when_no_thread_parks_again() {
    let clock = FakeClock::new();
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        !clock.await_any_parked_since(&mark, Duration::from_millis(30)),
        "no thread parked again after the mark"
    );
}

/// A thread that left its park before the advance matches its next park:
/// without the `!` (a surviving mutant) the wait ends at once with false.
/// The waiter reports its empty-predicate check and pauses; the test checks
/// that no result arrived before it opens the re-park gate.
#[test]
fn await_any_parked_since_matches_a_next_park_after_a_leave() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (first, left, second, exit, release, gate) = park_twice(&clock, until);
    first
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    left.recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to leave its park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (awaited_tx, awaited_rx) = mpsc::channel();
    let (checked, release_check) = clock.pause_next_await_any_predicate();
    let clock_wait = Arc::clone(&clock);
    let mark_wait = mark.clone();
    thread::spawn(move || {
        let result = clock_wait.await_any_parked_since(&mark_wait, Duration::from_secs(5));
        if let Ok(()) = awaited_tx.send(result) {}
    });
    checked
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for await_any to check the predicate");
    release_check.send(()).unwrap();
    let before_repark = awaited_rx.recv_timeout(Duration::from_millis(200));
    assert!(
        matches!(before_repark, Err(mpsc::RecvTimeoutError::Timeout)),
        "await_any remains unanswered while the re-park is gated"
    );
    gate.send(()).unwrap();
    second
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park again");
    let later = if matches!(before_repark, Err(mpsc::RecvTimeoutError::Timeout)) {
        awaited_rx.recv_timeout(Duration::from_secs(5))
    } else {
        Err(mpsc::RecvTimeoutError::Disconnected)
    };
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
    assert!(
        matches!(later, Ok(true)),
        "the park after the advance is later than the mark"
    );
}

/// A park carrying the mark's own id is the same park, not a later one;
/// the re-park past it is. `>=` or `||` (surviving mutants) match the first.
#[test]
fn await_any_parked_since_needs_an_id_past_the_mark() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (first, left, second, exit, release, gate) = park_twice(&clock, until);
    first
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        !clock.await_any_parked_since(&mark, Duration::from_millis(30)),
        "the park present at the advance is not a later park"
    );
    release.send(()).unwrap();
    left.recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to leave its park");
    gate.send(()).unwrap();
    second
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park again");
    assert!(
        await_any(&clock, &mark),
        "the re-park past the mark is a later park"
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

/// A park by a thread absent from the mark is not a later park, however new
/// its id is. `||` (a surviving mutant) matches it through its id alone.
#[test]
fn await_any_parked_since_ignores_a_thread_absent_from_the_mark() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a) = park_once(&clock, Some(until));
    parked_a
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the first thread to park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (parked_b, release_b, exit_b) = park_once(&clock, Some(until));
    parked_b
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the second thread to park");
    assert!(
        !clock.await_any_parked_since(&mark, Duration::from_millis(30)),
        "a park by a thread absent from the mark is not a later park"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    exit_a
        .recv_timeout(Duration::from_secs(5))
        .expect("the first thread exits");
    exit_b
        .recv_timeout(Duration::from_secs(5))
        .expect("the second thread exits");
}

#[test]
fn a_thread_parking_after_the_advance_is_absent_from_the_mark() {
    let clock = FakeClock::new();
    let (parked_a, release_a, exit_a, _) = park_once(&clock, None);
    parked_a
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for thread A to park");
    assert!(
        clock.await_parked_unbounded(Duration::from_secs(5)),
        "thread A is parked unbounded"
    );
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, None);
    parked_b
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for thread B to park");
    assert!(
        clock.await_thread_parked(b_id, None, Duration::from_secs(5)),
        "thread B is parked unbounded: {:?}, thread {b_id:?}",
        clock.parked(),
    );
    assert!(
        !clock.await_parked_since(&mark, None, Duration::ZERO),
        "thread B parked after the advance, so it is absent from the mark"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    exit_a
        .recv_timeout(Duration::from_secs(5))
        .expect("thread A exits");
    exit_b
        .recv_timeout(Duration::from_secs(5))
        .expect("thread B exits");
}

#[test]
fn await_thread_parked_is_false_for_a_thread_that_never_parked() {
    let clock = FakeClock::new();
    let (parked, release, exit, _) = park_once(&clock, None);
    parked
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    let idle = thread::current().id();
    assert!(
        !clock.await_thread_parked(idle, None, Duration::ZERO),
        "a thread that never parked is not parked: {:?}, thread {idle:?}",
        clock.parked(),
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

#[test]
fn await_thread_parked_is_false_for_the_wrong_until() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = clock.origin() + Duration::from_secs(60);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    parked
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    assert!(
        !clock.await_thread_parked(id, Some(wrong), Duration::ZERO),
        "the thread parked at another deadline: {:?}, thread {id:?}",
        clock.parked(),
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

#[test]
fn await_thread_parked_is_true_for_the_matching_until() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    parked
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    assert!(
        clock.await_thread_parked(id, Some(until), Duration::from_secs(5)),
        "the thread parked at its deadline: {:?}, thread {id:?}",
        clock.parked(),
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
}

#[test]
fn await_thread_parked_is_false_after_the_thread_leaves_its_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    parked
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("the parked thread exits");
    assert!(
        !clock.await_thread_parked(id, Some(until), Duration::ZERO),
        "the thread left its park: {:?}, thread {id:?}",
        clock.parked(),
    );
}

/// Parks one thread twice with no deadline: reports each park from inside
/// the wait, leaves the first on release, reports left, waits for the
/// gate, parks again, exits. Returns the parked thread's id with the
/// reports, the release and the gate. The test's own waits bound the run
/// at 5 s wall-clock.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
type TwiceUnbounded = (
    mpsc::Receiver<()>,
    mpsc::Receiver<()>,
    mpsc::Receiver<()>,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Sender<()>,
    std::thread::ThreadId,
);
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn park_twice_unbounded(clock: &Arc<FakeClock>) -> TwiceUnbounded {
    let (first_tx, first_rx) = mpsc::channel();
    let (left_tx, left_rx) = mpsc::channel();
    let (second_tx, second_rx) = mpsc::channel();
    let (exit_tx, exit_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let clock = Arc::clone(clock);
    let handle = thread::spawn(move || {
        clock.wait_until(None, &mut |_bound| {
            match first_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the first park");
        });
        match left_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        gate_rx
            .recv()
            .expect("waited for the test to open the gate");
        clock.wait_until(None, &mut |_bound| {
            match second_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            release_rx
                .recv()
                .expect("waited for release of the second park");
        });
        if let Ok(()) = exit_tx.send(()) {}
    });
    (
        first_rx,
        left_rx,
        second_rx,
        exit_rx,
        release_tx,
        gate_tx,
        handle.thread().id(),
    )
}

#[test]
fn a_thread_waited_on_by_id_is_in_the_mark_for_its_next_park() {
    let clock = FakeClock::new();
    let (first, left, second, exit, release, gate, b_id) = park_twice_unbounded(&clock);
    first
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for thread B to park");
    assert!(
        clock.await_thread_parked(b_id, None, Duration::from_secs(5)),
        "thread B is parked unbounded: {:?}, thread {b_id:?}",
        clock.parked(),
    );
    let mark = clock.advance_marked(Duration::from_millis(1));
    release.send(()).unwrap();
    left.recv_timeout(Duration::from_secs(5))
        .expect("waited for thread B to leave its park");
    gate.send(()).unwrap();
    second
        .recv_timeout(Duration::from_secs(5))
        .expect("waited for thread B to park again");
    assert!(
        clock.await_parked_since(&mark, None, Duration::from_secs(5)),
        "thread B is in the mark, so its next park matches: {:?}, thread {b_id:?}",
        clock.parked(),
    );
    release.send(()).unwrap();
    exit.recv_timeout(Duration::from_secs(5))
        .expect("thread B exits");
}
