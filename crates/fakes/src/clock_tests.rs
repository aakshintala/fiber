use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use contract::clock::{Clock, Wake};

use super::{FakeClock, Miss};
use crate::deadline::Deadline;

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

#[test]
fn wait_until_just_after_now_parks() {
    let clock = FakeClock::new();
    let until = clock.origin().checked_add(Duration::from_nanos(1)).unwrap();
    let mut observed_park = false;
    clock.wait_until(Some(until), &mut |bound| {
        assert_eq!(bound, None);
        assert_eq!(clock.parked(), vec![Some(until)]);
        observed_park = true;
    });
    assert!(observed_park, "a future instant reaches the park");
    assert!(
        clock.parked().is_empty(),
        "the returned wait leaves its park"
    );
    assert!(
        super::lock(&clock.state).observed.is_empty(),
        "a park outside an advance wait is not observed"
    );
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
        clock.await_parked(until, Deadline::after(Duration::from_secs(2))),
        "waited for the thread to park at its deadline"
    );
    assert_eq!(
        Deadline::after(Duration::from_secs(2))
            .recv(&rx)
            .expect("waited for the thread to report it had parked"),
        "parked"
    );
    // The thread still holds the stand-in mutex. `advance` wakes it only
    // after `wait_until` has registered the park and the closure is in
    // `Condvar::wait`.
    clock.advance(Duration::from_secs(10));
    assert_eq!(
        Deadline::after(Duration::from_secs(2))
            .recv(&rx)
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
        clock.await_parked(until, Deadline::after(Duration::from_secs(2))),
        "waited for a thread to park at the deadline"
    );
    assert_eq!(clock.parked(), vec![Some(until)]);
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_rx)
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
        clock.await_parked(until, Deadline::after(Duration::from_secs(5))),
        "waited for a thread to park at the deadline"
    );
    let clock_t = Arc::clone(&clock);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        match tx.send(clock_t.await_parked(until, Deadline::after(Duration::from_secs(30)))) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    assert!(
        Deadline::after(Duration::from_secs(5))
            .recv(&rx)
            .expect("waited for await_parked to see the thread already parked")
    );
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_rx)
        .expect("the parked thread exits");
}

/// `await_parked_count` on a helper. A mutant that waits out its 30 s `within`
/// fails this 5 s wait.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn await_count(clock: &Arc<FakeClock>, until: std::time::Instant, count: usize) -> bool {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    thread::spawn(move || {
        match tx.send(clock.await_parked_count(
            until,
            count,
            Deadline::after(Duration::from_secs(30)),
        )) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&rx)
        .expect("waited for await_parked_count")
}

/// Parks `n` threads at `until`. Each reports from inside `wait_until`, which
/// is past the park, then waits at most 5 s to be released. The returned
/// lock is `false` until the test sets it and notifies.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
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
    let wait = Deadline::after(Duration::from_secs(5));
    for _ in 0..n {
        wait.recv(&parked_rx).expect("waited for a thread to park");
    }
    release
}

#[test]
fn await_parked_count_is_false_before_any_thread_parks() {
    let clock = FakeClock::new();
    assert!(!clock.await_parked_count(
        clock.origin() + Duration::from_secs(5),
        1,
        Deadline::after(Duration::ZERO)
    ));
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
        Deadline::after(Duration::from_millis(30))
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
    Deadline::after(Duration::from_secs(2))
        .recv(&first)
        .expect("the first advance returns");
    Deadline::after(Duration::from_secs(2))
        .recv(&second)
        .expect("the second advance returns");
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(2));
}

/// An await expected true runs on a helper thread with a 5 s `within`,
/// so a wait that never matches fails the test instead of blocking it.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn await_since(
    clock: &Arc<FakeClock>,
    mark: &super::Mark,
    until: Option<std::time::Instant>,
) -> bool {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    let mark = mark.clone();
    thread::spawn(move || {
        match tx.send(clock.await_parked_since(
            &mark,
            until,
            Deadline::after(Duration::from_secs(5)),
        )) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&rx)
        .expect("waited for await_parked_since")
}

#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn await_unbounded(clock: &Arc<FakeClock>) -> bool {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    thread::spawn(move || {
        match tx.send(clock.await_parked_unbounded(Deadline::after(Duration::from_secs(5)))) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&rx)
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
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    assert!(
        clock.await_parked(until, Deadline::after(Duration::from_secs(5))),
        "waited for the thread to park at the deadline"
    );
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        !clock.await_parked_since(&mark, Some(until), Deadline::after(Duration::ZERO)),
        "the park present at the advance is not a later park"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
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
    Deadline::after(Duration::from_secs(5))
        .recv(&first_rx)
        .expect("waited for the first park");
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&second_rx)
        .expect("waited for the second park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (awaited_tx, awaited_rx) = mpsc::channel();
    let clock_wait = Arc::clone(&clock);
    let mark_wait = mark.clone();
    thread::spawn(move || {
        let result =
            clock_wait.await_any_parked_since(&mark_wait, Deadline::after(Duration::from_secs(5)));
        if let Ok(()) = awaited_tx.send(result) {}
    });
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&third_rx)
        .expect("waited for the third park");
    assert!(
        Deadline::after(Duration::from_secs(5))
            .recv(&awaited_rx)
            .expect("waited for a later park at any deadline"),
        "the third park is later than the mark despite its different deadline"
    );
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_rx)
        .expect("the parked thread exits");
}

#[test]
fn a_park_by_a_thread_absent_from_the_mark_does_not_match() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    assert!(
        !clock.await_parked_since(
            &mark,
            Some(until),
            Deadline::after(Duration::from_millis(30))
        ),
        "a park by a thread absent from the mark is not a later park"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
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
        Deadline::after(Duration::from_secs(5))
            .recv(&parked_rx)
            .expect("waited for the first park"),
        "first"
    );
    let mark = clock.advance_marked(Duration::from_millis(1));
    release_tx.send(()).unwrap();
    assert_eq!(
        Deadline::after(Duration::from_secs(5))
            .recv(&parked_rx)
            .expect("waited for the second park"),
        "second"
    );
    assert!(
        !clock.await_parked_since(&mark, Some(until), Deadline::after(Duration::ZERO)),
        "the re-park is at another deadline"
    );
    assert!(
        !clock.await_parked_since(&mark, None, Deadline::after(Duration::ZERO)),
        "the re-park has a deadline"
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        Deadline::after(Duration::from_secs(5))
            .recv(&parked_rx)
            .expect("waited for the third park"),
        "third"
    );
    assert!(
        await_since(&clock, &mark, None),
        "the re-park without a deadline matches"
    );
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_rx)
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
    Deadline::after(Duration::from_secs(2))
        .recv(&rx)
        .expect("advance_marked wakes subscribers");
    drop(wake);
}

#[test]
fn await_parked_unbounded_matches_only_a_park_without_a_deadline() {
    let clock = FakeClock::new();
    assert!(
        !clock.await_parked_unbounded(Deadline::after(Duration::from_millis(30))),
        "nobody is parked"
    );
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    assert!(
        !clock.await_parked_unbounded(Deadline::after(Duration::ZERO)),
        "the only park has a deadline"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
    let (parked_none, release_none, exit_none, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_none)
        .expect("waited for the thread to park without a deadline");
    assert!(await_unbounded(&clock), "a park without a deadline matches");
    release_none.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_none)
        .expect("the parked thread exits");
}

#[test]
fn mark_parked_marks_only_the_threads_parked_at_until() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let later = clock.origin() + Duration::from_secs(60);
    let (parked_first, release_first, exit_first, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_first)
        .expect("waited for the first thread to park");
    let (parked_second, release_second, exit_second, _) = park_once(&clock, Some(later));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_second)
        .expect("waited for the second thread to park");
    let mark = clock
        .mark_parked(until, Deadline::after(Duration::from_secs(5)))
        .expect("waited for a thread to park at the deadline");
    assert_eq!(
        mark.0.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
        vec![0]
    );
    release_first.send(()).unwrap();
    release_second.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_first)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_second)
        .expect("the second thread exits");
}

#[test]
fn mark_parked_is_none_when_nobody_parks() {
    let clock = FakeClock::new();
    assert!(
        clock
            .mark_parked(
                clock.origin() + Duration::from_secs(5),
                Deadline::after(Duration::from_millis(30))
            )
            .is_none()
    );
}

#[test]
fn mark_threads_returns_the_parked_threads() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let mark = clock
        .mark_parked(until, Deadline::after(Duration::from_secs(5)))
        .expect("waited for a thread to park at the deadline");
    assert_eq!(mark.threads(), vec![id]);
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
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
    Deadline::after(Duration::from_secs(5))
        .recv(&first_rx)
        .expect("waited for the first park");
    let mark = clock
        .mark_parked(until, Deadline::after(Duration::from_secs(5)))
        .expect("waited for the thread to park at the deadline");
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&second_rx)
        .expect("waited for the second park");
    assert!(
        await_since(&clock, &mark, Some(until)),
        "the second park is later than the mark"
    );
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_rx)
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
    Deadline::after(Duration::from_secs(5))
        .recv(&first)
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&left)
        .expect("waited for the thread to leave its park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    gate.send(()).unwrap();
    assert!(
        await_since(&clock, &mark, Some(until)),
        "the park after the advance is later than the mark"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn a_re_park_before_the_advance_does_not_match() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (first, left, second, exit, release, gate) = park_twice(&clock, until);
    Deadline::after(Duration::from_secs(5))
        .recv(&first)
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&left)
        .expect("waited for the thread to leave its park");
    gate.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&second)
        .expect("waited for the thread to park again");
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        !clock.await_parked_since(&mark, Some(until), Deadline::after(Duration::ZERO)),
        "the park present at the advance is not a later park"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

