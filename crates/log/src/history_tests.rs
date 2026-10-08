//! Tests for the history chain: segments root first, the fold point, and
//! the pointer errors.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::Path;

use contract::Seq;
use serde_json::{Value, json};

use super::*;

fn line(kind: &str, session: &str, seq: u64, payload: Value) -> Value {
    let Value::Object(payload) = payload else {
        panic!("a payload is an object");
    };
    json!({
        "kind": kind,
        "session_id": session,
        "ts": 1_759_150_000_000_u64 + seq,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    })
}

fn started(session: &str, seq: u64, from: Option<(&str, u64)>) -> Value {
    let mut payload = json!({
        "workspace": "/w",
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
    });
    if let Some((parent, at)) = from {
        payload["forked_from"] = json!({"session_id": parent, "seq": at});
    }
    line("session_started", session, seq, payload)
}

fn plain(session: &str, seq: u64, kind: &str) -> Value {
    line(kind, session, seq, json!({}))
}

/// Writes `sessions/id/events.jsonl` holding `lines`, one per line.
fn write_log(sessions: &Path, id: &str, lines: &[Value]) {
    let dir = sessions.join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(&line.to_string());
        text.push('\n');
    }
    fs::write(dir.join(EVENTS), text).unwrap();
}

fn chain_fixture(name: &str) -> (fakes::TempDir, std::path::PathBuf) {
    let home = fakes::TempDir::new(&format!("log-history-{name}"));
    let sessions = home.path().join("sessions");
    // Root A: six lines. B continues A at 2. C continues B at 1.
    write_log(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[
            started("s_aaaaaaaaaaaaaaaa", 0, None),
            plain("s_aaaaaaaaaaaaaaaa", 1, "turn_started"),
            plain("s_aaaaaaaaaaaaaaaa", 2, "turn_started"),
            plain("s_aaaaaaaaaaaaaaaa", 3, "turn_completed"),
            plain("s_aaaaaaaaaaaaaaaa", 4, "turn_started"),
            plain("s_aaaaaaaaaaaaaaaa", 5, "turn_completed"),
        ],
    );
    write_log(
        &sessions,
        "s_bbbbbbbbbbbbbbbb",
        &[
            started("s_bbbbbbbbbbbbbbbb", 0, Some(("s_aaaaaaaaaaaaaaaa", 2))),
            plain("s_bbbbbbbbbbbbbbbb", 1, "turn_started"),
            plain("s_bbbbbbbbbbbbbbbb", 2, "turn_completed"),
        ],
    );
    write_log(
        &sessions,
        "s_cccccccccccccccc",
        &[
            started("s_cccccccccccccccc", 0, Some(("s_bbbbbbbbbbbbbbbb", 1))),
            plain("s_cccccccccccccccc", 1, "turn_started"),
        ],
    );
    let dir = sessions.join("s_cccccccccccccccc");
    (home, dir)
}

fn ids(segments: &[Segment]) -> Vec<&str> {
    segments
        .iter()
        .map(|segment| segment.session_id.0.as_str())
        .collect()
}

fn tos(segments: &[Segment]) -> Vec<Option<u64>> {
    segments
        .iter()
        .map(|segment| segment.to.as_ref().map(|to| to.0))
        .collect()
}

#[test]
fn a_root_alone_is_one_segment_holding_all_of_it() {
    let home = fakes::TempDir::new("log-history-root");
    let sessions = home.path().join("sessions");
    write_log(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[started("s_aaaaaaaaaaaaaaaa", 0, None)],
    );
    let segments = history(&sessions.join("s_aaaaaaaaaaaaaaaa")).unwrap();
    assert_eq!(ids(&segments), ["s_aaaaaaaaaaaaaaaa"]);
    assert_eq!(tos(&segments), [None]);
}

