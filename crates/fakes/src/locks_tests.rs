//! Tests of [`NoLock`](super::NoLock) and [`FakeLock`](super::FakeLock)
//! through the public API: the no-op lock runs `run` once, and the fake
//! lock excludes a second holder of the same key while recording the keys
//! it was asked for, sorted with duplicates dropped.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::files::PathLock;

use super::{FakeLock, NoLock};
use crate::deadline::Deadline;

/// How long a test waits for a lock holder or a thread before failing.
const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn no_lock_hold_runs_run_once() {
    let lock = NoLock;
    let mut runs = 0;
    lock.hold(std::path::Path::new("/workspace/a.md"), &mut || {
        runs += 1;
    });
    assert_eq!(runs, 1);
}

#[test]
fn no_lock_hold_all_runs_run_once_for_several_paths() {
    let lock = NoLock;
    let mut runs = 0;
    lock.hold_all(
        &[
            PathBuf::from("/workspace/b.md"),
            PathBuf::from("/workspace/a.md"),
            PathBuf::from("/workspace/b.md"),
        ],
        &mut || {
            runs += 1;
        },
    );
    assert_eq!(runs, 1);
}

#[test]
fn fake_hold_runs_run_and_records_the_key() {
    let lock = FakeLock::new();
    let mut runs = 0;
    lock.hold(std::path::Path::new("/workspace/a.md"), &mut || {
        runs += 1;
    });
    assert_eq!(runs, 1);
    assert_eq!(lock.calls(), [PathBuf::from("/workspace/a.md")]);
}

#[test]
fn fake_hold_all_records_sorted_keys_with_duplicates_dropped() {
    let lock = FakeLock::new();
    let mut runs = 0;
    lock.hold_all(
        &[
            PathBuf::from("/workspace/b.md"),
            PathBuf::from("/workspace/a.md"),
            PathBuf::from("/workspace/b.md"),
        ],
        &mut || {
            runs += 1;
        },
    );
    assert_eq!(runs, 1);
    assert_eq!(
        lock.calls(),
        [
            PathBuf::from("/workspace/a.md"),
            PathBuf::from("/workspace/b.md")
        ]
    );
}

#[test]
fn fake_second_hold_waits_while_the_key_is_held() {
    let lock = Arc::new(FakeLock::new());
    let key = PathBuf::from("/workspace/a.md");
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let holder = Arc::clone(&lock);
    let held_key = key.clone();
    let holder = thread::spawn(move || {
        holder.hold(&held_key, &mut || {
            entered.send(()).unwrap();
            release_rx.recv().expect("the test releases the held key");
        });
    });
    assert!(
        Deadline::after(DEADLINE).recv(&entered_rx).is_ok(),
        "waited {DEADLINE:?} for the holder to be holding the key"
    );
    let (done, done_rx) = mpsc::channel();
    let waiter = Arc::clone(&lock);
    let waiter = thread::spawn(move || {
        waiter.hold(&key, &mut || {});
        done.send(()).unwrap();
    });
    let probe = Arc::clone(&lock);
    let (waiting, waiting_rx) = mpsc::channel();
    let poll_deadline = Deadline::after(DEADLINE);
    let poller = thread::spawn(move || {
        while probe.waiting() != 1 {
            if poll_deadline.left().is_zero() {
                return;
            }
            thread::yield_now();
        }
        waiting.send(()).unwrap();
    });
    assert!(
        Deadline::after(DEADLINE).recv(&waiting_rx).is_ok(),
        "waited {DEADLINE:?} for the second hold to be waiting"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "the second hold ran while the key was held"
    );
    release.send(()).unwrap();
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "waited {DEADLINE:?} for the second hold to run"
    );
    waiter.join().unwrap();
    holder.join().unwrap();
    poller.join().unwrap();
    assert_eq!(lock.waiting(), 0);
}
