//! `log.append` and `log.fsync` timing: one line per actual write and
//! sync, stamped at completion and emitted after the session lock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::Event;
use contract::{ErrorCode, SessionId};
use fakes::clock::FakeClock;
use fakes::deadline::Deadline;
use serde_json::{Value, json};

use super::{Log, after_sync, before_emit, before_sync};
use crate::diag::{Diag, Level, Process};
use crate::fixtures::event;

/// The deadline for every blocking wait below: a bug fails the test
/// instead of hanging it.
const WITHIN: Duration = Duration::from_secs(30);

/// Neutral hooks, replacing any a previous test on this thread left
/// behind. Each test calls it first, so hooks never leak between tests.
fn reset_hooks() {
    before_sync(|| {});
    after_sync(|| None);
    before_emit(|| {});
}

fn unsynced() -> Event {
    event("step_started", json!({}))
}

fn synced() -> Event {
    event("assistant_message_started", json!({}))
}

fn ephemeral() -> Event {
    event("assistant_message_delta", json!({"text": "x"}))
}

/// A log in a fresh directory with a debug writer installed: the writer's
/// home is the same directory, so its file is `<dir>/logs/hub.log`. The
/// log is shared so `'static` hooks can hold it.
fn timed(name: &str) -> (fakes::TempDir, Arc<FakeClock>, Arc<Log>) {
    let dir = fakes::TempDir::new(name);
    let clock = FakeClock::new();
    let diag = Arc::new(Diag::new(
        dir.path(),
        Process::Hub,
        Level::Debug,
        Arc::clone(&clock) as Arc<dyn Clock>,
    ));
    let log = Log::create(
        dir.path(),
        SessionId("s_1".into()),
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .unwrap();
    log.install_timing(diag);
    (dir, clock, Arc::new(log))
}

/// The `op_timing` lines the installed writer has written.
fn op_lines(dir: &fakes::TempDir) -> Vec<Value> {
    let path = dir.path().join("logs").join("hub.log");
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Asserts one line carries exactly `op` and `ms` in its data.
fn assert_closed(line: &Value, op: &str) {
    assert_eq!(line["code"], "op_timing", "{line}");
    assert_eq!(line["level"], "debug", "{line}");
    let data = line["data"].as_object().expect("data is an object");
    let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["ms", "op"], "closed data keys: {line}");
    assert_eq!(data["op"], op);
    assert!(data["ms"].is_u64(), "{line}");
}

fn ops(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .map(|line| line["data"]["op"].as_str().unwrap())
        .collect()
}

#[test]
fn unsynced_synced_and_ephemeral_write_the_expected_lines() {
    reset_hooks();
    let (dir, _clock, log) = timed("log-write-lines");
    log.append(&unsynced(), None, None).unwrap();
    log.append(&synced(), None, None).unwrap();
    log.append(&ephemeral(), None, None).unwrap();
    let lines = op_lines(&dir);
    assert_eq!(ops(&lines), ["log.append", "log.append", "log.fsync"]);
    assert_closed(&lines[0], "log.append");
    assert_closed(&lines[1], "log.append");
    assert_closed(&lines[2], "log.fsync");
    reset_hooks();
}

#[test]
fn timers_stamp_at_each_call_s_completion() {
    reset_hooks();
    // 5 ms outside the fsync scope, 7 ms inside it, 50 ms after both
    // stopped.
    let (dir, clock, log) = timed("log-write-stamps");
    let advancing = Arc::clone(&clock);
    before_sync(move || {
        // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
        advancing.advance(Duration::from_millis(5));
    });
    let advancing = Arc::clone(&clock);
    after_sync(move || {
        // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
        advancing.advance(Duration::from_millis(7));
        None
    });
    let advancing = Arc::clone(&clock);
    before_emit(move || {
        // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
        advancing.advance(Duration::from_millis(50));
    });
    log.append(&synced(), None, None).unwrap();
    let lines = op_lines(&dir);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_closed(&lines[0], "log.append");
    assert_closed(&lines[1], "log.fsync");
    // The write took no fake time; the sync took the 7 ms after it.
    // Without either stop the 50 ms would show here.
    assert_eq!(lines[0]["data"]["ms"], 0, "{lines:?}");
    assert_eq!(lines[1]["data"]["ms"], 7, "{lines:?}");
    reset_hooks();
}

#[test]
fn nothing_is_emitted_under_the_lock() {
    reset_hooks();
    let (dir, _clock, log) = timed("log-write-under-lock");
    let checking = Arc::clone(&log);
    let path = dir.path().join("logs").join("hub.log");
    before_emit(move || {
        // The timers drop after the guard: the lock is free and the
        // file holds no line for this append yet.
        assert!(!checking.held(), "emission holds the session lock");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(text.is_empty(), "a line was written under the lock");
    });
    log.append(&unsynced(), None, None).unwrap();
    let lines = op_lines(&dir);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_closed(&lines[0], "log.append");
    reset_hooks();
}

#[test]
fn a_failed_write_poisons_before_emission() {
    reset_hooks();
    let (dir, _clock, log) = timed("log-write-fails");
    let mut watcher = log.watch();
    log.break_writes();
    let checking = Arc::clone(&log);
    before_emit(move || {
        // Poisoning precedes emission: a nested append already refuses.
        let nested = checking.append(&unsynced(), None, None);
        assert!(
            matches!(&nested, Err(crate::Error::Poisoned { .. })),
            "{nested:?}"
        );
    });
    let failed = log.append(&unsynced(), None, None);
    assert!(
        matches!(&failed, Err(crate::Error::Io { .. })),
        "{failed:?}"
    );
    assert_eq!(failed.unwrap_err().code(), ErrorCode::IoFailed);
    let lines = op_lines(&dir);
    assert_eq!(ops(&lines), ["log.append"], "{lines:?}");
    // A later append refuses and writes no line.
    let later = log.append(&unsynced(), None, None);
    assert!(
        matches!(&later, Err(crate::Error::Poisoned { .. })),
        "{later:?}"
    );
    assert_eq!(op_lines(&dir).len(), 1);
    // The watcher registered before the failure ends with it, waited on
    // a thread under the one deadline.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(
            watcher
                .recv()
                .map(|line| line.is_some())
                .map_err(|e| e.code()),
        )
        .unwrap();
    });
    let ended = Deadline::after(WITHIN)
        .recv(&rx)
        .expect("the watcher to end with the failure");
    assert_eq!(ended, Err(ErrorCode::IoFailed));
    reset_hooks();
}