/// An await for any later park on a helper thread with a 5 s `within`,
/// so a wait that never matches fails the test instead of blocking it.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn await_any(clock: &Arc<FakeClock>, mark: &super::Mark) -> bool {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    let mark = mark.clone();
    thread::spawn(move || {
        match tx.send(clock.await_any_parked_since(&mark, Deadline::after(Duration::from_secs(5))))
        {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&rx)
        .expect("waited for await_any_parked_since")
}

/// `await_any_parked_since` times out false when no thread parks again: a
/// body replaced with `true` (a surviving mutant) answers true at once.
#[test]
fn await_any_parked_since_is_false_when_no_thread_parks_again() {
    let clock = FakeClock::new();
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        !clock.await_any_parked_since(&mark, Deadline::after(Duration::from_millis(30))),
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
    Deadline::after(Duration::from_secs(5))
        .recv(&first)
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&left)
        .expect("waited for the thread to leave its park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (awaited_tx, awaited_rx) = mpsc::channel();
    let (checked, release_check) = clock.pause_next_await_any_predicate();
    let clock_wait = Arc::clone(&clock);
    let mark_wait = mark.clone();
    thread::spawn(move || {
        let result =
            clock_wait.await_any_parked_since(&mark_wait, Deadline::after(Duration::from_secs(5)));
        if let Ok(()) = awaited_tx.send(result) {}
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&checked)
        .expect("waited for await_any to check the predicate");
    release_check.send(()).unwrap();
    let before_repark = Deadline::after(Duration::from_millis(200)).recv(&awaited_rx);
    assert!(
        matches!(before_repark, Err(mpsc::RecvTimeoutError::Timeout)),
        "await_any remains unanswered while the re-park is gated"
    );
    gate.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&second)
        .expect("waited for the thread to park again");
    let later = if matches!(before_repark, Err(mpsc::RecvTimeoutError::Timeout)) {
        Deadline::after(Duration::from_secs(5)).recv(&awaited_rx)
    } else {
        Err(mpsc::RecvTimeoutError::Disconnected)
    };
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
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
    Deadline::after(Duration::from_secs(5))
        .recv(&first)
        .expect("waited for the thread to park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        !clock.await_any_parked_since(&mark, Deadline::after(Duration::from_millis(30))),
        "the park present at the advance is not a later park"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&left)
        .expect("waited for the thread to leave its park");
    gate.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&second)
        .expect("waited for the thread to park again");
    assert!(
        await_any(&clock, &mark),
        "the re-park past the mark is a later park"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

/// A park by a thread absent from the mark is not a later park, however new
/// its id is. `||` (a surviving mutant) matches it through its id alone.
#[test]
fn await_any_parked_since_ignores_a_thread_absent_from_the_mark() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    assert!(
        !clock.await_any_parked_since(&mark, Deadline::after(Duration::from_millis(30))),
        "a park by a thread absent from the mark is not a later park"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn a_thread_parking_after_the_advance_is_absent_from_the_mark() {
    let clock = FakeClock::new();
    let (parked_a, release_a, exit_a, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for thread A to park");
    assert!(
        clock.await_parked_unbounded(Deadline::after(Duration::from_secs(5))),
        "thread A is parked unbounded"
    );
    let mark = clock.advance_marked(Duration::from_millis(1));
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for thread B to park");
    assert!(
        clock.await_thread_parked(b_id, None, Deadline::after(Duration::from_secs(5))),
        "thread B is parked unbounded: {:?}, thread {b_id:?}",
        clock.parked(),
    );
    assert!(
        !clock.await_parked_since(&mark, None, Deadline::after(Duration::ZERO)),
        "thread B parked after the advance, so it is absent from the mark"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("thread A exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("thread B exits");
}

#[test]
fn await_thread_parked_is_false_for_a_thread_that_never_parked() {
    let clock = FakeClock::new();
    let (parked, release, exit, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let idle = thread::current().id();
    assert!(
        !clock.await_thread_parked(idle, None, Deadline::after(Duration::ZERO)),
        "a thread that never parked is not parked: {:?}, thread {idle:?}",
        clock.parked(),
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn await_thread_parked_is_false_for_the_wrong_until() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = clock.origin() + Duration::from_secs(60);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    assert!(
        !clock.await_thread_parked(id, Some(wrong), Deadline::after(Duration::ZERO)),
        "the thread parked at another deadline: {:?}, thread {id:?}",
        clock.parked(),
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn await_thread_parked_is_true_for_the_matching_until() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    assert!(
        clock.await_thread_parked(id, Some(until), Deadline::after(Duration::from_secs(5))),
        "the thread parked at its deadline: {:?}, thread {id:?}",
        clock.parked(),
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn await_thread_parked_is_false_after_the_thread_leaves_its_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
    assert!(
        !clock.await_thread_parked(id, Some(until), Deadline::after(Duration::ZERO)),
        "the thread left its park: {:?}, thread {id:?}",
        clock.parked(),
    );
}

#[test]
fn a_thread_waited_on_by_id_is_in_the_mark_for_its_next_park() {
    let clock = FakeClock::new();
    // Thread A parks unbounded first.
    let (parked_a, release_a, exit_a, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for thread A to park");
    // Thread B blocks on the start channel before parking twice unbounded.
    let (start_tx, start_rx) = mpsc::channel::<()>();
    let (report_tx, report_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (exit_tx, exit_rx) = mpsc::channel();
    let clock_b = Arc::clone(&clock);
    let b = thread::spawn(move || {
        Deadline::after(Duration::from_secs(5))
            .recv(&start_rx)
            .expect("waited for the test to start thread B");
        for _ in 0..2 {
            clock_b.wait_until(None, &mut |_bound| {
                match report_tx.send(()) {
                    Ok(()) | Err(mpsc::SendError(())) => {}
                }
                release_rx.recv().expect("waited for release of a park");
            });
        }
        if let Ok(()) = exit_tx.send(()) {}
    });
    let b_id = b.thread().id();
    // Step 3 is the discriminator: the generic wait already sees A, so it
    // would let the step advance before B parks; only the thread wait holds
    // the advance until B is parked.
    assert!(
        clock.await_parked_unbounded(Deadline::after(Duration::ZERO)),
        "thread A is parked unbounded: {:?}",
        clock.parked(),
    );
    assert!(
        !clock.await_thread_parked(b_id, None, Deadline::after(Duration::ZERO)),
        "thread B has not parked yet: {:?}, thread {b_id:?}",
        clock.parked(),
    );
    start_tx.send(()).unwrap();
    assert!(
        clock.await_thread_parked(b_id, None, Deadline::after(Duration::from_secs(5))),
        "thread B parked unbounded: {:?}, thread {b_id:?}",
        clock.parked(),
    );
    let mark = clock.advance_marked(Duration::from_millis(1));
    release_tx.send(()).unwrap();
    let wait = Deadline::after(Duration::from_secs(5));
    wait.recv(&report_rx)
        .expect("waited for thread B to park the first time");
    wait.recv(&report_rx)
        .expect("waited for thread B to park again");
    assert!(
        clock.await_parked_since(&mark, None, Deadline::after(Duration::from_secs(5))),
        "thread B is in the mark, so its next park matches: {:?}, thread {b_id:?}",
        clock.parked(),
    );
    release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_rx)
        .expect("thread B exits");
    release_a.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("thread A exits");
}

/// `mark_parked` returns the park that arrives after it starts waiting. The
/// wait predicate signals while nobody is parked; the test releases the
/// waiter before starting the parker, so it enters the real condvar wait and
/// the arriving park wakes it. With the predicate inverted, the hook does not
/// signal and the bounded receive fails: this kills the
/// `parked_count(state, until) == 0` mutant.
#[test]
fn mark_parked_returns_the_park_that_arrives_after_it_starts() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (waiting, release_waiter) = clock.pause_next_mark_parked_predicate();
    let (result_tx, result_rx) = mpsc::channel();
    let clock_wait = Arc::clone(&clock);
    thread::spawn(move || {
        let mark = clock_wait.mark_parked(until, Deadline::after(Duration::from_secs(5)));
        match result_tx.send(mark) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("waited for mark_parked to block with nobody parked");
    release_waiter.send(()).unwrap();
    let (parker_release_tx, parker_release_rx) = mpsc::channel::<()>();
    let (parker_exit_tx, parker_exit_rx) = mpsc::channel();
    let clock_park = Arc::clone(&clock);
    let parker = thread::spawn(move || {
        clock_park.wait_until(Some(until), &mut |bound| {
            assert_eq!(bound, None);
            parker_release_rx
                .recv()
                .expect("waited for the test to release the parked thread");
        });
        if let Ok(()) = parker_exit_tx.send(()) {}
    });
    let parker_id = parker.thread().id();
    assert!(
        clock.await_thread_parked(
            parker_id,
            Some(until),
            Deadline::after(Duration::from_secs(5))
        ),
        "the parker parked at its deadline: {:?}, thread {parker_id:?}",
        clock.parked(),
    );
    let mark = Deadline::after(Duration::from_secs(5))
        .recv(&result_rx)
        .expect("waited for mark_parked to return the park")
        .expect("a parked thread is marked");
    assert_eq!(mark.0.len(), 1);
    assert_eq!(mark.0[0].0, parker_id);
    parker_release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&parker_exit_rx)
        .expect("the parked thread exits");
}

/// Runs the helper on a worker thread, catching its intentional panic: the
/// worker sends `Ok(mark)` on success and `Err(message)` on failure, so the
/// test receives the outcome through its deadline instead of joining a thread
/// that a bug would hang.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
/// Runs `work` on a worker thread, catching its intentional panic: the
/// worker sends `Ok(mark)` on success and `Err(message)` on failure, so the
/// test receives the outcome through its deadline instead of joining a
/// thread that a bug would hang.
fn run_worker<T>(work: impl FnOnce() -> T + Send + 'static) -> mpsc::Receiver<Result<T, String>>
where
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        let outcome = outcome.map_err(|payload| {
            if let Some(message) = payload.downcast_ref::<String>() {
                message.clone()
            } else if let Some(message) = payload.downcast_ref::<&str>() {
                (*message).to_string()
            } else {
                "a panic without a message".to_string()
            }
        });
        match tx.send(outcome) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx
}

fn run_advance_when_parked(
    clock: &Arc<FakeClock>,
    expected: Option<std::time::Instant>,
    thread: Option<std::thread::ThreadId>,
    advance: Duration,
    deadline: Deadline,
) -> mpsc::Receiver<Result<super::Mark, String>> {
    let clock = Arc::clone(clock);
    run_worker(move || clock.advance_when_parked(expected, thread, advance, &deadline))
}

fn run_try_advance_when_parked(
    clock: &Arc<FakeClock>,
    expected: Option<std::time::Instant>,
    thread: Option<std::thread::ThreadId>,
    advance: Duration,
    pause: Duration,
) -> mpsc::Receiver<Result<Result<super::Mark, super::Miss>, String>> {
    let clock = Arc::clone(clock);
    run_worker(move || clock.try_advance_when_parked(expected, thread, advance, pause))
}

/// Channel-held parks in sequence; dropping the release sender ends the
/// sequence too. Nothing joins the holder, including on a test failure.
fn park_seq(
    clock: &Arc<FakeClock>,
    untils: Vec<Option<std::time::Instant>>,
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
    let holder = thread::spawn(move || {
        for until in untils {
            let mut released = false;
            clock.wait_until(until, &mut |_| {
                let _sent = parked_tx.send(());
                released = release_rx.recv().is_ok();
            });
            if !released {
                break;
            }
        }
        let _sent = exit_tx.send(());
    });
    (parked_rx, release_tx, exit_rx, holder.thread().id())
}

/// One thread holds two parks until the test sends or drops its release.
fn park_nested(
    clock: &Arc<FakeClock>,
    until: std::time::Instant,
) -> (
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
    std::thread::ThreadId,
) {
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (exit_tx, exit_rx) = mpsc::channel();
    let clock = Arc::clone(clock);
    let holder = thread::spawn(move || {
        clock.wait_until(Some(until), &mut |_| {
            clock.wait_until(Some(until), &mut |_| {
                let _sent = parked_tx.send(());
                let _released = release_rx.recv();
            });
        });
        let _sent = exit_tx.send(());
    });
    (parked_rx, release_tx, exit_rx, holder.thread().id())
}

#[test]
fn a_stale_park_on_the_named_thread_is_rejected_while_another_thread_reparks_at_its_instant() {
    // Fails on the base: A's held park matches after the zero-length move.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (a_park, a_release, a_exit, a) = park_seq(&clock, vec![Some(e), Some(e)]);
    let (b_park, b_release, b_exit, _) = park_seq(&clock, vec![Some(e), Some(e)]);
    let parked = Deadline::after(Duration::from_secs(5));
    parked.recv_or_fail(&a_park, "A's first park");
    parked.recv_or_fail(&b_park, "B's first park");
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        Some(a),
        Duration::ZERO,
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the zero step")
        .unwrap();
    b_release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&b_park, "B's fresh park");
    let before = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        Some(a),
        e.duration_since(before),
        Deadline::after(Duration::ZERO),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the stale named park rejection")
        .unwrap_err();
    assert!(
        message.contains("the deadline passed before the named park arrived"),
        "{message}"
    );
    assert_eq!(clock.now(), before);
    a_release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&a_park, "A's fresh park");
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        Some(a),
        e.duration_since(clock.now()),
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the fresh named park advance")
        .unwrap();
    assert_eq!(clock.now(), e);
    a_release.send(()).unwrap();
    b_release.send(()).unwrap();
    let exited = Deadline::after(Duration::from_secs(5));
    exited.recv_or_fail(&a_exit, "A's exit");
    exited.recv_or_fail(&b_exit, "B's exit");
}

#[test]
fn a_step_past_the_named_threads_stale_park_waits_for_its_new_park() {
    // Fails on the base: the stale E1 park is reported as a mismatch.
    let clock = FakeClock::new();
    let e1 = clock.now() + Duration::from_secs(30);
    let e2 = e1 + Duration::from_secs(10);
    let e3 = e2 + Duration::from_secs(10);
    let (parked, release, exit, a) = park_seq(&clock, vec![Some(e1), Some(e2), Some(e3)]);
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "A's first park");
    let outcome = run_advance_when_parked(
        &clock,
        Some(e1),
        Some(a),
        e1.duration_since(clock.now()),
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the first advance")
        .unwrap();
    assert_eq!(clock.now(), e1);
    for (expected, selected) in [(e2, Some(a)), (e3, None)] {
        let (checked, resume) = clock.pause_next_advance_wait();
        let outcome = run_advance_when_parked(
            &clock,
            Some(expected),
            selected,
            expected.duration_since(clock.now()),
            Deadline::after(Duration::from_secs(30)),
        );
        Deadline::after(Duration::from_secs(5))
            .recv_or_fail(&checked, "the helper waiting for a newer park");
        resume.send(()).unwrap();
        release.send(()).unwrap();
        Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "A's newer park");
        Deadline::after(Duration::from_secs(5))
            .recv_or_fail(&outcome, "the newer park advance")
            .unwrap();
        assert_eq!(clock.now(), expected);
    }
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "A's exit");
}

/// With no active wait, a move is not a competing driver for the next wait.
#[test]
fn a_move_before_a_wait_does_not_fail_the_next_advance() {
    let clock = FakeClock::new();
    clock.advance(Duration::from_secs(1));
    let until = clock.now() + Duration::from_secs(30);
    let (parked, release, exit, thread) = park_seq(&clock, vec![Some(until)]);
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the fresh park");
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        Some(thread),
        until.duration_since(clock.now()),
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the advance after an unwatched move")
        .unwrap();
    assert_eq!(clock.now(), until);
    assert_eq!(clock.wall(), wall_epoch() + Duration::from_secs(31));
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn another_drivers_move_fails_an_active_advance_wait() {
    // Fails on the base: no move notification ends the helper's wait.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (checked, resume) = clock.pause_next_advance_wait();
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        None,
        Duration::from_millis(1),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&checked, "the active helper's condvar wait");
    // The pause holds the state lock, so release it before reading now().
    resume.send(()).unwrap();
    let before = clock.now();
    clock.advance_marked(Duration::from_millis(1));
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the other-driver failure")
        .unwrap_err();
    assert!(
        message.contains("time moved during the wait by another driver"),
        "{message}"
    );
    assert_eq!(clock.now(), before + Duration::from_millis(1));
    assert!(!super::lock(&clock.state).watching);
}

#[test]
fn a_move_then_a_wrong_park_reports_the_move() {
    // Fails on the base with the pass pause: it reports the wrong park.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (entered, resume) = clock.pause_next_advance_pass();
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        None,
        Duration::from_millis(1),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&entered, "the helper's pass pause");
    let before = clock.now();
    clock.advance_marked(Duration::from_millis(1));
    let (parked, release, exit, _) = park_once(&clock, Some(e + Duration::from_secs(1)));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the later wrong park");
    resume.send(()).unwrap();
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the first logged failure")
        .unwrap_err();
    assert!(
        message.contains("time moved during the wait by another driver"),
        "{message}"
    );
    assert!(!message.contains("different instant"), "{message}");
    assert_eq!(clock.now(), before + Duration::from_millis(1));
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the wrong parker's exit");
}

#[test]
fn a_wrong_park_then_a_move_reports_the_wrong_park() {
    // Passes on the base with the pass pause; fails a Moved-first scan.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (entered, resume) = clock.pause_next_advance_pass();
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        None,
        Duration::from_millis(1),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&entered, "the helper's pass pause");
    let (parked, release, exit, _) = park_once(&clock, Some(e + Duration::from_secs(1)));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the earlier wrong park");
    let before = clock.now();
    clock.advance_marked(Duration::from_millis(1));
    resume.send(()).unwrap();
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the first logged failure")
        .unwrap_err();
    assert!(
        message.contains("a watched park arrived at a different instant first"),
        "{message}"
    );
    assert!(!message.contains("time moved"), "{message}");
    assert_eq!(clock.now(), before + Duration::from_millis(1));
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the wrong parker's exit");
}

#[test]
fn a_move_before_the_named_park_fails_rather_than_matches() {
    // Fails on the base with the pass pause: it advances on the match.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (entered, resume) = clock.pause_next_advance_pass();
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&entered, "the helper's pass pause");
    let before = clock.now();
    clock.advance_marked(Duration::from_millis(1));
    let (parked, release, exit, _) = park_once(&clock, Some(e));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the later matching park");
    resume.send(()).unwrap();
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the move before the match")
        .unwrap_err();
    assert!(
        message.contains("time moved during the wait by another driver"),
        "{message}"
    );
    assert_eq!(clock.now(), before + Duration::from_millis(1));
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the matching parker's exit");
}

#[test]
fn a_named_threads_park_held_across_an_advance_does_not_match() {
    // Fails on the base: advance leaves the held park matchable.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (parked, release, exit, a) = park_once(&clock, Some(e));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "A's park");
    clock.advance(Duration::from_millis(1));
    let before = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        Some(a),
        e.duration_since(before),
        Deadline::after(Duration::ZERO),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the stale park rejection")
        .unwrap_err();
    assert!(
        message.contains("the deadline passed before the named park arrived"),
        "{message}"
    );
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(1));
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "A's exit");
}

#[test]
fn an_unnamed_park_held_across_a_sleep_does_not_match_and_its_re_park_does() {
    // Fails on the base: sleep leaves the held park matchable.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (first, left, second, exit, release, gate) = park_twice(&clock, e);
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&first, "the first park");
    Clock::sleep(&*clock, Duration::from_millis(1));
    let before = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        None,
        e.duration_since(before),
        Deadline::after(Duration::ZERO),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the stale unnamed park rejection")
        .unwrap_err();
    assert!(
        message.contains("the deadline passed before the named park arrived"),
        "{message}"
    );
    assert_eq!(clock.now(), before);
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&left, "leaving the first park");
    gate.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&second, "the second park");
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        None,
        e.duration_since(clock.now()),
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the fresh unnamed park advance")
        .unwrap();
    assert_eq!(clock.now(), e);
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the twice parker's exit");
}

