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
    let locks = Arc::new(PathLocks::new());
    let (done, done_rx) = mpsc::channel();
    let worker = Arc::clone(&locks);
    let handle = thread::spawn(move || {
        let path = std::path::Path::new("/ws/a.txt");
        let guard = worker.lock(path);
        drop(guard);
        let again = worker.lock(path);
        drop(again);
        done.send(()).unwrap();
    });
    assert!(
        done_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the lock, release and relock to run"
    );
    handle.join().unwrap();
    assert!(locks.is_clear());
}

#[test]
fn two_paths_do_not_block_each_other() {
    let locks = Arc::new(PathLocks::new());
    let first = Arc::clone(&locks);
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        let guard = first.lock(std::path::Path::new("/ws/a.txt"));
        entered.send(()).unwrap();
        release_rx
            .recv()
            .expect("the test releases the first path");
        drop(guard);
    });
    assert!(
        entered_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the first path to be held"
    );
    let other = Arc::clone(&locks);
    let (done, finished) = mpsc::channel();
    let handle = thread::spawn(move || {
        let guard = other.lock(std::path::Path::new("/ws/b.txt"));
        done.send(()).unwrap();
        drop(guard);
    });
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for a lock on a different path"
    );
    release.send(()).unwrap();
    holder.join().unwrap();
    handle.join().unwrap();
    assert!(locks.is_clear());
}

#[test]
fn a_second_lock_on_a_held_path_blocks_until_the_guard_drops() {
    let locks = Arc::new(PathLocks::new());
    let path = std::path::PathBuf::from("/ws/a.txt");
    let first = Arc::clone(&locks);
    let first_path = path.clone();
    let (holding, holding_rx) = mpsc::channel();
    let (release_first, release_first_rx) = mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        let guard = first.lock(&first_path);
        holding.send(()).unwrap();
        release_first_rx
            .recv()
            .expect("the test releases the held path");
        drop(guard);
    });
    assert!(
        holding_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the first lock to be held"
    );
    let waiting = Arc::clone(&locks);
    let path_for_wait = path.clone();
    let (entered, entered_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
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
    release_first.send(()).unwrap();
    assert!(
        entered_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the second lock to acquire the path"
    );
    holder.join().unwrap();
    handle.join().unwrap();
    assert!(locks.is_clear());
}

#[test]
fn no_entry_remains_after_release() {
    let locks = Arc::new(PathLocks::new());
    let (done, done_rx) = mpsc::channel();
    let worker = Arc::clone(&locks);
    let handle = thread::spawn(move || {
        let guard = worker.lock(std::path::Path::new("/ws/a.txt"));
        drop(guard);
        done.send(()).unwrap();
    });
    assert!(
        done_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the lock and release to run"
    );
    handle.join().unwrap();
    assert!(locks.is_clear());
}

#[test]
fn a_dyn_hold_blocks_a_second_hold_on_the_same_path() {
    use contract::files::PathLock;

    let locks = Arc::new(PathLocks::new());
    let path = std::path::PathBuf::from("/ws/dyn-a.txt");
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let first = Arc::clone(&locks);
    let first_path = path.clone();
    let handle = thread::spawn(move || {
        let lock: &dyn PathLock = &*first;
        lock.hold(&first_path, &mut || {
            entered.send(()).unwrap();
            release_rx
                .recv()
                .expect("the test releases the first hold");
        });
    });
    assert!(
        entered_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the first hold to run"
    );
    let second = Arc::clone(&locks);
    let (done, done_rx) = mpsc::channel();
    let second_handle = thread::spawn(move || {
        let lock: &dyn PathLock = &*second;
        lock.hold(&path, &mut || {
            done.send(()).unwrap();
        });
    });
    let probe = Arc::clone(&locks);
    wait_until("a second hold to be waiting", move || probe.waiting() == 1);
    assert!(
        done_rx.try_recv().is_err(),
        "the second hold ran while the first held the path"
    );
    release.send(()).unwrap();
    handle.join().unwrap();
    assert!(
        done_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the second hold to run"
    );
    second_handle.join().unwrap();
    assert!(locks.is_clear());
}

