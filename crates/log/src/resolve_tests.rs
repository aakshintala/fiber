//! Tests for [`opening_ts`](super::opening_ts): the first complete
//! line's `ts` when it opens a session, `None` otherwise.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;

use serde_json::json;

use super::*;

fn started_line(ts: u64) -> String {
    let line = json!({
        "kind": "session_started", "seq": 0, "session_id": "s_1",
        "ts": ts, "schema_version": 1,
        "payload": {"workspace": "/w",
            "variables": {"path": "", "names": [], "source": "login_shell"}},
    });
    format!("{line}\n")
}

fn other_line(ts: u64) -> String {
    let line = json!({
        "kind": "session_status", "session_id": "s_1",
        "ts": ts, "schema_version": 1, "payload": {},
    });
    format!("{line}\n")
}

#[test]
fn opening_ts_reads_the_first_lines_ts_when_it_opens_a_session() {
    let home = fakes::TempDir::new("log-opening-ts");
    let dir = home.path().join("session");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        format!("{}{}", started_line(7), other_line(9)),
    )
    .unwrap();
    assert_eq!(opening_ts(&dir), Some(7));
}

#[test]
fn opening_ts_is_none_without_a_session_started_first_line() {
    let home = fakes::TempDir::new("log-opening-none");
    let dir = home.path().join("session");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), other_line(9)).unwrap();
    assert_eq!(opening_ts(&dir), None);
}

#[test]
fn opening_ts_is_none_for_a_missing_or_torn_log() {
    let home = fakes::TempDir::new("log-opening-torn");
    let dir = home.path().join("session");
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(opening_ts(&dir), None);
    fs::write(dir.join("events.jsonl"), "{\"kind\": \"sess").unwrap();
    assert_eq!(opening_ts(&dir), None);
}
