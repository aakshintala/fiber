use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::PathLocks;

const DEADLINE: Duration = Duration::from_secs(10);

fn wait_until(what: &str, pred: impl Fn() -> bool + Send + 'static) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        while !pred() {
            thread::yield_now();
        }
        done.send(()).unwrap();
    });
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for {what}"
    );
}

#[test]
fn a_guard_releases_on_drop() {
    let locks = PathLocks::new();
    let path = std::path::Path::new("/ws/a.txt");
    let guard = locks.lock(path);
    drop(guard);
    let again = locks.lock(path);
    drop(again);
    assert!(locks.is_clear());
}

#[test]
fn two_paths_do_not_block_each_other() {
    let locks = Arc::new(PathLocks::new());
    let held = locks.lock(std::path::Path::new("/ws/a.txt"));
    let other = Arc::clone(&locks);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let guard = other.lock(std::path::Path::new("/ws/b.txt"));
        done.send(()).unwrap();
        drop(guard);
    });
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for a lock on a different path"
    );
    drop(held);
    assert!(locks.is_clear());
}

#[test]
fn a_second_lock_on_a_held_path_blocks_until_the_guard_drops() {
    let locks = Arc::new(PathLocks::new());
    let path = std::path::PathBuf::from("/ws/a.txt");
    let guard = locks.lock(&path);
    let waiting = Arc::clone(&locks);
    let path_for_wait = path.clone();
    let (entered, entered_rx) = mpsc::channel();
    thread::spawn(move || {
        let guard = waiting.lock(&path_for_wait);
        entered.send(()).unwrap();
        drop(guard);
    });
    let probe = Arc::clone(&locks);
    wait_until("a second lock to be waiting", move || probe.waiting() == 1);
    assert!(
        entered_rx.try_recv().is_err(),
        "the second lock acquired the path while it was held"
    );
    drop(guard);
    assert!(
        entered_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the second lock to acquire the path"
    );
    assert!(locks.is_clear());
}

#[test]
fn no_entry_remains_after_release() {
    let locks = PathLocks::new();
    let guard = locks.lock(std::path::Path::new("/ws/a.txt"));
    drop(guard);
    assert!(locks.is_clear());
}