#[test]
fn the_emit_hook_runs_once() {
    reset_hooks();
    // A nested durable append on a healthy log: one line per actual
    // write, outer and nested, each once.
    let (dir, _clock, log) = timed("log-write-hook-once");
    let runs = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&runs);
    let nested = Arc::clone(&log);
    before_emit(move || {
        counting.fetch_add(1, AtomicOrdering::SeqCst);
        nested.append(&unsynced(), None, None).unwrap();
    });
    log.append(&unsynced(), None, None).unwrap();
    assert_eq!(runs.load(AtomicOrdering::SeqCst), 1);
    let lines = op_lines(&dir);
    assert_eq!(ops(&lines), ["log.append", "log.append"], "{lines:?}");
    reset_hooks();

    // A poisoned append after `break_writes`: the failed write only,
    // none for the poisoned call, and the hook ran once.
    let (dir, _clock, log) = timed("log-write-hook-poisoned");
    log.break_writes();
    let runs = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&runs);
    let nested = Arc::clone(&log);
    before_emit(move || {
        counting.fetch_add(1, AtomicOrdering::SeqCst);
        let failed = nested.append(&unsynced(), None, None);
        assert!(
            matches!(&failed, Err(crate::Error::Poisoned { .. })),
            "{failed:?}"
        );
    });
    let failed = log.append(&unsynced(), None, None);
    assert!(
        matches!(&failed, Err(crate::Error::Io { .. })),
        "{failed:?}"
    );
    assert_eq!(runs.load(AtomicOrdering::SeqCst), 1);
    let lines = op_lines(&dir);
    assert_eq!(ops(&lines), ["log.append"], "{lines:?}");
    reset_hooks();
}

#[test]
fn a_failed_sync_poisons_and_counts_the_attempt() {
    reset_hooks();
    let (dir, _clock, log) = timed("log-write-sync-fails");
    let base = log.fsyncs();
    after_sync(|| Some(std::io::Error::other("no sync")));
    let failed = log.append(&synced(), None, None);
    assert!(
        matches!(&failed, Err(crate::Error::Io { .. })),
        "{failed:?}"
    );
    assert_eq!(failed.unwrap_err().code(), ErrorCode::IoFailed);
    let lines = op_lines(&dir);
    assert_eq!(ops(&lines), ["log.append", "log.fsync"], "{lines:?}");
    // The attempt counts as today, and the log is poisoned.
    assert_eq!(log.fsyncs(), base + 1);
    let later = log.append(&unsynced(), None, None);
    assert!(
        matches!(&later, Err(crate::Error::Poisoned { .. })),
        "{later:?}"
    );
    reset_hooks();
}

