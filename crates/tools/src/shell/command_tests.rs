use std::io::{self, ErrorKind, Read};
use std::process::{Child, Command};
use std::sync::{Arc, TryLockError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use fakes::CancelToken;
use fakes::clock::FakeClock;

use super::{
    Inner, Shared, StopKind, already_woken, bump, finish, group_alive, lock, note_eof, park,
    poll_while_occupied, read_output, refused_group, suppress_term,
};

#[test]
fn a_moved_sequence_wakes_without_a_cancel() {
    assert!(already_woken(1, 0, false, false));
}

#[test]
fn a_cancel_wakes_only_while_the_run_is_going() {
    assert!(already_woken(0, 0, true, true));
    assert!(!already_woken(0, 0, false, true));
    assert!(!already_woken(0, 0, true, false));
}

#[test]
fn the_drain_polls_only_while_the_group_may_be_occupied() {
    assert!(poll_while_occupied(false));
    assert!(!poll_while_occupied(true));
}

#[test]
fn a_second_signal_and_an_empty_group_are_not_signalled() {
    assert!(suppress_term(true, false));
    assert!(suppress_term(false, true));
    assert!(!suppress_term(false, false));
}

#[test]
fn bump_advances_the_sequence() {
    let mut inner = Inner {
        reaped: false,
        status: None,
        eof: false,
        output: Vec::new(),
        discard: false,
        seq: 0,
    };
    bump(&mut inner);
    assert_eq!(inner.seq, 1);
}

#[test]
fn an_open_pipe_is_discarded_once_the_run_returns() {
    let shared = Shared::default();
    finish(&shared, None, false, true, false);
    assert!(lock(&shared.inner).discard);
}

#[test]
fn finish_distinguishes_a_held_pipe_from_an_unfinished_stop() {
    let held = finish(&Shared::default(), None, false, true, false);
    assert!(held.held_open);
    assert!(!held.indeterminate);

    let closed = finish(&Shared::default(), None, false, true, true);
    assert!(!closed.held_open);
    assert!(!closed.indeterminate);

    let still_occupied = finish(&Shared::default(), None, false, false, false);
    assert!(!still_occupied.held_open);

    let stopped_open = finish(
        &Shared::default(),
        Some(StopKind::Cancel),
        true,
        true,
        false,
    );
    assert!(stopped_open.indeterminate);
    assert!(!stopped_open.held_open);

    let stopped_occupied = finish(
        &Shared::default(),
        Some(StopKind::Timeout),
        true,
        false,
        true,
    );
    assert!(stopped_occupied.indeterminate);
    assert!(!stopped_occupied.held_open);

    let stopped_clean = finish(
        &Shared::default(),
        Some(StopKind::Timeout),
        true,
        true,
        true,
    );
    assert!(!stopped_clean.indeterminate);
    assert!(!stopped_clean.held_open);
}

#[test]
fn an_interrupted_read_is_retried() {
    let shared = Arc::new(Shared::default());
    let reader = Arc::clone(&shared);
    read_output(
        Scripted {
            steps: vec![
                Err(io::Error::new(ErrorKind::Interrupted, "again")),
                Ok(b"hi".to_vec()),
            ],
        },
        &reader,
    );
    let inner = lock(&shared.inner);
    assert_eq!(inner.output, b"hi");
    assert!(inner.eof);
}

#[test]
fn a_read_error_ends_the_output() {
    let shared = Shared::default();
    read_output(
        Scripted {
            steps: vec![Err(io::Error::other("broken")), Ok(b"later".to_vec())],
        },
        &shared,
    );
    let inner = lock(&shared.inner);
    assert!(
        inner.output.is_empty(),
        "bytes after a read error were kept"
    );
    assert!(inner.eof);
}

struct Scripted {
    steps: Vec<io::Result<Vec<u8>>>,
}

impl Read for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.steps.is_empty() {
            return Ok(0);
        }
        match self.steps.remove(0) {
            Ok(bytes) => {
                let n = bytes.len().min(buf.len());
                if let Some(slot) = buf.get_mut(..n) {
                    slot.copy_from_slice(bytes.get(..n).unwrap_or(&[]));
                }
                Ok(n)
            }
            Err(err) => Err(err),
        }
    }
}

/// Passes `None` to the closure, the bound a fake clock gives when `until`
/// was still ahead at registration. With `wake` set, it first asserts that
/// the caller holds the waiter lock across `wait_until`, so no wake can land
/// between this point and the condvar wait, then delivers a wake from
/// another thread. That thread blocks on the lock until the wait releases
/// it, whenever it runs.
struct BoundlessClock {
    origin: Instant,
    wake: Option<Arc<Shared>>,
}

impl Clock for BoundlessClock {
    fn now(&self) -> Instant {
        self.origin
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _duration: Duration) {}

    fn wait_until(&self, _until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        if let Some(shared) = &self.wake {
            assert!(
                matches!(shared.inner.try_lock(), Err(TryLockError::WouldBlock)),
                "park does not hold the waiter lock across wait_until"
            );
            let shared = Arc::clone(shared);
            thread::spawn(move || shared.wake());
        }
        wait(None);
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

fn assert_park_returns(clock: BoundlessClock, shared: Arc<Shared>, seen: u64) {
    const DEADLINE: Duration = Duration::from_secs(5);
    let cancel = CancelToken::new();
    let (done, finished) = mpsc::channel();
    let origin = clock.origin;
    thread::spawn(move || {
        park(
            &clock,
            &shared,
            &cancel,
            origin.checked_add(Duration::from_secs(2)),
            false,
            false,
            seen,
        );
        done.send(()).unwrap();
    });
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for park to return"
    );
}

#[test]
fn a_wake_that_landed_before_park_is_not_lost() {
    let shared = Arc::new(Shared::default());
    let origin = FakeClock::new().origin();
    shared.wake();
    assert_park_returns(
        BoundlessClock { origin, wake: None },
        Arc::clone(&shared),
        0,
    );
    assert_ne!(lock(&shared.inner).seq, 0);
}

#[test]
fn a_wake_inside_wait_until_is_not_lost() {
    let shared = Arc::new(Shared::default());
    let origin = FakeClock::new().origin();
    assert_park_returns(
        BoundlessClock {
            origin,
            wake: Some(Arc::clone(&shared)),
        },
        Arc::clone(&shared),
        0,
    );
}

/// A process in the test's own group that a stray group signal would kill.
fn sentinel() -> Child {
    Command::new("sleep").arg("30").spawn().unwrap()
}

/// Whether the sentinel is still running; ends it either way.
fn survived(mut sentinel: Child) -> bool {
    let alive = sentinel.try_wait().unwrap().is_none();
    sentinel.kill().unwrap();
    sentinel.wait().unwrap();
    alive
}

#[test]
fn group_zero_and_one_are_refused() {
    assert!(refused_group(0));
    assert!(refused_group(1));
    assert!(!refused_group(2));
}

/// Only the probe runs here. A test never hands 0 or 1 to `signal_group`:
/// a mutant of its guard would then send that signal to every process the
/// user owns.
#[test]
fn group_zero_and_one_are_never_occupied() {
    for group in [0, 1] {
        let sentinel = sentinel();
        assert!(!group_alive(group), "group {group} looked occupied");
        assert!(
            survived(sentinel),
            "group {group} probe touched the sentinel"
        );
    }
}

#[test]
fn note_eof_is_idempotent() {
    let shared = Shared::default();
    note_eof(&shared);
    let seq = lock(&shared.inner).seq;
    note_eof(&shared);
    assert_eq!(lock(&shared.inner).seq, seq);
    assert!(lock(&shared.inner).eof);
}
