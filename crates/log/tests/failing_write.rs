//! A write that fails part way. The test runs a child under a file-size limit
//! (`ulimit -f`, with SIGXFSZ ignored), so a real `write` stops short at the
//! limit, as a full disk stops it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::*;
use contract::ErrorCode;
use log::{Error, Log, read};
use serde_json::json;

/// Run as a child by `a_failed_write_poisons_the_log_and_reopening_repairs_it`,
/// under a file-size limit, on the session in `LOG_TEST_SESSIONS`. Run on its
/// own it does nothing.
#[test]
fn child_writing_past_a_file_size_limit() {
    let Ok(sessions) = std::env::var("LOG_TEST_SESSIONS") else {
        return;
    };
    let log = Log::open(
        Path::new(&sessions),
        id("s_1"),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let mut watcher = log.watch();
    // The watcher falls far behind before the failure.
    let written: Vec<_> = (0..STEPS)
        .map(|_| {
            log.append(&delta("x"), None, None).unwrap();
            log.append(&empty("step_started"), None, None).unwrap()
        })
        .collect();

    let big = event(
        "session_named",
        json!({"name": "n".repeat(4 * 1024 * 1024), "by": "person"}),
    );
    let first = log.append(&big, None, None).unwrap_err();
    assert!(matches!(first, Error::Io { .. }), "{first:?}");
    assert_eq!(first.code(), ErrorCode::IoFailed);
    assert!(first.to_string().contains("events.jsonl"), "{first}");

    // Every later append refuses, even one that would fit.
    let later = log.append(&empty("step_started"), None, None).unwrap_err();
    assert!(
        matches!(&later, Error::Poisoned { session, .. } if session == "s_1"),
        "{later:?}"
    );
    assert_eq!(later.code(), ErrorCode::IoFailed);
    assert!(log.append(&delta("x"), None, None).is_err());

    // The lagging watcher first gets every durable line that was written,
    // then the failure, rather than waiting for a line it will never get.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut durable = Vec::new();
        let ended = loop {
            match watcher.recv() {
                Ok(Some(line)) if line.is_durable() => durable.push(line),
                Ok(Some(_)) => {}
                Ok(None) => panic!("the watcher ended without the failure"),
                Err(e) => break e,
            }
        };
        tx.send((durable, ended.code())).unwrap();
    });
    let (durable, ended) = rx
        .recv_timeout(DEADLINE)
        .expect("the watcher to end with the failure before the deadline");
    assert_eq!(durable, written);
    assert_eq!(ended, ErrorCode::IoFailed);

    // A watcher made after the failure gets it at once.
    let mut late = log.watch();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(late.recv().map(|l| l.is_some()).map_err(|e| e.code()))
            .unwrap();
    });
    let got = rx
        .recv_timeout(DEADLINE)
        .expect("a watcher of a stopped log to return at once");
    assert_eq!(got, Err(ErrorCode::IoFailed));

    // Dropping the stopped log does not turn its failure into a clean end.
    let mut orphan = log.watch();
    drop(log);
    let ended = orphan
        .recv_timeout(DEADLINE)
        .expect("the failure of a dropped log")
        .unwrap_err();
    assert_eq!(ended.code(), ErrorCode::IoFailed);
}

/// How many durable lines the child writes before its write fails. With an
/// ephemeral line each, more than a watcher's queue holds.
const STEPS: usize = 600;

/// How long the child waits for a watcher.
const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn a_failed_write_poisons_the_log_and_reopening_repairs_it() {
    let tmp = TestDir::new("fail-write");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let first = log.append(&session_started(), None, None).unwrap();
    drop(log);
    let path = tmp.session(&id("s_1")).join("events.jsonl");
    let before = fs::read(&path).unwrap();

    // 2,048 blocks is at least 1 MiB, whatever the shell's block size: room
    // for the small lines, not for the big one.
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"trap '' XFSZ; ulimit -f 2048; exec "$0" "$@""#)
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_writing_past_a_file_size_limit",
            "--nocapture",
        ])
        .env("LOG_TEST_SESSIONS", tmp.path())
        .status()
        .unwrap();
    assert!(status.success());

    // The write stopped short: a partial line follows the complete one.
    let torn = fs::read(&path).unwrap();
    assert!(torn.len() > before.len(), "the child wrote nothing");
    assert!(torn.starts_with(&before));

    let dir = tmp.session(&id("s_1"));
    let complete = read(&dir).unwrap();
    assert_eq!(complete.len(), 1 + STEPS);
    assert_eq!(complete[0], first);
    let log = Log::open(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let next = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(next.seq.map(|s| s.0), Some(1 + STEPS as u64));
    assert_eq!(read(&dir).unwrap().len(), 2 + STEPS);
}