#[test]
fn one_fresh_park_beside_stale_parks_matches_with_no_thread_named() {
    // Fails on the base: the two stale threads count as several threads.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (a_park, a_release, a_exit, _) = park_once(&clock, Some(e));
    let (b_park, b_release, b_exit, _) = park_once(&clock, Some(e));
    let parked = Deadline::after(Duration::from_secs(5));
    parked.recv_or_fail(&a_park, "A's park");
    parked.recv_or_fail(&b_park, "B's park");
    clock.advance(Duration::from_millis(1));
    let (c_park, c_release, c_exit, c) = park_once(&clock, Some(e));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&c_park, "C's fresh park");
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        None,
        e.duration_since(clock.now()),
        Deadline::after(Duration::from_secs(5)),
    );
    let mark = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the sole fresh thread's advance")
        .unwrap();
    assert!(mark.0.iter().any(|(thread, _)| *thread == c), "{mark:?}");
    assert_eq!(clock.now(), e);
    a_release.send(()).unwrap();
    b_release.send(()).unwrap();
    c_release.send(()).unwrap();
    let exited = Deadline::after(Duration::from_secs(5));
    exited.recv_or_fail(&a_exit, "A's exit");
    exited.recv_or_fail(&b_exit, "B's exit");
    exited.recv_or_fail(&c_exit, "C's exit");
}

#[test]
fn nested_parks_on_one_thread_match_with_no_thread_named() {
    // Fails on the base: it counts parks rather than distinct threads.
    let clock = FakeClock::new();
    let e = clock.now() + Duration::from_secs(30);
    let (parked, release, exit, t) = park_nested(&clock, e);
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the nested inner park");
    let outcome = run_advance_when_parked(
        &clock,
        Some(e),
        None,
        e.duration_since(clock.now()),
        Deadline::after(Duration::from_secs(5)),
    );
    let mark = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the nested park advance")
        .unwrap();
    assert_eq!(mark.0.len(), 1, "{mark:?}");
    assert_eq!(mark.0[0].0, t);
    assert_eq!(clock.now(), e);
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the nested parker's exit");
}