#[test]
fn a_two_link_chain_is_root_first_with_the_fork_as_the_root_to() {
    let (_home, dir) = chain_fixture("two");
    let parent = dir.parent().unwrap().join("s_bbbbbbbbbbbbbbbb");
    let segments = history(&parent).unwrap();
    assert_eq!(ids(&segments), ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"]);
    assert_eq!(tos(&segments), [Some(2), None]);
}

#[test]
fn a_three_link_chain_carries_each_childs_fork_as_its_parent_to() {
    let (_home, dir) = chain_fixture("three");
    let segments = history(&dir).unwrap();
    assert_eq!(
        ids(&segments),
        [
            "s_aaaaaaaaaaaaaaaa",
            "s_bbbbbbbbbbbbbbbb",
            "s_cccccccccccccccc"
        ]
    );
    assert_eq!(tos(&segments), [Some(2), Some(1), None]);
}

#[test]
fn history_to_on_the_middle_gives_two_segments_with_the_point_as_the_last_to() {
    let (_home, dir) = chain_fixture("middle");
    let parent = dir.parent().unwrap().join("s_bbbbbbbbbbbbbbbb");
    let segments = history_to(&parent, Seq(1)).unwrap();
    assert_eq!(ids(&segments), ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"]);
    assert_eq!(tos(&segments), [Some(2), Some(1)]);
}

#[test]
fn history_to_on_the_root_sets_the_only_segments_to() {
    let (_home, dir) = chain_fixture("root-to");
    let root = dir.parent().unwrap().join("s_aaaaaaaaaaaaaaaa");
    let segments = history_to(&root, Seq(4)).unwrap();
    assert_eq!(ids(&segments), ["s_aaaaaaaaaaaaaaaa"]);
    assert_eq!(tos(&segments), [Some(4)]);
}

#[test]
fn a_missing_parent_is_not_found() {
    let home = fakes::TempDir::new("log-history-missing");
    let sessions = home.path().join("sessions");
    write_log(
        &sessions,
        "s_bbbbbbbbbbbbbbbb",
        &[started(
            "s_bbbbbbbbbbbbbbbb",
            0,
            Some(("s_aaaaaaaaaaaaaaaa", 2)),
        )],
    );
    let error = history(&sessions.join("s_bbbbbbbbbbbbbbbb")).unwrap_err();
    assert!(matches!(error, Error::NotFound(_)));
    assert_eq!(error.code(), contract::ErrorCode::SessionNotFound);
}

#[test]
fn a_cycle_is_a_pointer_and_the_walk_stops() {
    let home = fakes::TempDir::new("log-history-cycle");
    let sessions = home.path().join("sessions");
    write_log(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[started(
            "s_aaaaaaaaaaaaaaaa",
            0,
            Some(("s_bbbbbbbbbbbbbbbb", 0)),
        )],
    );
    write_log(
        &sessions,
        "s_bbbbbbbbbbbbbbbb",
        &[started(
            "s_bbbbbbbbbbbbbbbb",
            0,
            Some(("s_aaaaaaaaaaaaaaaa", 0)),
        )],
    );
    let error = history(&sessions.join("s_aaaaaaaaaaaaaaaa")).unwrap_err();
    assert!(matches!(error, Error::Pointer { .. }));
    assert_eq!(error.code(), contract::ErrorCode::LogCorrupt);
}

#[test]
fn a_point_past_the_parents_end_is_a_pointer_from_lines() {
    let home = fakes::TempDir::new("log-history-past-end");
    let sessions = home.path().join("sessions");
    write_log(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[started("s_aaaaaaaaaaaaaaaa", 0, None)],
    );
    write_log(
        &sessions,
        "s_bbbbbbbbbbbbbbbb",
        &[started(
            "s_bbbbbbbbbbbbbbbb",
            0,
            Some(("s_aaaaaaaaaaaaaaaa", 9)),
        )],
    );
    let segments = history(&sessions.join("s_bbbbbbbbbbbbbbbb")).unwrap();
    let error = match segments[0].lines(0) {
        Ok(_) => panic!("a point past the parent's end fails"),
        Err(error) => error,
    };
    assert!(matches!(error, Error::Pointer { .. }));
}

#[test]
fn lines_holds_the_point_and_nothing_past_it() {
    let (_home, dir) = chain_fixture("window");
    let root = dir.parent().unwrap().join("s_aaaaaaaaaaaaaaaa");
    let segments = history_to(&root, Seq(2)).unwrap();
    let kinds: Vec<String> = segments[0]
        .lines(0)
        .unwrap()
        .map(|line| line.unwrap().kind)
        .collect();
    assert_eq!(kinds, ["session_started", "turn_started", "turn_started"]);
}

#[test]
fn lines_from_the_point_holds_one_line_and_past_the_point_holds_none() {
    let (_home, dir) = chain_fixture("boundaries");
    let root = dir.parent().unwrap().join("s_aaaaaaaaaaaaaaaa");
    let segments = history_to(&root, Seq(2)).unwrap();
    let at: Vec<u64> = segments[0]
        .lines(2)
        .unwrap()
        .map(|line| line.unwrap().seq.unwrap().0)
        .collect();
    assert_eq!(at, [2]);
    assert_eq!(segments[0].lines(3).unwrap().count(), 0);
}

#[test]
fn lines_to_the_last_line_holds_the_whole_log() {
    let (_home, dir) = chain_fixture("last");
    let root = dir.parent().unwrap().join("s_aaaaaaaaaaaaaaaa");
    let segments = history(&root).unwrap();
    assert_eq!(segments[0].lines(0).unwrap().count(), 6);
}

#[test]
fn a_first_line_that_is_no_session_started_is_a_pointer() {
    let home = fakes::TempDir::new("log-history-first");
    let sessions = home.path().join("sessions");
    write_log(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[plain("s_aaaaaaaaaaaaaaaa", 0, "turn_started")],
    );
    let error = history(&sessions.join("s_aaaaaaaaaaaaaaaa")).unwrap_err();
    assert!(matches!(error, Error::Pointer { .. }));
}

#[test]
fn last_line_on_a_missing_or_empty_log_is_none() {
    let home = fakes::TempDir::new("log-history-last-missing");
    let sessions = home.path().join("sessions");
    assert!(last_line(&sessions.join("s_aaaaaaaaaaaaaaaa")).is_none());
    let dir = sessions.join("s_bbbbbbbbbbbbbbbb");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(EVENTS), b"").unwrap();
    assert!(last_line(&dir).is_none());
}

#[test]
fn last_line_returns_the_one_line_of_a_one_line_log() {
    let home = fakes::TempDir::new("log-history-last-one");
    let sessions = home.path().join("sessions");
    write_log(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[started("s_aaaaaaaaaaaaaaaa", 0, None)],
    );
    let found = last_line(&sessions.join("s_aaaaaaaaaaaaaaaa")).unwrap();
    assert_eq!(found.kind, "session_started");
    assert_eq!(found.seq.unwrap().0, 0);
}

#[test]
fn last_line_drops_a_torn_tail() {
    let home = fakes::TempDir::new("log-history-last-torn");
    let sessions = home.path().join("sessions");
    write_log(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[
            started("s_aaaaaaaaaaaaaaaa", 0, None),
            plain("s_aaaaaaaaaaaaaaaa", 1, "turn_started"),
        ],
    );
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    let first = serde_json::to_string(&started("s_aaaaaaaaaaaaaaaa", 0, None)).unwrap();
    fs::write(
        dir.join(EVENTS),
        format!("{first}\n{{\"kind\": \"turn_started\", torn"),
    )
    .unwrap();
    // The fixture above holds one complete line and a torn tail.
    let found = last_line(&dir).unwrap();
    assert_eq!(found.kind, "session_started");
}
