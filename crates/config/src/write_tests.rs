use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;
use fakes::TempDir;
use serde_json::json;

use super::atomic::{locked, waiting};
use super::lines::remove_line;
use super::*;

/// How long the test waits for a thread before failing.
const DEADLINE: Duration = Duration::from_secs(10);

#[track_caller]
fn wait_until(what: &str, pred: impl Fn() -> bool + Send + 'static) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        while !pred() {
            thread::yield_now();
        }
        done.send(()).unwrap();
    });
    assert!(
        Deadline::after(DEADLINE).recv(&finished).is_ok(),
        "waited {DEADLINE:?} for {what}"
    );
}

/// Runs `f`, which may block, on a worker and returns its result,
/// failing the test after [`DEADLINE`] with `what` named.
#[track_caller]
fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(f());
    });
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(value) => value,
        Err(_) => panic!("waited {DEADLINE:?} for {what}"),
    }
}

#[test]
fn a_second_update_waits_for_the_files_lock_then_keeps_both_writes() {
    let dir = TempDir::new("fiber-write-lock");
    let file = dir.path().join("fiber-acme.json");
    let setup = file.clone();
    within("the first update", move || {
        update(&setup, &["a".to_owned()], Value::from(1), false)
    })
    .unwrap();
    let setup = file.clone();
    let held = within("the test to take the file's lock", move || locked(&setup)).unwrap();
    let (started, started_rx) = mpsc::channel();
    let (done, done_rx) = mpsc::channel();
    let worker_file = file.clone();
    let worker = thread::spawn(move || {
        started.send(()).unwrap();
        update(&worker_file, &["b".to_owned()], Value::from(2), false).unwrap();
        done.send(()).unwrap();
    });
    assert!(
        Deadline::after(DEADLINE).recv(&started_rx).is_ok(),
        "waited {DEADLINE:?} for the second update to reach the file's lock"
    );
    wait_until("the second update to be waiting on the file's lock", || {
        waiting() == 1
    });
    assert!(
        done_rx.try_recv().is_err(),
        "the second update finished while the file was locked"
    );
    // Another session's write lands while the second update waits:
    // the lock serializes whole-file writes, it does not hide them.
    fs::write(&file, "{\"a\": 1, \"c\": 3}\n").unwrap();
    drop(held);
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "waited {DEADLINE:?} for the second update to finish after the release"
    );
    worker.join().unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, json!({"a": 1, "b": 2, "c": 3}));
}

#[test]
fn a_remove_waits_for_the_files_lock_then_keeps_the_other_write() {
    let dir = TempDir::new("fiber-write-lock");
    let file = dir.path().join("rules");
    fs::write(&file, "{\"gone\": 1}\n").unwrap();
    let setup = file.clone();
    let held = within("the test to take the file's lock", move || locked(&setup)).unwrap();
    let (started, started_rx) = mpsc::channel();
    let (done, done_rx) = mpsc::channel();
    let worker_file = file.clone();
    let worker = thread::spawn(move || {
        started.send(()).unwrap();
        let removed = remove_line(&worker_file, 1, "{\"gone\": 1}").unwrap();
        done.send(removed).unwrap();
    });
    assert!(
        Deadline::after(DEADLINE).recv(&started_rx).is_ok(),
        "waited {DEADLINE:?} for the remove to reach the file's lock"
    );
    wait_until("the remove to be waiting on the file's lock", || {
        waiting() == 1
    });
    assert!(
        done_rx.try_recv().is_err(),
        "the remove finished while the file was locked"
    );
    // Another session's line lands while the remove waits: the lock
    // serializes whole-file writes, it does not hide them.
    fs::write(&file, "{\"gone\": 1}\n{\"late\": 3}\n").unwrap();
    drop(held);
    let removed = match Deadline::after(DEADLINE).recv(&done_rx) {
        Ok(removed) => removed,
        Err(_) => panic!("waited {DEADLINE:?} for the remove to finish after the release"),
    };
    worker.join().unwrap();
    assert!(removed, "the held line is still there");
    assert_eq!(fs::read(&file).unwrap(), "{\"late\": 3}\n".as_bytes());
}