#[test]
fn advance_when_parked_moves_time_short_of_a_park_present_before_it_starts() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let (wake_tx, wake_rx) = mpsc::channel();
    let wake: Arc<dyn Wake> = Arc::new(ChanWake(wake_tx));
    clock.subscribe(Arc::downgrade(&wake));
    let mark = clock.advance_when_parked(
        Some(until),
        None,
        Duration::from_millis(5),
        &Deadline::after(Duration::from_secs(5)),
    );
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    assert_eq!(clock.wall(), wall_epoch() + Duration::from_millis(5));
    assert!(
        mark.0.iter().any(|(thread, _)| *thread == id),
        "the returned mark holds the parked thread: thread {id:?}",
    );
    Deadline::after(Duration::from_secs(2))
        .recv(&wake_rx)
        .expect("advance_when_parked wakes subscribers");
    drop(wake);
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_advances_exactly_to_the_named_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    clock.advance_when_parked(
        Some(until),
        None,
        Duration::from_secs(30),
        &Deadline::after(Duration::from_secs(5)),
    );
    assert_eq!(clock.now(), until);
    assert_eq!(clock.wall(), wall_epoch() + Duration::from_secs(30));
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_stops_one_instant_short_of_the_named_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    clock.advance_when_parked(
        Some(until),
        None,
        Duration::from_secs(30) - Duration::from_nanos(1),
        &Deadline::after(Duration::from_secs(5)),
    );
    assert_eq!(
        clock.now(),
        until.checked_sub(Duration::from_nanos(1)).unwrap()
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_waits_for_a_park_that_arrives_after_it_starts() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    // The pause proves the helper is already waiting when the park arrives:
    // the acknowledgement orders the wait before the parker starts, so this
    // exercises the arriving-after-start path, not the already-parked one.
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("waited for advance_when_parked to wait with nobody parked");
    release_waiter.send(()).unwrap();
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let mark = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to see the park")
        .expect("advance_when_parked advances for the park");
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    assert_eq!(mark.0.len(), 1);
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_fails_a_wrong_park_first() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (parked, release, exit, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let wall = clock.wall();
    // A 30 s helper bound with a 5 s observation bound: a mismatch fails at
    // once, it does not merely expire.
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the wrong park")
        .expect_err("advance_when_parked fails a wrong park");
    assert!(
        message.contains("a watched park arrived at a different instant first"),
        "the failure names its kind: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    assert_eq!(clock.wall(), wall, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_fails_a_park_one_instant_before_the_named_one() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until.checked_sub(Duration::from_nanos(1)).unwrap();
    let (parked, release, exit, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the wrong park")
        .expect_err("advance_when_parked fails a park one instant early");
    assert!(
        message.contains("a watched park arrived at a different instant first"),
        "the failure names its kind: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_fails_an_advance_past_the_named_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let wall = clock.wall();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_secs(30) + Duration::from_nanos(1),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the advance")
        .expect_err("advance_when_parked fails an advance past the named park");
    assert!(
        message.contains("the advance would pass the named instant"),
        "the failure names its kind: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    assert_eq!(clock.wall(), wall, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_fails_a_finite_park_where_none_was_named() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        None,
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the finite park")
        .expect_err("advance_when_parked fails a finite park where None was named");
    assert!(
        message.contains("where None was named"),
        "the failure names its kind: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_advances_for_an_unbounded_park() {
    let clock = FakeClock::new();
    let (parked, release, exit, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    clock.advance_when_parked(
        None,
        None,
        Duration::from_millis(5),
        &Deadline::after(Duration::from_secs(5)),
    );
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    assert_eq!(clock.wall(), wall_epoch() + Duration::from_millis(5));
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_fails_an_unbounded_park_where_a_finite_instant_was_named() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the unbounded park")
        .expect_err("advance_when_parked fails an unbounded park");
    assert!(
        message.contains("without a deadline"),
        "the failure names its kind: {message}",
    );
    assert!(
        message.contains("None"),
        "the failure reports the unbounded park: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_fails_at_the_deadline_naming_no_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let now = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_millis(200)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to expire")
        .expect_err("advance_when_parked fails at the deadline");
    assert!(
        message.contains("the deadline passed"),
        "the failure names its kind: {message}",
    );
    assert!(
        message.contains("parked threads: []"),
        "the failure names the empty park list: {message}",
    );
    assert_eq!(clock.now(), now, "an expiry moves no time");
}

#[test]
fn advance_when_parked_waits_for_the_named_thread() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    // The test thread never parks: the other thread's park at the right
    // instant is not proof, so the wait expires instead of advancing. A zero
    // deadline fails at once with the unrelated park held: no wall-clock
    // wait is needed to prove the helper takes nobody else's park.
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        Some(thread::current().id()),
        Duration::from_millis(5),
        Deadline::after(Duration::ZERO),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to expire")
        .expect_err("advance_when_parked waits for the named thread");
    assert!(
        message.contains("the deadline passed"),
        "the wait expires rather than taking another thread's park: {message}",
    );
    assert!(
        message.contains("ThreadId"),
        "the failure still lists the parked thread: {message}",
    );
    assert_eq!(clock.now(), now, "an expiry moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_advances_for_the_named_thread_among_several_parks() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    clock.advance_when_parked(
        Some(until),
        Some(b_id),
        Duration::from_millis(5),
        &Deadline::after(Duration::from_secs(5)),
    );
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_fails_several_parks_with_no_thread_named() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, first_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    let (parked_b, release_b, exit_b, second_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    let now = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the ambiguous wait")
        .expect_err("advance_when_parked fails several parks with no thread named");
    assert!(
        message.contains("no thread was named"),
        "the failure names its kind: {message}",
    );
    let list = message
        .split("parked threads: [")
        .nth(1)
        .expect("the failure includes the parked thread list");
    let parked_at = Some(until);
    let first = format!("{first_id:?} at {parked_at:?}");
    let rest = list
        .strip_prefix(first.as_str())
        .expect("the first parked thread leads the list");
    assert!(rest.starts_with(", "), "two entries are separated: {list}",);
    assert!(
        rest.contains(&format!("{second_id:?} at {parked_at:?}")),
        "both parked threads and their instants appear: {list}",
    );
    assert_eq!(clock.now(), now, "an ambiguous wait moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_rejects_a_named_park_at_now() {
    let clock = FakeClock::new();
    let now = clock.now();
    let wall = clock.wall();
    let outcome = run_advance_when_parked(
        &clock,
        Some(now),
        None,
        Duration::from_millis(1),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the past park")
        .expect_err("advance_when_parked rejects a named park at now");
    assert!(
        message.contains("already passed"),
        "the failure names its kind: {message}",
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
    assert_eq!(clock.wall(), wall, "a rejected wait moves no time");
}

#[test]
fn advance_when_parked_rejects_a_named_park_before_now() {
    let clock = FakeClock::new();
    clock.advance(Duration::from_millis(5));
    let past = clock.origin();
    let now = clock.now();
    let outcome = run_advance_when_parked(
        &clock,
        Some(past),
        None,
        Duration::from_millis(1),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the past park")
        .expect_err("advance_when_parked rejects a named park before now");
    assert!(
        message.contains("already passed"),
        "the failure names its kind: {message}",
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
}

#[test]
fn advance_when_parked_rejects_an_advance_the_clock_cannot_represent() {
    let clock = FakeClock::new();
    let (parked, release, exit, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let wall = clock.wall();
    let outcome = run_advance_when_parked(
        &clock,
        None,
        None,
        Duration::MAX,
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the advance")
        .expect_err("advance_when_parked rejects an overflowing advance");
    let overflow = if clock.origin().checked_add(Duration::MAX).is_none() {
        "monotonic instant"
    } else {
        "wall time"
    };
    assert!(
        message.contains(&format!("overflows the clock's {overflow}")),
        "the failure names the overflowing clock: {message}",
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
    assert_eq!(clock.wall(), wall, "a rejected wait moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

/// Records each panic's file and line under a temporary hook, restoring the
/// previous hook on drop so no later phase of the test observes it. The hook
/// itself never panics: it records through `try_lock` only.
#[allow(
    clippy::type_complexity,
    reason = "the panic hook's type is set by std"
)]
struct HookWatch {
    seen: Arc<Mutex<Vec<(String, u32)>>>,
    old: Option<Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>>,
}

#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
impl HookWatch {
    fn install() -> Self {
        let seen: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let old = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let (file, line) = info.location().map_or_else(
                || ("<no location>".to_string(), 0),
                |location| (location.file().to_string(), location.line()),
            );
            if let Ok(mut guard) = record.try_lock() {
                guard.push((file, line));
            }
        }));
        Self {
            seen,
            old: Some(old),
        }
    }

    fn locations(&self) -> Vec<(String, u32)> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for HookWatch {
    fn drop(&mut self) {
        if let Some(old) = self.old.take() {
            std::panic::set_hook(old);
        }
    }
}

/// Asserts the hook saw exactly one panic at this test's helper call: the
/// failure belongs to the caller's line.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn assert_caller(locations: &[(String, u32)], line: u32) {
    assert_eq!(
        locations.len(),
        1,
        "the helper fails once, at the caller's line: {locations:?}",
    );
    assert!(
        locations[0].0.ends_with("clock_tests.rs"),
        "the failure is reported in this test: {:?}",
        locations[0].0,
    );
    assert_eq!(
        locations[0].1, line,
        "the failure is reported at the caller's line",
    );
}

#[test]
fn advance_when_parked_names_the_caller_and_every_park_on_a_wrong_instant() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (parked_a, release_a, exit_a, a_id) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    let now = clock.now();
    let step = Duration::from_millis(5);
    let bound = Deadline::after(Duration::from_secs(5));
    let watch = HookWatch::install();
    let line = line!() + 1;
    let run = || clock.advance_when_parked(Some(until), None, step, &bound);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    let locations = watch.locations();
    drop(watch);
    let message = outcome
        .expect_err("advance_when_parked fails a wrong park")
        .downcast::<String>()
        .expect("the helper panics with a message");
    assert_caller(&locations, line);
    assert!(
        message.contains("a watched park arrived at a different instant first"),
        "the failure names its kind: {message}",
    );
    assert!(
        message.contains(&format!("{a_id:?}")),
        "the failure names the first parked thread: {message}",
    );
    assert!(
        message.contains(&format!("{b_id:?}")),
        "the failure names the second parked thread: {message}",
    );
    assert!(
        message.contains("at Some("),
        "the failure reports the finite instant: {message}",
    );
    assert!(
        message.contains("at None"),
        "the failure reports the unbounded park: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_names_the_caller_on_a_finite_park_where_none_was_named() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let step = Duration::from_millis(5);
    let bound = Deadline::after(Duration::from_secs(5));
    let watch = HookWatch::install();
    let line = line!() + 1;
    let run = || clock.advance_when_parked(None, None, step, &bound);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    let locations = watch.locations();
    drop(watch);
    let message = outcome
        .expect_err("advance_when_parked fails a finite park")
        .downcast::<String>()
        .expect("the helper panics with a message");
    assert_caller(&locations, line);
    assert!(
        message.contains("where None was named"),
        "the failure names its kind: {message}",
    );
    assert!(
        message.contains(&format!("{id:?}")),
        "the failure names the parked thread: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_names_the_caller_on_an_advance_past_the_named_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let step = Duration::from_secs(31);
    let bound = Deadline::after(Duration::from_secs(5));
    let watch = HookWatch::install();
    let line = line!() + 1;
    let run = || clock.advance_when_parked(Some(until), None, step, &bound);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    let locations = watch.locations();
    drop(watch);
    let message = outcome
        .expect_err("advance_when_parked fails the advance")
        .downcast::<String>()
        .expect("the helper panics with a message");
    assert_caller(&locations, line);
    assert!(
        message.contains("the advance would pass the named instant"),
        "the failure names its kind: {message}",
    );
    assert!(
        message.contains(&format!("{id:?}")),
        "the failure names the parked thread: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_names_the_caller_on_the_deadline_with_no_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let now = clock.now();
    let step = Duration::from_millis(5);
    let bound = Deadline::after(Duration::from_millis(200));
    let watch = HookWatch::install();
    let line = line!() + 1;
    let run = || clock.advance_when_parked(Some(until), None, step, &bound);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    let locations = watch.locations();
    drop(watch);
    let message = outcome
        .expect_err("advance_when_parked fails at the deadline")
        .downcast::<String>()
        .expect("the helper panics with a message");
    assert_caller(&locations, line);
    assert!(
        message.contains("the deadline passed"),
        "the failure names its kind: {message}",
    );
    assert!(
        message.contains("parked threads: []"),
        "the failure names the empty park list: {message}",
    );
    assert_eq!(clock.now(), now, "an expiry moves no time");
}

#[test]
fn advance_when_parked_fails_a_wrong_park_beside_the_right_one() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    let now = clock.now();
    // The right park is held too, but the first wrong one fails the wait: a
    // convenient later park never hides it.
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the wrong park")
        .expect_err("advance_when_parked fails a wrong park beside the right one");
    assert!(
        message.contains("a watched park arrived at a different instant first"),
        "the failure names its kind: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_advances_for_the_selected_thread_beside_a_wrong_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    // Only the named thread is watched: the other thread's wrong park neither
    // satisfies the wait nor fails it.
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        Some(b_id),
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to see the named thread")
        .expect("advance_when_parked advances for the named thread");
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_fails_a_wrong_park_that_left_before_the_right_one() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    // The pause proves the helper checked with nobody parked and is waiting,
    // so the wrong park below arrives after the wait starts. No outside held
    // wait can order the wrong park's leave against the helper's diagnosis,
    // which is why the helper observes arrivals under its own lock.
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("waited for advance_when_parked to wait with nobody parked");
    release_waiter.send(()).unwrap();
    // Parks wrong, reports from inside the wait (past the park, so the helper
    // observed it), then leaves at once without any release from the test.
    let (wrong_tx, wrong_rx) = mpsc::channel();
    let clock_wrong = Arc::clone(&clock);
    thread::spawn(move || {
        clock_wrong.wait_until(Some(wrong), &mut |_bound| match wrong_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        });
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&wrong_rx)
        .expect("waited for the thread to park at the wrong instant");
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park at the named instant");
    let now = clock.now();
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the wrong park")
        .expect_err("advance_when_parked fails a wrong park that left");
    assert!(
        message.contains("a watched park arrived at a different instant first"),
        "the left park still fails the wait: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_with_a_wrong_park_held_never_reaches_its_wait_point() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (parked, release, exit, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    // Armed while a watched mismatch is already held: the helper fails before
    // it could wait, so the wait point never signals. Without the predicate
    // gate the hook would signal and the bounded receive below would succeed.
    let (waiting, _release_waiter) = clock.pause_next_advance_wait();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to reject the wrong park")
        .expect_err("advance_when_parked fails the held wrong park");
    assert!(
        Deadline::after(Duration::ZERO).recv(&waiting).is_err(),
        "the wait point does not signal while a mismatch is held",
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_advances_for_a_re_park_after_a_leave() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (first, left, second, exit, release, gate) = park_twice(&clock, until);
    Deadline::after(Duration::from_secs(5))
        .recv(&first)
        .expect("waited for the thread to park");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&left)
        .expect("waited for the thread to leave its park");
    gate.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&second)
        .expect("waited for the thread to park again");
    let mark = clock.advance_when_parked(
        Some(until),
        None,
        Duration::from_millis(5),
        &Deadline::after(Duration::from_secs(5)),
    );
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    assert_eq!(
        mark.0.len(),
        1,
        "the returned mark holds the re-parked thread",
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_wakes_a_subscriber_after_releasing_the_clock_lock() {
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
        clock.await_parked(until, Deadline::after(Duration::from_secs(2))),
        "waited for the thread to park at its deadline"
    );
    assert_eq!(
        Deadline::after(Duration::from_secs(2))
            .recv(&rx)
            .expect("waited for the thread to report it had parked"),
        "parked"
    );
    // The thread still holds the stand-in mutex. The helper wakes it only
    // after the wait registered the park and the closure is in `Condvar::wait`,
    // and the wake reads the clock, which deadlocks while the clock lock is held.
    let mark = clock.advance_when_parked(
        Some(until),
        None,
        Duration::from_secs(10),
        &Deadline::after(Duration::from_secs(5)),
    );
    assert_eq!(mark.0.len(), 1);
    assert_eq!(
        Deadline::after(Duration::from_secs(2))
            .recv(&rx)
            .expect("waited for advance_when_parked to wake the parked thread"),
        "woke"
    );
    assert!(clock.parked().is_empty());
}

struct PanicWake;

impl Wake for PanicWake {
    fn wake(&self) {
        panic!("the subscriber panicked while waking");
    }
}

#[test]
fn advance_when_parked_moves_time_before_a_panicking_subscriber_wakes() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let wake: Arc<dyn Wake> = Arc::new(PanicWake);
    clock.subscribe(Arc::downgrade(&wake));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        clock.advance_when_parked(
            Some(until),
            None,
            Duration::from_millis(5),
            &Deadline::after(Duration::from_secs(5)),
        );
    }));
    assert!(
        outcome.is_err(),
        "the subscriber's panic reaches the helper's caller",
    );
    // Time moves before subscribers wake, as advance_marked promises, so the
    // panic after a successful move does not roll the clock back.
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    drop(wake);
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

/// What `advance_when_parked` has observed while it waits: the parks that
/// arrived after the wait started, whether they are still held or left.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn observed_snapshot(
    clock: &Arc<FakeClock>,
) -> Vec<(std::thread::ThreadId, Option<std::time::Instant>)> {
    super::lock(&clock.state)
        .observed
        .iter()
        .filter_map(|entry| match entry {
            super::Observation::Park(thread, until) => Some((*thread, *until)),
            super::Observation::Moved => None,
        })
        .collect()
}

#[test]
fn advance_when_parked_observes_a_park_that_leaves_while_it_waits() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    // The selected thread gates its park on the test, so its id is known
    // before the helper starts and it parks only to resolve the wait.
    let (start_tx, start_rx) = mpsc::channel::<()>();
    let (selected_tx, selected_rx) = mpsc::channel();
    let (selected_release_tx, selected_release_rx) = mpsc::channel::<()>();
    let (selected_exit_tx, selected_exit_rx) = mpsc::channel();
    let clock_selected = Arc::clone(&clock);
    let selected = thread::spawn(move || {
        start_rx
            .recv()
            .expect("waited for the test to start the selected thread");
        clock_selected.wait_until(Some(until), &mut |_bound| {
            match selected_tx.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
            selected_release_rx
                .recv()
                .expect("waited for the test to release the parked thread");
        });
        if let Ok(()) = selected_exit_tx.send(()) {}
    });
    let selected_id = selected.thread().id();
    // The pause proves the helper armed its observation before anything
    // parks, so the park below arrives after the wait starts.
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        Some(selected_id),
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("waited for advance_when_parked to wait with nobody parked");
    release_waiter.send(()).unwrap();
    // Another thread parks at the named instant but is not selected, so the
    // helper keeps waiting while observing it.
    let (parked, release, exit, other_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the other thread to park");
    assert!(
        observed_snapshot(&clock)
            .iter()
            .any(|(id, parked_until)| *id == other_id && *parked_until == Some(until)),
        "the helper observed the other thread's arrival",
    );
    // The other thread leaves before the helper diagnoses anything: the
    // observation stays while the held park goes.
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the other thread exits");
    assert!(
        !clock.parked().contains(&Some(until)),
        "the other thread left its park",
    );
    assert!(
        observed_snapshot(&clock)
            .iter()
            .any(|(id, parked_until)| *id == other_id && *parked_until == Some(until)),
        "the helper keeps the observation after the park left",
    );
    // The selected thread's park resolves the wait, proving the observation
    // of the other thread neither satisfied nor failed it.
    start_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&selected_rx)
        .expect("waited for the selected thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to see the selected thread")
        .expect("advance_when_parked advances for the selected thread");
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(5));
    selected_release_tx.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&selected_exit_rx)
        .expect("the selected thread exits");
}

#[test]
fn a_failed_advance_wait_clears_its_observations_before_the_next_wait() {
    let clock = FakeClock::new();
    let first_expected = clock.origin() + Duration::from_secs(30);
    let wrong = first_expected + Duration::from_secs(1);
    let (first_checked, release_first_wait) = clock.pause_next_advance_wait();
    let first = run_advance_when_parked(
        &clock,
        Some(first_expected),
        None,
        Duration::from_millis(1),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&first_checked)
        .expect("waited for the first advance wait to start");
    release_first_wait.send(()).unwrap();

    let (wrong_parked, release_wrong, wrong_exit, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5))
        .recv(&wrong_parked)
        .expect("waited for the wrong park to arrive");
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&first)
        .expect("waited for the first advance wait to reject the wrong park")
        .expect_err("the first advance wait rejects a wrong park");
    assert!(message.contains("different instant first"), "{message}");
    {
        let state = super::lock(&clock.state);
        assert!(!state.watching, "the failed wait stops watching");
        assert!(
            state.observed.is_empty(),
            "the failed wait clears observations"
        );
    }
    release_wrong.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&wrong_exit)
        .expect("the wrong park exits");

    let second_expected = clock.origin() + Duration::from_secs(60);
    let (second_checked, release_second_wait) = clock.pause_next_advance_wait();
    let second = run_advance_when_parked(
        &clock,
        Some(second_expected),
        None,
        Duration::from_millis(1),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&second_checked)
        .expect("waited for the second advance wait to start");
    release_second_wait.send(()).unwrap();
    let (right_parked, release_right, right_exit, _) = park_once(&clock, Some(second_expected));
    Deadline::after(Duration::from_secs(5))
        .recv(&right_parked)
        .expect("waited for the second wait's matching park");
    Deadline::after(Duration::from_secs(5))
        .recv(&second)
        .expect("waited for the second advance wait")
        .expect("the second advance wait succeeds with a different expectation");
    assert_eq!(clock.now(), clock.origin() + Duration::from_millis(1));
    release_right.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&right_exit)
        .expect("the matching park exits");
}

#[test]
fn advance_when_parked_rejects_a_second_wait_while_one_is_active() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    // The pause proves the first wait is waiting before the second starts,
    // so the second overlaps it rather than following it.
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let first = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("waited for the first advance wait to wait");
    release_waiter.send(()).unwrap();
    // A second wait while the first is active is rejected at once: it never
    // clears the first wait's observation log.
    let second = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let second_message = Deadline::after(Duration::from_secs(5))
        .recv(&second)
        .expect("waited for the second advance wait to be rejected")
        .expect_err("the second overlapping advance wait is rejected");
    assert!(
        second_message.contains("already active"),
        "the rejection names the overlap: {second_message}",
    );
    // The wrong park arrives and leaves before the named one; the first wait
    // still fails on it.
    let (wrong_tx, wrong_rx) = mpsc::channel();
    let clock_wrong = Arc::clone(&clock);
    thread::spawn(move || {
        clock_wrong.wait_until(Some(wrong), &mut |_bound| match wrong_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        });
    });
    Deadline::after(Duration::from_secs(5))
        .recv(&wrong_rx)
        .expect("waited for the thread to park at the wrong instant");
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park at the named instant");
    let now = clock.now();
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&first)
        .expect("waited for the first advance wait to reject the wrong park")
        .expect_err("the first advance wait still fails on its wrong park");
    assert!(
        message.contains("a watched park arrived at a different instant first"),
        "the left park still fails the first wait: {message}",
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn subscribing_one_waker_twice_wakes_it_once_per_advance() {
    let clock = FakeClock::new();
    let (tx, rx) = mpsc::channel();
    let wake: Arc<dyn Wake> = Arc::new(ChanWake(tx));
    clock.subscribe(Arc::downgrade(&wake));
    clock.subscribe(Arc::downgrade(&Arc::clone(&wake)));
    clock.advance(Duration::from_millis(5));
    assert!(rx.try_recv().is_ok(), "one advance wakes the waker once");
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the second subscription wakes nothing more"
    );
    clock.advance(Duration::from_millis(5));
    assert!(
        rx.try_recv().is_ok(),
        "a second advance wakes the waker once more"
    );
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the second subscription still wakes nothing more"
    );
    drop(wake);
}

