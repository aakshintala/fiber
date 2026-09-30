//! A write that fails part way. The test runs a child under a file-size limit
//! (`ulimit -f`, with SIGXFSZ ignored), so a real `write` stops short at the
//! limit, as a full disk stops it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

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
    let log = Log::open(Path::new(&sessions), id("s_1")).unwrap();
    let mut watcher = log.watch();

    let big = event(
        "session_named",
        json!({"name": "n".repeat(64 * 1024), "by": "person"}),
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

    // A watcher is told, rather than left waiting for a line it will never
    // get.
    let ended = watcher.recv().unwrap_err();
    assert!(matches!(ended, Error::Poisoned { .. }), "{ended:?}");
}

#[test]
fn a_failed_write_poisons_the_log_and_reopening_repairs_it() {
    let tmp = TestDir::new("fail-write");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    let first = log.append(&session_started(), None, None).unwrap();
    drop(log);
    let path = tmp.session(&id("s_1")).join("events.jsonl");
    let before = fs::read(&path).unwrap();

    // 16 blocks is at least 8 KiB, whatever the shell's block size.
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"trap '' XFSZ; ulimit -f 16; exec "$0" "$@""#)
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
    assert_eq!(read(&dir).unwrap(), std::slice::from_ref(&first));
    let log = Log::open(tmp.path(), id("s_1")).unwrap();
    let next = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(next.seq.map(|s| s.0), Some(1));
    assert_eq!(read(&dir).unwrap(), [first, next]);
}