#[test]
fn a_dyn_hold_does_not_block_a_different_path() {
    use contract::files::PathLock;

    let locks = Arc::new(PathLocks::new());
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let first = Arc::clone(&locks);
    let handle = thread::spawn(move || {
        let lock: &dyn PathLock = &*first;
        lock.hold(std::path::Path::new("/ws/dyn-a.txt"), &mut || {
            entered.send(()).unwrap();
            release_rx
                .recv()
                .expect("the test releases the first hold");
        });
    });
    assert!(
        entered_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the first hold to run"
    );
    let second = Arc::clone(&locks);
    let (done, done_rx) = mpsc::channel();
    let second_handle = thread::spawn(move || {
        let lock: &dyn PathLock = &*second;
        lock.hold(std::path::Path::new("/ws/dyn-b.txt"), &mut || {
            done.send(()).unwrap();
        });
    });
    assert!(
        done_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the hold on a different path to run"
    );
    release.send(()).unwrap();
    handle.join().unwrap();
    second_handle.join().unwrap();
    assert!(locks.is_clear());
}

#[test]
fn an_alias_contends_with_the_built_in_key() {
    use contract::files::PathLock;

    use super::super::resolve;

    let dir = fakes::TempDir::new("fiber-locks-alias");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("file.txt"), "hi").unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
    // The key the file tools lock, reached through a symlinked parent
    // and through a `..`: both aliases resolve to it.
    let key = resolve(dir.path(), "link/file.txt").unwrap();
    assert_eq!(
        key,
        resolve(dir.path(), "real/file.txt").unwrap(),
        "the symlinked parent resolves to the built-in key"
    );
    assert_eq!(
        key,
        resolve(dir.path(), "real/../real/file.txt").unwrap(),
        "the `..` resolves to the built-in key"
    );
    let locks = Arc::new(PathLocks::new());
    for alias in [
        dir.path().join("link/file.txt"),
        dir.path().join("real/../real/file.txt"),
    ] {
        let first = Arc::clone(&locks);
        let key_for_hold = key.clone();
        let (holding, holding_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            let guard = first.lock(&key_for_hold);
            holding.send(()).unwrap();
            release_rx
                .recv()
                .expect("the test releases the built-in key");
            drop(guard);
        });
        assert!(
            holding_rx.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for the built-in key to be held"
        );
        let waiting = Arc::clone(&locks);
        let (done, done_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let lock: &dyn PathLock = &*waiting;
            lock.hold(&alias, &mut || {
                done.send(()).unwrap();
            });
        });
        let probe = Arc::clone(&locks);
        wait_until("a hold through an alias to be waiting", move || {
            probe.waiting() == 1
        });
        assert!(
            done_rx.try_recv().is_err(),
            "the hold through the alias ran while the built-in key was held"
        );
        release.send(()).unwrap();
        assert!(
            done_rx.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for the hold through the alias to acquire the key"
        );
        holder.join().unwrap();
        handle.join().unwrap();
    }
    assert!(locks.is_clear());
}

#[test]
fn hold_all_dedupes_paths_that_resolve_to_one_key() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use contract::files::PathLock;

    let dir = fakes::TempDir::new("fiber-locks-hold-all-alias");
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("file.txt"), "hi").unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
    // Each pair resolves to one effective key. Without the dedupe in
    // `PathLocks::hold_all` the second lock would wait on the first,
    // held by the same thread, and miss the deadline below.
    for pair in [
        vec![dir.path().join("sub/../a.txt"), dir.path().join("a.txt")],
        vec![
            dir.path().join("link/file.txt"),
            dir.path().join("real/file.txt"),
        ],
    ] {
        let locks = Arc::new(PathLocks::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let worker_locks = Arc::clone(&locks);
        let worker_runs = Arc::clone(&runs);
        let (done, done_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let lock: &dyn PathLock = &*worker_locks;
            lock.hold_all(&pair, &mut || {
                worker_runs.fetch_add(1, Ordering::SeqCst);
            });
            done.send(()).unwrap();
        });
        assert!(
            done_rx.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for hold_all on an alias pair to run"
        );
        handle.join().unwrap();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "hold_all on an alias pair ran once"
        );
        assert!(locks.is_clear());
    }
}