#[test]
fn two_distinct_wakers_each_wake_once_per_advance() {
    let clock = FakeClock::new();
    let (tx, rx) = mpsc::channel();
    let first: Arc<dyn Wake> = Arc::new(ChanWake(tx.clone()));
    let second: Arc<dyn Wake> = Arc::new(ChanWake(tx));
    clock.subscribe(Arc::downgrade(&first));
    clock.subscribe(Arc::downgrade(&second));
    clock.advance(Duration::from_millis(5));
    assert!(rx.try_recv().is_ok(), "the first waker wakes");
    assert!(rx.try_recv().is_ok(), "the second waker wakes");
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "each distinct waker wakes exactly once"
    );
    drop(first);
    drop(second);
}

#[test]
fn subscribe_drops_dead_wakers() {
    let clock = FakeClock::new();
    let (tx, _rx) = mpsc::channel();
    let first: Arc<dyn Wake> = Arc::new(ChanWake(tx.clone()));
    clock.subscribe(Arc::downgrade(&first));
    drop(first);
    let second: Arc<dyn Wake> = Arc::new(ChanWake(tx));
    clock.subscribe(Arc::downgrade(&second));
    assert_eq!(
        super::lock(&clock.state).wakers.len(),
        1,
        "the dead subscription is gone"
    );
    drop(second);
}

fn run_advance_when_parked_threads(
    clock: &Arc<FakeClock>,
    expected: Option<std::time::Instant>,
    count: usize,
    advance: Duration,
    deadline: Deadline,
) -> mpsc::Receiver<Result<super::Mark, String>> {
    let clock = Arc::clone(clock);
    run_worker(move || clock.advance_when_parked_threads(expected, count, advance, &deadline))
}