/// The short-write child: does nothing without its env var, so the suite
/// passes it by. The parent below runs it under a file-size limit.
#[test]
fn child_oversized_write_reports_io() {
    let Ok(sessions) = std::env::var("LOG_TEST_TIMING_SESSIONS") else {
        return;
    };
    reset_hooks();
    let clock = FakeClock::new();
    let diag = Arc::new(Diag::new(
        Path::new(&sessions),
        Process::Hub,
        Level::Debug,
        Arc::clone(&clock) as Arc<dyn Clock>,
    ));
    let log = Log::open(
        Path::new(&sessions),
        SessionId("s_1".into()),
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .unwrap();
    log.install_timing(diag);
    log.append(&unsynced(), None, None).unwrap();
    log.append(&unsynced(), None, None).unwrap();
    let big = event(
        "session_named",
        json!({"name": "n".repeat(4 * 1024 * 1024), "by": "person"}),
    );
    let failed = log.append(&big, None, None);
    assert!(
        matches!(&failed, Err(crate::Error::Io { .. })),
        "{failed:?}"
    );
}

#[test]
fn a_short_write_emits_its_append_and_no_fsync() {
    reset_hooks();
    let dir = fakes::TempDir::new("log-write-short");
    let log = Log::create(
        dir.path(),
        SessionId("s_1".into()),
        FakeClock::new() as Arc<dyn Clock>,
    )
    .unwrap();
    log.append(&unsynced(), None, None).unwrap();
    drop(log);
    let executable = std::env::current_exe().unwrap();
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"trap '' XFSZ; ulimit -f 2048; exec "$0" "$@""#)
        .arg(&executable)
        .args([
            "--exact",
            "write::timing_tests::child_oversized_write_reports_io",
            "--nocapture",
        ])
        .env("LOG_TEST_TIMING_SESSIONS", dir.path())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    // A watchdog kills the group when the test fails or times out, even
    // when destructors never run; the wait below reaps the child once.
    let watchdog = fakes::Watchdog::group(group);
    // The exit status arrives on a channel under the one deadline.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(child.wait()).unwrap();
    });
    let status = match Deadline::after(WITHIN).recv(&rx) {
        Ok(status) => status.unwrap(),
        Err(_) => {
            drop(fakes::kill_group(group, "KILL"));
            let reaped = Deadline::after(WITHIN).recv(&rx).is_ok();
            panic!("waited {WITHIN:?} for the short-write child to exit (reaped: {reaped})");
        }
    };
    assert!(status.success(), "{status:?}");
    drop(fakes::kill_group(group, "KILL"));
    assert!(
        fakes::group_empties(group, fakes::Deadline::after(WITHIN)),
        "the short-write child left its process group {group} alive"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
    // Two small appends, then exactly one append for the oversized
    // write and no fsync for it.
    let lines = op_lines(&dir);
    assert_eq!(
        ops(&lines),
        ["log.append", "log.append", "log.append"],
        "{lines:?}"
    );
    for line in &lines {
        assert_closed(line, "log.append");
    }
    reset_hooks();
}

#[test]
fn timing_changes_no_durable_byte_and_no_fsync_count() {
    reset_hooks();
    let plain_dir = fakes::TempDir::new("log-write-plain");
    let plain = Log::create(
        plain_dir.path(),
        SessionId("s_1".into()),
        FakeClock::new() as Arc<dyn Clock>,
    )
    .unwrap();
    let (timed_dir, _clock, timed) = timed("log-write-timed");
    drop(_clock);
    // Fixed wall time on both clocks, so the bytes must match exactly.
    let events = [unsynced(), unsynced(), synced(), unsynced()];
    for event in &events {
        plain.append(event, None, None).unwrap();
        timed.append(event, None, None).unwrap();
    }
    let plain_bytes = std::fs::read(plain_dir.path().join("s_1").join("events.jsonl")).unwrap();
    let timed_bytes = std::fs::read(timed_dir.path().join("s_1").join("events.jsonl")).unwrap();
    assert_eq!(plain_bytes, timed_bytes);
    assert_eq!(plain.fsyncs(), timed.fsyncs());
    reset_hooks();
}

#[test]
fn untimed_and_info_logs_write_no_diagnostic_file() {
    reset_hooks();
    let dir = fakes::TempDir::new("log-write-untimed");
    let plain = Log::create(
        dir.path(),
        SessionId("s_1".into()),
        FakeClock::new() as Arc<dyn Clock>,
    )
    .unwrap();
    plain.append(&synced(), None, None).unwrap();
    assert!(!dir.path().join("logs").exists());
    reset_hooks();
}

#[test]
fn an_info_install_writes_no_diagnostic_file() {
    reset_hooks();
    let dir = fakes::TempDir::new("log-write-info");
    let clock = FakeClock::new();
    let diag = Arc::new(Diag::new(
        dir.path(),
        Process::Hub,
        Level::Info,
        Arc::clone(&clock) as Arc<dyn Clock>,
    ));
    let log = Log::create(
        dir.path(),
        SessionId("s_1".into()),
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .unwrap();
    log.install_timing(diag);
    log.append(&synced(), None, None).unwrap();
    assert!(!dir.path().join("logs").exists());
    reset_hooks();
}

#[test]
fn the_first_install_wins() {
    reset_hooks();
    let dir = fakes::TempDir::new("log-write-first-wins");
    let elsewhere = fakes::TempDir::new("log-write-second");
    let clock = FakeClock::new();
    let first = Arc::new(Diag::new(
        dir.path(),
        Process::Hub,
        Level::Debug,
        Arc::clone(&clock) as Arc<dyn Clock>,
    ));
    let second = Arc::new(Diag::new(
        elsewhere.path(),
        Process::Hub,
        Level::Debug,
        Arc::clone(&clock) as Arc<dyn Clock>,
    ));
    let log = Log::create(
        dir.path(),
        SessionId("s_1".into()),
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .unwrap();
    log.install_timing(first);
    log.install_timing(second);
    log.append(&unsynced(), None, None).unwrap();
    assert_eq!(op_lines(&dir).len(), 1);
    assert!(!elsewhere.path().join("logs").exists());
    reset_hooks();
}
