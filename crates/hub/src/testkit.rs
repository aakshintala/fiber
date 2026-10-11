//! The hub tests' shared setup, beside [`fake`][crate::fake]: the `id`
//! and `args` builders, the temporary home builders, the hub and feed
//! builders, the `await_true` and `stop_within` waits, and the fixture log
//! writer. Each test file keeps its own `Temp` with its scenario methods;
//! only the copied bodies live here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value, json};

use crate::Starter;
use crate::connection::Hub;
use crate::feed::{Feed, RUN_SCAN};

/// The wait in [`await_true`] and [`stop_within`].
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// Session `n` as an id.
pub(crate) fn id(n: u64) -> String {
    format!("s_{n:016x}")
}

/// `value`'s object as command arguments.
pub(crate) fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

/// A hub home under a held temporary directory: `<tmp>/h`.
pub(crate) fn home(prefix: &str) -> (fakes::TempDir, PathBuf) {
    let held = fakes::TempDir::new(prefix);
    let dir = held.path().join("h");
    fs::create_dir_all(&dir).unwrap();
    (held, dir)
}

/// The same, plus `h/run` for tests that bind session sockets.
pub(crate) fn with_run(prefix: &str) -> (fakes::TempDir, PathBuf) {
    let (held, dir) = home(prefix);
    fs::create_dir_all(dir.join("run")).unwrap();
    (held, dir)
}

/// A hub in `dir` starting sessions through `starter`, on `clock`.
pub(crate) fn hub(dir: &Path, starter: Arc<dyn Starter>, clock: Arc<dyn Clock>) -> Hub {
    Hub::new(
        dir,
        "0.0.0",
        starter,
        Arc::clone(&clock),
        crate::diag::open(dir, clock),
    )
}

/// A feed over `dir` on a fake clock, not yet started.
pub(crate) fn new_feed(dir: &Path) -> (Arc<Feed>, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    (Arc::new(Feed::new(dir, timed)), clock)
}

/// Waits until the scanner parks for the scan after now.
pub(crate) fn await_scanner(clock: &FakeClock) {
    assert!(
        clock.await_parked(clock.now() + RUN_SCAN, DEADLINE),
        "the scanner parks until the next scan"
    );
}

/// Starts `feed`, and waits until its scanner parks for the next scan.
pub(crate) fn start(feed: &Arc<Feed>, clock: &FakeClock) {
    feed.start();
    await_scanner(clock);
}

/// Waits under [`DEADLINE`] until `done` holds, naming `what` on expiry.
#[track_caller]
pub(crate) fn await_true(what: &str, done: impl Fn() -> bool + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    thread::spawn(move || {
        while !done() {
            if cancel_rx.try_recv().is_ok() {
                return;
            }
            thread::yield_now();
        }
        tx.send(()).unwrap_or(());
    });
    if Deadline::after(DEADLINE).recv(&rx).is_err() {
        cancel_tx.send(()).unwrap_or(());
        panic!("waited for {what}");
    }
}

/// Stops `feed` under [`DEADLINE`]: a stop that hangs fails the test.
#[track_caller]
pub(crate) fn stop_within(feed: &Arc<Feed>) {
    let (tx, rx) = mpsc::channel();
    let stopping = Arc::clone(feed);
    thread::spawn(move || {
        stopping.stop();
        tx.send(()).unwrap_or(());
    });
    assert!(Deadline::after(DEADLINE).recv(&rx).is_ok(), "stop returns");
}

/// Writes `id`'s log: a `session_started` recording `workspace` and
/// `parent` when given, then each of `last` in order.
pub(crate) fn write_log(
    home: &Path,
    id: &str,
    workspace: &str,
    parent: Option<&str>,
    last: &[Value],
) {
    let dir = home.join("projects").join("-w").join("sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut text = format!("{}\n", started_line(id, workspace, parent));
    for line in last {
        text.push_str(&format!("{}\n", serde_json::to_string(line).unwrap()));
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

/// The `session_started` line [`write_log`] starts each fixture log with.
pub(crate) fn started_line(id: &str, workspace: &str, parent: Option<&str>) -> String {
    let mut payload = json!({"workspace": workspace});
    if let Some(parent) = parent {
        payload["parent"] = Value::String(parent.to_owned());
    }
    serde_json::to_string(&json!({
        "kind": "session_started", "session_id": id, "ts": 1, "schema_version": 1,
        "payload": payload,
    }))
    .unwrap()
}