#[test]
fn advance_when_parked_threads_advances_once_the_count_is_parked() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, a_id) = park_once(&clock, Some(until));
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    let step = Duration::from_millis(5);
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        step,
        Deadline::after(Duration::from_secs(5)),
    );
    let mark = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to advance")
        .expect("two threads parked at the named instant advance");
    assert_eq!(
        clock.now(),
        clock.origin() + step,
        "time moves by exactly the advance"
    );
    let threads = mark.threads();
    assert!(
        threads.contains(&a_id),
        "the mark holds the first thread: {threads:?}"
    );
    assert!(
        threads.contains(&b_id),
        "the mark holds the second thread: {threads:?}"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_threads_waits_for_the_last_thread() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let step = Duration::from_millis(5);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        step,
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("the helper waits for the second thread");
    // The pause holds the state lock, so the release goes first: nothing
    // can park while the helper is paused.
    release_waiter.send(()).expect("released the wait");
    assert_eq!(
        clock.now(),
        clock.origin(),
        "one parked thread never satisfies a count of two"
    );
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    let mark = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to advance")
        .expect("the second thread completes the count");
    assert_eq!(
        clock.now(),
        clock.origin() + step,
        "time moves by exactly the advance"
    );
    assert_eq!(mark.threads().len(), 2, "the mark holds both threads");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_threads_fails_at_the_deadline_with_fewer_threads() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        Duration::from_millis(5),
        Deadline::after(Duration::from_millis(200)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to expire")
        .expect_err("one parked thread never satisfies a count of two");
    assert!(
        message.contains("the deadline passed"),
        "the failure names its kind: {message}"
    );
    assert!(
        message.contains("the deadline passed before 2 threads parked at the named instant"),
        "the failure names the awaited count: {message}"
    );
    assert!(
        message.contains(", threads 2"),
        "the failure lists the thread count: {message}"
    );
    assert!(
        message.contains(&format!("{id:?} at {:?}", Some(until))),
        "the failure names the parked thread and its instant: {message}"
    );
    assert_eq!(clock.now(), now, "an expired wait moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");

    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let now = clock.now();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        Duration::from_millis(5),
        Deadline::after(Duration::from_millis(200)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to expire")
        .expect_err("no parked thread never satisfies a count of two");
    assert!(
        message.contains("the deadline passed"),
        "the failure names its kind: {message}"
    );
    assert!(
        message.contains("parked threads: []"),
        "the failure lists no parked thread: {message}"
    );
    assert_eq!(clock.now(), now, "an expired wait moves no time");
}

#[test]
fn advance_when_parked_threads_fails_with_more_than_the_count() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, a_id) = park_once(&clock, Some(until));
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, Some(until));
    let (parked_c, release_c, exit_c, c_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_c)
        .expect("waited for the third thread to park");
    let now = clock.now();
    // A generous deadline: the failure arrives at once, not at the deadline.
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to fail")
        .expect_err("three threads never satisfy a count of two");
    assert!(
        message.contains("more than 2"),
        "the failure names the excess: {message}"
    );
    for id in [a_id, b_id, c_id] {
        assert!(
            message.contains(&format!("{id:?}")),
            "the failure names every parked thread: {message}"
        );
    }
    assert!(
        message.contains("at Some("),
        "the failure reports the named instant: {message}"
    );
    assert_eq!(clock.now(), now, "an excess wait moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    release_c.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_c)
        .expect("the third thread exits");
}

#[test]
fn advance_when_parked_threads_counts_nested_parks_on_one_thread_once() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_nested(&clock, until);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the nested parks");
    let now = clock.now();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        Duration::from_millis(5),
        Deadline::after(Duration::ZERO),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to expire")
        .expect_err("one thread's nested parks never satisfy a count of two");
    assert!(
        message.contains("the deadline passed"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), now, "an expired wait moves no time");
    let step = Duration::from_millis(5);
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        1,
        step,
        Deadline::after(Duration::from_secs(5)),
    );
    let mark = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to advance")
        .expect("one thread's nested parks satisfy a count of one");
    assert_eq!(clock.now(), now + step, "time moves by exactly the advance");
    assert_eq!(
        mark.threads().len(),
        1,
        "nested parks on one thread count once"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_threads_does_not_count_a_thread_that_left() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let step = Duration::from_millis(5);
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        step,
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("the helper waits with nobody parked");
    // The pause holds the state lock, so the release goes first.
    release_waiter.send(()).expect("released the wait");
    // The helper's next look is the one that sees the leaver's park, so
    // this pause marks that look. Arm it before the leaver starts.
    let (looked, release_look) = clock.pause_next_advance_wait();
    // A leaver parks at the named instant and exits: its arrival reaches
    // the observation log, then it is gone.
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&looked)
        .expect("waited for the helper's look at the leaver's park");
    // The pause holds the state lock: release it before any thread that
    // needs the lock can finish, such as the leaver's exit.
    release_look.send(()).expect("released the look");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the leaver to park");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the leaver exits");
    assert_eq!(
        clock.now(),
        clock.origin(),
        "the leaver alone never satisfies a count of two"
    );
    // Arm the decision pause before the held thread parks, so the pause
    // fires on the look that sees it.
    let (decided, release_decision) = clock.pause_next_advance_wait();
    let (parked_a, release_a, exit_a, a_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first held thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&decided)
        .expect("the helper looks with one held thread and a leaver in its log");
    release_decision.send(()).expect("released the wait");
    assert_eq!(
        clock.now(),
        clock.origin(),
        "one held thread plus a leaver never satisfies a count of two"
    );
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second held thread to park");
    let mark = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to advance")
        .expect("two held threads satisfy the count");
    assert_eq!(
        clock.now(),
        clock.origin() + step,
        "time moves by exactly the advance"
    );
    let threads = mark.threads();
    assert!(
        threads.contains(&a_id),
        "the mark holds the first held thread: {threads:?}"
    );
    assert!(
        threads.contains(&b_id),
        "the mark holds the second held thread: {threads:?}"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first held thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second held thread exits");
}

#[test]
fn advance_when_parked_threads_ignores_parks_made_before_the_last_move() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    // Even a zero-length move closes the boundary: both parks are stale.
    clock.advance(Duration::ZERO);
    let now = clock.now();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        Duration::from_millis(5),
        Deadline::after(Duration::ZERO),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to expire")
        .expect_err("parks made before the last move are stale");
    assert!(
        message.contains("the deadline passed"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), now, "an expired wait moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_threads_fails_a_park_at_another_instant() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let earlier = clock.origin() + Duration::from_secs(29);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(earlier));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    let now = clock.now();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(5)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to fail")
        .expect_err("a park at another instant mismatches");
    assert!(
        message.contains("different instant"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");

    let clock = FakeClock::new();
    let step = Duration::from_millis(5);
    let (parked_a, release_a, exit_a, a_id) = park_once(&clock, None);
    let (parked_b, release_b, exit_b, b_id) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first unbounded park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second unbounded park");
    let outcome = run_advance_when_parked_threads(
        &clock,
        None,
        2,
        step,
        Deadline::after(Duration::from_secs(5)),
    );
    let mark = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to advance")
        .expect("two unbounded parks satisfy a count of two");
    assert_eq!(
        clock.now(),
        clock.origin() + step,
        "time moves by exactly the advance"
    );
    let threads = mark.threads();
    assert!(
        threads.contains(&a_id) && threads.contains(&b_id),
        "the mark holds both threads: {threads:?}"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_threads_with_count_one_matches_the_unnamed_helper() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let step = Duration::from_millis(5);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        1,
        step,
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to advance")
        .expect("one thread satisfies a count of one");
    assert_eq!(
        clock.now(),
        clock.origin() + step,
        "time moves by exactly the advance"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");

    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    let now = clock.now();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        1,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(5)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to fail")
        .expect_err("two threads never satisfy a count of one");
    assert!(
        message.contains("no thread was named"),
        "a count of one keeps the unnamed text: {message}"
    );
    assert!(
        !message.contains(", threads"),
        "a count of one adds no count text: {message}"
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

#[test]
fn advance_when_parked_threads_rejects_a_zero_count() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let now = clock.now();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        0,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to fail")
        .expect_err("a count of zero never waits");
    assert!(
        message.contains("at least 1"),
        "the failure names its kind: {message}"
    );
    assert!(
        message.contains("threads 0"),
        "the failure names the count: {message}"
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
    // The rejected wait never registered: a later wait still proceeds.
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let step = Duration::from_millis(5);
    let outcome = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        step,
        Deadline::after(Duration::from_secs(5)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked to advance")
        .expect("the clock still advances after a rejected count");
    assert_eq!(clock.now(), now + step, "time moves by exactly the advance");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_threads_rejects_an_advance_past_the_named_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_a)
        .expect("waited for the first thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_b)
        .expect("waited for the second thread to park");
    let now = clock.now();
    let outcome = run_advance_when_parked_threads(
        &clock,
        Some(until),
        2,
        Duration::from_secs(31),
        Deadline::after(Duration::from_secs(5)),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_threads to fail")
        .expect_err("an advance past the named park never moves");
    assert!(
        message.contains("would pass the named instant"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), now, "an overshoot moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_a)
        .expect("the first thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_b)
        .expect("the second thread exits");
}

/// Runs `advance_when_parked_then` on a worker thread, catching its
/// intentional panic: the worker sends `Ok(mark)` on success and
/// `Err(message)` on failure, so the test receives the outcome through its
/// deadline instead of joining a thread that a bug would hang.
fn run_advance_when_parked_then(
    clock: &Arc<FakeClock>,
    expected: Option<std::time::Instant>,
    thread: Option<std::thread::ThreadId>,
    advance: Duration,
    deadline: Deadline,
    hook: impl FnOnce() + Send + 'static,
) -> mpsc::Receiver<Result<super::Mark, String>> {
    let clock = Arc::clone(clock);
    run_worker(move || clock.advance_when_parked_then(expected, thread, advance, &deadline, hook))
}

/// A subscriber that reports the fake clock's `now()` at each wake.
struct WokeAt {
    tx: mpsc::Sender<std::time::Instant>,
    clock: Mutex<Option<Arc<FakeClock>>>,
}

impl Wake for WokeAt {
    fn wake(&self) {
        if let Some(clock) = self.clock.lock().unwrap().as_ref() {
            match self.tx.send(clock.now()) {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
        }
    }
}

#[test]
fn advance_when_parked_then_runs_the_hook_between_the_park_and_the_move() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let step = Duration::from_millis(5);
    let (wake_tx, wake_rx) = mpsc::channel();
    let wake: Arc<dyn Wake> = Arc::new(WokeAt {
        tx: wake_tx,
        clock: Mutex::new(Some(Arc::clone(&clock))),
    });
    clock.subscribe(Arc::downgrade(&wake));
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let outcome = run_advance_when_parked_then(
        &clock,
        Some(until),
        None,
        step,
        Deadline::after(Duration::from_secs(30)),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("the helper waits for the park");
    assert_eq!(
        hooks.load(Ordering::SeqCst),
        0,
        "the hook does not run before the park is observed"
    );
    // The pause holds the state lock, so the release goes first.
    release_waiter.send(()).expect("released the wait");
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to advance")
        .expect("the parked thread advances");
    assert_eq!(hooks.load(Ordering::SeqCst), 1, "the hook runs once");
    // The hook cannot read the clock, and the wake runs after the move
    // whichever order the hook took, so this event order alone cannot prove
    // the hook ran before the move: only the hook-panic test proves that.
    assert_eq!(
        Deadline::after(Duration::from_secs(5))
            .recv(&wake_rx)
            .expect("waited for the wake"),
        clock.origin() + step,
        "the wake sees the moved time"
    );
    assert_eq!(
        clock.now(),
        clock.origin() + step,
        "time moves by exactly the advance"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_then_never_runs_the_hook_for_a_wrong_instant() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (parked, release, exit, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let outcome = run_advance_when_parked_then(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(5)),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to fail")
        .expect_err("a wrong park mismatches");
    assert!(
        message.contains("different instant"),
        "the failure names its kind: {message}"
    );
    assert_eq!(
        hooks.load(Ordering::SeqCst),
        0,
        "a failed park never runs the hook"
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_then_never_runs_the_hook_for_a_finite_park_where_none_was_named() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let outcome = run_advance_when_parked_then(
        &clock,
        None,
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(5)),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to fail")
        .expect_err("a finite park where none was named mismatches");
    assert!(
        message.contains("where None was named"),
        "the failure names its kind: {message}"
    );
    assert_eq!(
        hooks.load(Ordering::SeqCst),
        0,
        "a failed park never runs the hook"
    );
    assert_eq!(clock.now(), now, "a mismatch moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_then_never_runs_the_hook_for_an_expired_deadline() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let now = clock.now();
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let outcome = run_advance_when_parked_then(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::ZERO),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to expire")
        .expect_err("an expired wait never runs the hook");
    assert!(
        message.contains("the deadline passed"),
        "the failure names its kind: {message}"
    );
    assert_eq!(
        hooks.load(Ordering::SeqCst),
        0,
        "an expired wait never runs the hook"
    );
    assert_eq!(clock.now(), now, "an expired wait moves no time");
}

#[test]
fn advance_when_parked_then_never_runs_the_hook_for_a_rejected_advance() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let outcome = run_advance_when_parked_then(
        &clock,
        Some(until),
        None,
        Duration::from_secs(31),
        Deadline::after(Duration::from_secs(5)),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to fail")
        .expect_err("an overshoot never runs the hook");
    assert!(
        message.contains("would pass the named instant"),
        "the failure names its kind: {message}"
    );
    assert_eq!(
        hooks.load(Ordering::SeqCst),
        0,
        "a rejected advance never runs the hook"
    );
    assert_eq!(clock.now(), now, "a rejected advance moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_then_never_runs_the_hook_for_a_named_park_at_now() {
    let clock = FakeClock::new();
    let now = clock.now();
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let outcome = run_advance_when_parked_then(
        &clock,
        Some(now),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(5)),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to fail")
        .expect_err("a named park at now never runs the hook");
    assert!(
        message.contains("already passed"),
        "the failure names its kind: {message}"
    );
    assert_eq!(
        hooks.load(Ordering::SeqCst),
        0,
        "a rejected wait never runs the hook"
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
}

#[test]
fn advance_when_parked_then_never_runs_the_hook_for_another_drivers_move() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let outcome = run_advance_when_parked_then(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&waiting)
        .expect("the helper waits for the park");
    // The pause holds the state lock, so the release goes first.
    release_waiter.send(()).expect("released the wait");
    let before = clock.now();
    clock.advance_marked(Duration::from_millis(1));
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to fail")
        .expect_err("another driver's move never runs the hook");
    assert!(
        message.contains("time moved during the wait by another driver"),
        "the failure names its kind: {message}"
    );
    assert_eq!(
        hooks.load(Ordering::SeqCst),
        0,
        "another driver's move never runs the hook"
    );
    assert_eq!(
        clock.now(),
        before + Duration::from_millis(1),
        "only the other driver moves time"
    );
}

#[test]
fn advance_when_parked_then_hook_panic_leaves_time_unmoved() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let step = Duration::from_millis(5);
    let (wake_tx, wake_rx) = mpsc::channel();
    let wake: Arc<dyn Wake> = Arc::new(ChanWake(wake_tx));
    clock.subscribe(Arc::downgrade(&wake));
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let wall = clock.wall();
    let outcome = run_advance_when_parked_then(
        &clock,
        Some(until),
        None,
        step,
        Deadline::after(Duration::from_secs(5)),
        || panic!("hook failed"),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to fail")
        .expect_err("a panicking hook fails the wait");
    assert!(
        message.contains("hook failed"),
        "the helper panics with the hook's payload: {message}"
    );
    assert_eq!(clock.now(), now, "a panicking hook moves no time");
    assert_eq!(clock.wall(), wall, "a panicking hook moves no time");
    assert!(wake_rx.try_recv().is_err(), "a panicking hook wakes nobody");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn advance_when_parked_then_runs_the_hook_for_the_named_thread_among_several_parks() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let elsewhere = clock.origin() + Duration::from_secs(31);
    let step = Duration::from_millis(5);
    let (parked_other, release_other, exit_other, _) = park_once(&clock, Some(elsewhere));
    let (parked_named, release_named, exit_named, named_id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_other)
        .expect("waited for the other thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv(&parked_named)
        .expect("waited for the named thread to park");
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let outcome = run_advance_when_parked_then(
        &clock,
        Some(until),
        Some(named_id),
        step,
        Deadline::after(Duration::from_secs(5)),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to advance")
        .expect("the named thread advances among several parks");
    assert_eq!(hooks.load(Ordering::SeqCst), 1, "the hook runs once");
    assert_eq!(
        clock.now(),
        clock.origin() + step,
        "time moves by exactly the advance"
    );
    release_other.send(()).unwrap();
    release_named.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_other)
        .expect("the other thread exits");
    Deadline::after(Duration::from_secs(5))
        .recv(&exit_named)
        .expect("the named thread exits");

    let clock = FakeClock::new();
    let (parked, release, exit, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the unbounded park");
    let hooks = Arc::new(AtomicUsize::new(0));
    let hooks_t = Arc::clone(&hooks);
    let outcome = run_advance_when_parked_then(
        &clock,
        None,
        None,
        step,
        Deadline::after(Duration::from_secs(5)),
        move || {
            hooks_t.fetch_add(1, Ordering::SeqCst);
        },
    );
    Deadline::after(Duration::from_secs(5))
        .recv(&outcome)
        .expect("waited for advance_when_parked_then to advance")
        .expect("an unbounded park advances");
    assert_eq!(
        hooks.load(Ordering::SeqCst),
        1,
        "an unbounded park runs the hook once"
    );
    assert_eq!(
        clock.now(),
        clock.origin() + step,
        "time moves by exactly the advance"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn try_advance_when_parked_advances_a_named_thread_to_a_park_already_present() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let now = clock.now();
    let step = Duration::from_millis(5);
    let outcome =
        run_try_advance_when_parked(&clock, Some(until), Some(id), step, Duration::from_secs(30));
    let mark = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the soft advance")
        .expect("the worker never panics on a matching park")
        .expect("a park already present is Ok");
    assert!(
        mark.threads().contains(&id),
        "the mark holds the named thread"
    );
    assert_eq!(clock.now(), now + step, "time moves by exactly the advance");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_advances_without_a_named_thread() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let now = clock.now();
    let step = Duration::from_millis(5);
    let outcome =
        run_try_advance_when_parked(&clock, Some(until), None, step, Duration::from_secs(30));
    let mark = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the soft advance")
        .expect("the worker never panics on a matching park")
        .expect("a park already present is Ok");
    assert!(
        mark.threads().contains(&id),
        "the mark holds the parked thread"
    );
    assert_eq!(clock.now(), now + step, "time moves by exactly the advance");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_advances_to_a_park_that_arrives_during_the_pause() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    // The pause proves the helper is waiting before the park arrives: it
    // fires only with no match or mismatch yet.
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&waiting, "the helper waiting for a park");
    release_waiter.send(()).unwrap();
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the soft advance")
        .expect("the worker never panics on a matching park")
        .expect("a park that arrives during the pause is Ok");
    assert_eq!(
        clock.now(),
        clock.origin() + Duration::from_millis(5),
        "time moves by exactly the advance"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_matches_a_park_present_when_the_pause_expires() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    // The held pass freezes the helper before its decision, while its pause
    // keeps running: the park below arrives before the pause expires.
    let (entered, resume) = clock.pause_next_advance_pass();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&entered, "the helper's pass pause");
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    // A receive longer than the pause expires it while the pass is still
    // held, so the decision below runs with no time left and a park held.
    let held = Deadline::after(Duration::from_millis(500)).recv(&outcome);
    assert!(
        matches!(held, Err(mpsc::RecvTimeoutError::Timeout)),
        "nothing answers while the pass is held"
    );
    resume.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the expired-pause advance")
        .expect("the worker never panics on a matching park")
        .expect("a park present at expiry still matches");
    assert_eq!(
        clock.now(),
        clock.origin() + Duration::from_millis(5),
        "time moves by exactly the advance"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_reports_no_park_when_nothing_parks() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let now = clock.now();
    let wall = clock.wall();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );
    let miss = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the NoPark report")
        .expect("the worker never panics on an empty wait");
    assert_eq!(
        miss.expect_err("no park within the pause is a Miss"),
        Miss::NoPark,
        "an empty wait reports NoPark"
    );
    assert_eq!(clock.now(), now, "a Miss moves no time");
    assert_eq!(clock.wall(), wall, "a Miss moves no time");
}

#[test]
fn try_advance_when_parked_ignores_a_stale_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    // The held park predates this move, so it is stale: it neither matches
    // nor mismatches.
    clock.advance(Duration::from_millis(1));
    let now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );
    let miss = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the stale-park report")
        .expect("the worker never panics on a stale park");
    assert_eq!(
        miss.expect_err("a stale park is a Miss"),
        Miss::NoPark,
        "a stale park reports NoPark"
    );
    assert_eq!(clock.now(), now, "a Miss moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_reports_a_mismatch_at_another_instant() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (parked, release, exit, id) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let now = clock.now();
    let wall = clock.wall();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    let miss = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the Mismatch report")
        .expect("the worker never panics on a wrong park");
    assert_eq!(
        miss.expect_err("a park at another instant is a Miss"),
        Miss::Mismatch {
            watched: (id, Some(wrong)),
            parked: vec![(id, Some(wrong))],
        },
        "the Mismatch names the watched park and every held park"
    );
    assert_eq!(clock.now(), now, "a Miss moves no time");
    assert_eq!(clock.wall(), wall, "a Miss moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_reports_a_mismatch_for_an_unbounded_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    let miss = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the Mismatch report")
        .expect("the worker never panics on an unbounded park");
    assert_eq!(
        miss.expect_err("an unbounded park where a finite instant was named is a Miss"),
        Miss::Mismatch {
            watched: (id, None),
            parked: vec![(id, None)],
        },
        "the Mismatch names the unbounded watched park"
    );
    assert_eq!(clock.now(), now, "a Miss moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_reports_a_mismatch_where_none_was_named() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        None,
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    let miss = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the Mismatch report")
        .expect("the worker never panics on a finite park");
    assert_eq!(
        miss.expect_err("a finite park where None was named is a Miss"),
        Miss::Mismatch {
            watched: (id, Some(until)),
            parked: vec![(id, Some(until))],
        },
        "the Mismatch names the finite watched park"
    );
    assert_eq!(clock.now(), now, "a Miss moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_prefers_a_mismatch_beside_the_right_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (wrong_parked, release_wrong, exit_wrong, wrong_id) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&wrong_parked, "the wrong thread to park");
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the right thread to park");
    let now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    let miss = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the Mismatch report")
        .expect("the worker never panics on a wrong park");
    assert_eq!(
        miss.expect_err("a wrong park beside the right one is a Miss"),
        Miss::Mismatch {
            watched: (wrong_id, Some(wrong)),
            parked: vec![(wrong_id, Some(wrong)), (id, Some(until))],
        },
        "a convenient later park never hides the first wrong one"
    );
    assert_eq!(clock.now(), now, "a Miss moves no time");
    release_wrong.send(()).unwrap();
    release.send(()).unwrap();
    let exited = Deadline::after(Duration::from_secs(5));
    exited.recv_or_fail(&exit_wrong, "the wrong thread's exit");
    exited.recv_or_fail(&exit, "the right thread's exit");
}

#[test]
fn try_advance_when_parked_with_a_named_thread_ignores_another_threads_wrong_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (parked, release, exit, id) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the named thread to park");
    let (other_parked, release_other, exit_other, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&other_parked, "the other thread to park");
    let now = clock.now();
    let step = Duration::from_millis(5);
    let outcome =
        run_try_advance_when_parked(&clock, Some(until), Some(id), step, Duration::from_secs(30));
    let mark = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the soft advance")
        .expect("the worker never panics on an unwatched wrong park")
        .expect("another thread's wrong park is not a Mismatch");
    assert!(
        mark.threads().contains(&id),
        "the mark holds the named thread"
    );
    assert_eq!(clock.now(), now + step, "time moves by exactly the advance");
    release.send(()).unwrap();
    release_other.send(()).unwrap();
    let exited = Deadline::after(Duration::from_secs(5));
    exited.recv_or_fail(&exit, "the named thread's exit");
    exited.recv_or_fail(&exit_other, "the other thread's exit");
}

#[test]
fn try_advance_when_parked_reports_a_mismatch_from_the_log_after_the_park_left() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    // The held pass freezes the helper before its decision, so the wrong
    // park arrives and leaves while the helper is frozen.
    let (entered, resume) = clock.pause_next_advance_pass();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&entered, "the helper's pass pause");
    let (parked, release, exit, wrong_id) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the wrong thread to park");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the wrong thread's exit");
    assert!(
        clock.parked().is_empty(),
        "the mismatching park left before the helper looked"
    );
    let now = clock.now();
    resume.send(()).unwrap();
    let miss = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the Mismatch report")
        .expect("the worker never panics on a logged wrong park");
    assert_eq!(
        miss.expect_err("a departed wrong park is still a Miss"),
        Miss::Mismatch {
            watched: (wrong_id, Some(wrong)),
            parked: vec![],
        },
        "the log names the departed park; nothing is held"
    );
    assert_eq!(clock.now(), now, "a Miss moves no time");
}

#[test]
fn miss_display_names_threads_instants_and_the_pause() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, a) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked_a, "the first thread to park");
    let (parked_b, release_b, exit_b, b) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked_b, "the second thread to park");
    let mismatch = Miss::Mismatch {
        watched: (a, Some(until)),
        parked: vec![(a, Some(until)), (b, None)],
    };
    let text = format!("{mismatch}");
    assert!(
        text.contains(&format!("watched park {a:?}")),
        "the Mismatch names the watched park: {text}"
    );
    assert!(
        text.contains(&format!("{a:?}")),
        "the Mismatch names the first parked thread: {text}"
    );
    assert!(
        text.contains(&format!("{b:?}")),
        "the Mismatch names the second parked thread: {text}"
    );
    assert!(
        text.contains("at Some("),
        "the Mismatch reports the finite instant: {text}"
    );
    assert!(
        text.contains("at None"),
        "the Mismatch reports the unbounded park: {text}"
    );
    let no_park = format!("{}", Miss::NoPark);
    assert!(
        no_park.contains("pause"),
        "NoPark names the pause it waited: {no_park}"
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    let exited = Deadline::after(Duration::from_secs(5));
    exited.recv_or_fail(&exit_a, "the first thread's exit");
    exited.recv_or_fail(&exit_b, "the second thread's exit");
}

/// Three distinct thread ids: this thread, then two that have exited. Joining
/// a finished thread waits on no clock and no timing.
fn three_thread_ids() -> (thread::ThreadId, thread::ThreadId, thread::ThreadId) {
    let first = thread::current().id();
    let second = thread::spawn(|| thread::current().id()).join().unwrap();
    let third = thread::spawn(|| thread::current().id()).join().unwrap();
    (first, second, third)
}

#[test]
fn mismatch_display_with_no_parked_thread_prints_empty_brackets() {
    let (watched, _, _) = three_thread_ids();
    let mismatch = Miss::Mismatch {
        watched: (watched, None),
        parked: vec![],
    };
    assert_eq!(
        format!("{mismatch}"),
        format!("watched park {watched:?} at None; parked threads: []"),
    );
}

#[test]
fn mismatch_display_with_one_parked_thread_writes_no_separator() {
    let (watched, _, _) = three_thread_ids();
    let mismatch = Miss::Mismatch {
        watched: (watched, None),
        parked: vec![(watched, None)],
    };
    assert_eq!(
        format!("{mismatch}"),
        format!("watched park {watched:?} at None; parked threads: [{watched:?} at None]"),
    );
}

#[test]
fn mismatch_display_with_two_parked_threads_separates_them_with_one_comma() {
    let (watched, second, _) = three_thread_ids();
    let mismatch = Miss::Mismatch {
        watched: (watched, None),
        parked: vec![(watched, None), (second, None)],
    };
    assert_eq!(
        format!("{mismatch}"),
        format!(
            "watched park {watched:?} at None; parked threads: [{watched:?} at None, {second:?} at None]"
        ),
    );
}

#[test]
fn try_advance_when_parked_leaves_no_active_wait_after_ok() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the soft advance")
        .expect("the worker never panics on a matching park")
        .expect("a park already present is Ok");
    assert!(
        !super::lock(&clock.state).watching,
        "the soft wait stops watching on Ok"
    );
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
    // A strict wait still proceeds: no wait is left active.
    let later = clock.now() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(later));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the later thread to park");
    let strict = run_advance_when_parked(
        &clock,
        Some(later),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&strict, "the strict advance after Ok")
        .expect("the clock still advances after a soft Ok");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the later thread's exit");
}

#[test]
fn try_advance_when_parked_leaves_no_active_wait_after_no_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the NoPark report")
        .expect("the worker never panics on an empty wait")
        .expect_err("no park within the pause is a Miss");
    assert!(
        !super::lock(&clock.state).watching,
        "the soft wait stops watching on NoPark"
    );
    // A strict wait still proceeds: no wait is left active.
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let strict = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&strict, "the strict advance after NoPark")
        .expect("the clock still advances after a soft NoPark");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_leaves_no_active_wait_after_mismatch() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let wrong = until + Duration::from_secs(1);
    let (wrong_parked, release_wrong, exit_wrong, _) = park_once(&clock, Some(wrong));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&wrong_parked, "the wrong thread to park");
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the Mismatch report")
        .expect("the worker never panics on a wrong park")
        .expect_err("a park at another instant is a Miss");
    assert!(
        !super::lock(&clock.state).watching,
        "the soft wait stops watching on Mismatch"
    );
    release_wrong.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit_wrong, "the wrong thread's exit");
    // A strict wait still proceeds: no wait is left active.
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let strict = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&strict, "the strict advance after Mismatch")
        .expect("the clock still advances after a soft Mismatch");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_rejects_a_named_park_at_or_before_now() {
    // At now: the named park already passed before the wait starts.
    let clock = FakeClock::new();
    let at_now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(at_now),
        None,
        Duration::from_millis(1),
        Duration::from_secs(30),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the at-now rejection")
        .expect_err("the soft wait rejects a named park at now");
    assert!(
        message.contains("already passed"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), at_now, "a rejected wait moves no time");
    // Before now: a move after the named instant fails the same way.
    let clock = FakeClock::new();
    clock.advance(Duration::from_millis(5));
    let past = clock.origin();
    let now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(past),
        None,
        Duration::from_millis(1),
        Duration::from_secs(30),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the before-now rejection")
        .expect_err("the soft wait rejects a named park before now");
    assert!(
        message.contains("already passed"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
}

#[test]
fn try_advance_when_parked_rejects_an_advance_past_the_named_park() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        until.duration_since(now) + Duration::from_secs(1),
        Duration::from_secs(30),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the overshoot rejection")
        .expect_err("the soft wait rejects an advance past the named park");
    assert!(
        message.contains("the advance would pass the named instant"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_rejects_an_advance_the_clock_cannot_represent() {
    let clock = FakeClock::new();
    let (parked, release, exit, _) = park_once(&clock, None);
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let now = clock.now();
    let wall = clock.wall();
    let outcome =
        run_try_advance_when_parked(&clock, None, None, Duration::MAX, Duration::from_secs(30));
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the overflowing rejection")
        .expect_err("the soft wait rejects an overflowing advance");
    let overflow = if clock.origin().checked_add(Duration::MAX).is_none() {
        "monotonic instant"
    } else {
        "wall time"
    };
    assert!(
        message.contains(&format!("overflows the clock's {overflow}")),
        "the failure names the overflowing clock: {message}"
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
    assert_eq!(clock.wall(), wall, "a rejected wait moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_rejects_a_second_wait_while_one_is_active() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    // The pause proves the first wait is waiting before the soft wait
    // starts, so the soft wait overlaps it rather than following it.
    let (waiting, release_waiter) = clock.pause_next_advance_wait();
    let first = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&waiting, "the first wait's condvar wait");
    // Release the first waiter before the soft wait starts: the pause holds
    // the state lock, which the soft wait needs to see the active wait.
    release_waiter.send(()).unwrap();
    let second = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&second, "the soft wait's rejection")
        .expect_err("a soft wait overlapping an active wait is rejected");
    assert!(
        message.contains("already active"),
        "the rejection names the overlap: {message}"
    );
    assert!(
        super::lock(&clock.state).watching,
        "the rejected soft wait leaves the first wait's log alone"
    );
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&first, "the first wait's advance")
        .expect("the first wait still advances after the overlap");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn another_drivers_move_fails_a_soft_advance_wait() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (checked, resume) = clock.pause_next_advance_wait();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(1),
        Duration::from_secs(30),
    );
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&checked, "the soft wait's condvar wait");
    // The pause holds the state lock, so release it before reading now().
    resume.send(()).unwrap();
    let before = clock.now();
    clock.advance_marked(Duration::from_millis(1));
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the other-driver failure")
        .expect_err("another driver's move still fails a soft wait");
    assert!(
        message.contains("time moved during the wait by another driver"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), before + Duration::from_millis(1));
    assert!(!super::lock(&clock.state).watching);
}

#[test]
fn try_advance_when_parked_rejects_several_threads_with_no_thread_named() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked_a, release_a, exit_a, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked_a, "the first thread to park");
    let (parked_b, release_b, exit_b, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked_b, "the second thread to park");
    let now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::from_secs(30),
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the several-threads rejection")
        .expect_err("the soft wait rejects several unnamed threads");
    assert!(
        message.contains("several threads hold a park"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    let exited = Deadline::after(Duration::from_secs(5));
    exited.recv_or_fail(&exit_a, "the first thread's exit");
    exited.recv_or_fail(&exit_b, "the second thread's exit");
}

#[test]
fn try_advance_when_parked_rejects_a_zero_pause() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let now = clock.now();
    let outcome = run_try_advance_when_parked(
        &clock,
        Some(until),
        None,
        Duration::from_millis(5),
        Duration::ZERO,
    );
    let message = Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&outcome, "the zero-pause rejection")
        .expect_err("a pause of zero never waits");
    assert!(
        message.contains("pause"),
        "the failure names the pause: {message}"
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
    assert!(
        !super::lock(&clock.state).watching,
        "the rejected wait never registered"
    );
    // The rejected wait never registered: a later wait still proceeds.
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&parked, "the thread to park");
    let step = Duration::from_millis(5);
    let strict = run_advance_when_parked(
        &clock,
        Some(until),
        None,
        step,
        Deadline::after(Duration::from_secs(30)),
    );
    Deadline::after(Duration::from_secs(5))
        .recv_or_fail(&strict, "the strict advance after a zero pause")
        .expect("the clock still advances after a rejected pause");
    assert_eq!(clock.now(), now + step, "time moves by exactly the advance");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5)).recv_or_fail(&exit, "the parked thread's exit");
}

#[test]
fn try_advance_when_parked_names_the_caller_on_a_loud_failure() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let (parked, release, exit, _) = park_once(&clock, Some(until));
    Deadline::after(Duration::from_secs(5))
        .recv(&parked)
        .expect("waited for the thread to park");
    let now = clock.now();
    let step = until.duration_since(now) + Duration::from_secs(1);
    let watch = HookWatch::install();
    let line = line!() + 3;
    let run = || {
        clock
            .try_advance_when_parked(Some(until), None, step, Duration::from_secs(30))
            .expect_err("the soft wait fails the overshoot loudly");
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    let locations = watch.locations();
    drop(watch);
    let message = outcome
        .expect_err("try_advance_when_parked fails the overshoot")
        .downcast::<String>()
        .expect("the helper panics with a message");
    assert_caller(&locations, line);
    assert!(
        message.contains("the advance would pass the named instant"),
        "the failure names its kind: {message}"
    );
    assert_eq!(clock.now(), now, "a loud failure moves no time");
    release.send(()).unwrap();
    Deadline::after(Duration::from_secs(5))
        .recv(&exit)
        .expect("the parked thread exits");
}

#[test]
fn try_advance_when_parked_names_the_caller_on_a_zero_pause() {
    let clock = FakeClock::new();
    let until = clock.origin() + Duration::from_secs(30);
    let now = clock.now();
    let watch = HookWatch::install();
    let line = line!() + 3;
    let run = || {
        clock
            .try_advance_when_parked(Some(until), None, Duration::from_millis(5), Duration::ZERO)
            .expect_err("the soft wait rejects a zero pause loudly");
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    let locations = watch.locations();
    drop(watch);
    let message = outcome
        .expect_err("try_advance_when_parked rejects a zero pause")
        .downcast::<String>()
        .expect("the helper panics with a message");
    assert_caller(&locations, line);
    assert!(
        message.contains("pause"),
        "the failure names the pause: {message}"
    );
    assert_eq!(clock.now(), now, "a rejected wait moves no time");
}
