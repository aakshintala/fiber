//! Tests for the history chain: segments root first, the fold point, and
//! the pointer errors.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;

use contract::Seq;
use serde_json::{Value, json};

use super::*;
use crate::fixtures::{corrupt, line as fixture_line, session_log};

fn started(session: &str, seq: u64, from: Option<(&str, u64)>) -> String {
    let mut payload = json!({
        "workspace": "/w",
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
    });
    if let Some((parent, at)) = from {
        payload["forked_from"] = json!({"session_id": parent, "seq": at});
    }
    fixture_line(
        "session_started",
        session,
        1_759_150_000_000_u64 + seq,
        seq,
        &payload,
    )
}

fn started_in(session: &str, seq: u64, parent: &str, at: u64, project: Option<&str>) -> String {
    let mut payload = json!({
        "workspace": "/w",
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
    });
    let mut from = json!({"session_id": parent, "seq": at});
    if let Some(project) = project {
        from["project"] = Value::String(project.to_owned());
    }
    payload["forked_from"] = from;
    fixture_line(
        "session_started",
        session,
        1_759_150_000_000_u64 + seq,
        seq,
        &payload,
    )
}

fn project_dir(home: &fakes::TempDir, key: &str, id: &str) -> std::path::PathBuf {
    crate::session_dir(home.path(), key, &SessionId(id.into()))
}

fn plain(session: &str, seq: u64, kind: &str) -> String {
    fixture_line(kind, session, 1_759_150_000_000_u64 + seq, seq, &json!({}))
}

fn write_lines(sessions: &std::path::Path, id: &str, lines: &[String]) {
    session_log(&sessions.join(id), lines.concat().as_bytes());
}

fn chain_fixture(name: &str) -> (fakes::TempDir, std::path::PathBuf) {
    let home = fakes::TempDir::new(&format!("log-history-{name}"));
    let sessions = home.path().join("sessions");
    // Root A: six lines. B continues A at 2. C continues B at 1.
    write_lines(
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
    write_lines(
        &sessions,
        "s_bbbbbbbbbbbbbbbb",
        &[
            started("s_bbbbbbbbbbbbbbbb", 0, Some(("s_aaaaaaaaaaaaaaaa", 2))),
            plain("s_bbbbbbbbbbbbbbbb", 1, "turn_started"),
            plain("s_bbbbbbbbbbbbbbbb", 2, "turn_completed"),
        ],
    );
    write_lines(
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
    write_lines(
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
    write_lines(
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
    write_lines(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[started(
            "s_aaaaaaaaaaaaaaaa",
            0,
            Some(("s_bbbbbbbbbbbbbbbb", 0)),
        )],
    );
    write_lines(
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
    write_lines(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[started("s_aaaaaaaaaaaaaaaa", 0, None)],
    );
    write_lines(
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
fn range_caps_the_window_at_the_point() {
    let (_home, dir) = chain_fixture("range");
    let segments = history(&dir).unwrap();
    let root = &segments[0];
    for (from, max, expected) in [
        (1, 1, vec![1]),
        (0, 10, vec![0, 1, 2]),
        (2, 10, vec![2]),
        (3, 10, vec![]),
        (0, 0, vec![]),
    ] {
        let lines = root.range(from, max).unwrap();
        assert_eq!(
            lines
                .iter()
                .map(|line| line.seq.as_ref().unwrap().0)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn range_parses_only_the_window_and_stops_scanning_at_the_point() {
    let (_home, dir) = chain_fixture("range-corrupt");
    let segments = history(&dir).unwrap();
    let root = &segments[0];
    corrupt(&root.dir, 1);
    corrupt(&root.dir, 3);
    let lines = root.range(2, 1).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].seq, Some(Seq(2)));
    assert!(root.range(1, 1).is_err());
}

#[test]
fn range_refuses_a_short_log_unless_from_is_past_the_point() {
    let (_home, dir) = chain_fixture("range-short");
    let mut segments = history(&dir).unwrap();
    let root = &mut segments[0];
    root.to = Some(Seq(6));
    for from in [0, 5, 6] {
        let error = root.range(from, 1).unwrap_err();
        assert!(matches!(error, Error::Pointer { .. }));
        assert!(error.to_string().contains("s_aaaaaaaaaaaaaaaa"));
    }
    assert!(root.range(7, 1).unwrap().is_empty());
    root.to = Some(Seq(5));
    assert_eq!(root.range(5, 1).unwrap()[0].seq, Some(Seq(5)));
}

#[test]
fn lines_holds_the_point_and_nothing_past_it() {
    let (_home, dir) = chain_fixture("window");
    let root = dir.parent().unwrap().join("s_aaaaaaaaaaaaaaaa");
    let segments = history_to(&root, Seq(2)).unwrap();
    let kinds: Vec<String> = segments[0]
        .lines(0)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
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
        .into_iter()
        .map(|line| line.seq.unwrap().0)
        .collect();
    assert_eq!(at, [2]);
    assert!(segments[0].lines(3).unwrap().is_empty());
}

#[test]
fn lines_to_the_last_line_holds_the_whole_log() {
    let (_home, dir) = chain_fixture("last");
    let root = dir.parent().unwrap().join("s_aaaaaaaaaaaaaaaa");
    let segments = history(&root).unwrap();
    assert_eq!(segments[0].lines(0).unwrap().len(), 6);
}

#[test]
fn a_first_line_that_is_no_session_started_is_a_pointer() {
    let home = fakes::TempDir::new("log-history-first");
    let sessions = home.path().join("sessions");
    write_lines(
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
    write_lines(
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
    write_lines(
        &sessions,
        "s_aaaaaaaaaaaaaaaa",
        &[
            started("s_aaaaaaaaaaaaaaaa", 0, None),
            plain("s_aaaaaaaaaaaaaaaa", 1, "turn_started"),
        ],
    );
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    let first = started("s_aaaaaaaaaaaaaaaa", 0, None);
    fs::write(
        dir.join(EVENTS),
        format!("{first}{{\"kind\": \"turn_started\", torn"),
    )
    .unwrap();
    // The fixture above holds one complete line and a torn tail.
    let found = last_line(&dir).unwrap();
    assert_eq!(found.kind, "session_started");
}

#[test]
fn a_forked_from_that_names_no_point_is_a_pointer() {
    // The pointer reads as a session and a `seq`, or not at all: a
    // `forked_from` without either is corrupt, not absent.
    let home = fakes::TempDir::new("log-history-bad-fork");
    let sessions = home.path().join("sessions");
    let mut without_seq = json!({
        "kind": "session_started",
        "session_id": "s_aaaaaaaaaaaaaaaa",
        "ts": 1_759_150_000_000_u64,
        "schema_version": 1,
        "seq": 0,
        "payload": {
            "workspace": "/w",
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
        },
    });
    without_seq["payload"]["forked_from"] = json!({"session_id": "s_bbbbbbbbbbbbbbbb"});
    session_log(
        &sessions.join("s_aaaaaaaaaaaaaaaa"),
        format!("{without_seq}\n").as_bytes(),
    );
    let error = history(&sessions.join("s_aaaaaaaaaaaaaaaa")).unwrap_err();
    assert!(matches!(error, Error::Pointer { .. }));
    assert_eq!(error.code(), contract::ErrorCode::LogCorrupt);
    let mut not_an_object = json!({
        "kind": "session_started",
        "session_id": "s_bbbbbbbbbbbbbbbb",
        "ts": 1_759_150_000_000_u64,
        "schema_version": 1,
        "seq": 0,
        "payload": {
            "workspace": "/w",
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
        },
    });
    not_an_object["payload"]["forked_from"] = json!("s_aaaaaaaaaaaaaaaa");
    session_log(
        &sessions.join("s_bbbbbbbbbbbbbbbb"),
        format!("{not_an_object}\n").as_bytes(),
    );
    let error = history(&sessions.join("s_bbbbbbbbbbbbbbbb")).unwrap_err();
    assert!(matches!(error, Error::Pointer { .. }));
}

#[test]
fn lines_past_the_point_are_never_parsed() {
    // Only the window a consumer needs is parsed: a corrupt line past
    // the point fails nothing.
    let home = fakes::TempDir::new("log-history-past-point");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for line in [
        started("s_aaaaaaaaaaaaaaaa", 0, None),
        plain("s_aaaaaaaaaaaaaaaa", 1, "turn_started"),
    ] {
        text.push_str(&line);
    }
    text.push_str("{\"kind\": \"turn_started\", broken\n");
    fs::write(dir.join(EVENTS), text).unwrap();
    let segments = history_to(&dir, Seq(1)).unwrap();
    let kinds: Vec<String> = segments[0]
        .lines(0)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert_eq!(kinds, ["session_started", "turn_started"]);
}

#[test]
fn a_garbage_line_right_past_the_point_is_never_read() {
    // The point's own line is the last one read: a garbage line at
    // `to + 1` still returns the lines from `from` to `to` with no error.
    let home = fakes::TempDir::new("log-history-garbage");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for line in [
        started("s_aaaaaaaaaaaaaaaa", 0, None),
        plain("s_aaaaaaaaaaaaaaaa", 1, "turn_started"),
        plain("s_aaaaaaaaaaaaaaaa", 2, "turn_completed"),
    ] {
        text.push_str(&line);
    }
    text.push_str("{\"kind\": \"turn_started\", broken\n");
    fs::write(dir.join(EVENTS), text).unwrap();
    let segments = history_to(&dir, Seq(2)).unwrap();
    let held: Vec<u64> = segments[0]
        .lines(1)
        .unwrap()
        .into_iter()
        .map(|line| line.seq.unwrap().0)
        .collect();
    assert_eq!(held, [1, 2]);
}

#[test]
fn a_torn_tail_is_dropped_even_when_its_bytes_parse() {
    // The last line has no newline, so it is torn and is not a line, even
    // though its bytes are a whole event.
    let home = fakes::TempDir::new("log-history-torn-parses");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    let mut text = String::new();
    text.push_str(&started(id, 0, None));
    text.push_str(&plain(id, 1, "turn_started"));
    text.push_str(plain(id, 2, "turn_completed").trim_end());
    session_log(&sessions.join(id), text.as_bytes());
    let segments = history(&sessions.join(id)).unwrap();
    let held: Vec<u64> = segments[0]
        .lines(0)
        .unwrap()
        .into_iter()
        .map(|line| line.seq.unwrap().0)
        .collect();
    assert_eq!(held, [0, 1]);
}

#[test]
fn an_empty_log_holds_no_lines() {
    let home = fakes::TempDir::new("log-history-empty");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    session_log(&sessions.join(id), b"");
    let segment = Segment {
        session_id: SessionId(id.to_owned()),
        dir: sessions.join(id),
        to: None,
    };
    assert!(segment.lines(0).unwrap().is_empty());
}

#[test]
fn lines_of_a_session_with_no_log_is_not_found() {
    let home = fakes::TempDir::new("log-history-missing-log");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    let segment = Segment {
        session_id: SessionId(id.to_owned()),
        dir: sessions.join(id),
        to: None,
    };
    let error = match segment.lines(0) {
        Ok(_) => panic!("a missing log is not found"),
        Err(error) => error,
    };
    assert!(matches!(error, Error::NotFound(_)));
}

#[test]
fn a_refused_open_that_is_not_a_missing_log_is_io() {
    // The session path is a file, so opening the log under it fails for a
    // reason other than a missing log.
    let home = fakes::TempDir::new("log-history-refused-open");
    let sessions = home.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let id = "s_aaaaaaaaaaaaaaaa";
    fs::write(sessions.join(id), "").unwrap();
    let segment = Segment {
        session_id: SessionId(id.to_owned()),
        dir: sessions.join(id),
        to: None,
    };
    let error = match segment.lines(0) {
        Ok(_) => panic!("a refused open fails"),
        Err(error) => error,
    };
    assert!(matches!(error, Error::Io { .. }));
}

#[test]
fn an_unreadable_line_reports_its_own_one_based_number() {
    let home = fakes::TempDir::new("log-history-unreadable-number");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    let mut text = String::new();
    text.push_str(&started(id, 0, None));
    text.push_str(&plain(id, 1, "turn_started"));
    text.push_str("not an event\n");
    session_log(&sessions.join(id), text.as_bytes());
    let segments = history(&sessions.join(id)).unwrap();
    let line = match segments[0].lines(0) {
        Ok(_) => panic!("a garbage line is unreadable"),
        Err(Error::Unreadable { line, .. }) => line,
        Err(other) => panic!("expected Unreadable, got {other}"),
    };
    assert_eq!(line, 3);
}

#[test]
fn a_read_from_n_parses_no_line_below_n() {
    // Only the window from `from` is parsed: a corrupt line below `from`
    // fails nothing, while a corrupt line at `from` names its own number.
    let home = fakes::TempDir::new("log-history-from-n");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    write_lines(
        &sessions,
        id,
        &[
            started(id, 0, None),
            plain(id, 1, "turn_started"),
            plain(id, 2, "turn_started"),
            plain(id, 3, "turn_completed"),
            plain(id, 4, "turn_completed"),
        ],
    );
    let dir = sessions.join(id);
    corrupt(&dir, 1);
    let segments = history_to(&dir, Seq(4)).unwrap();
    let held: Vec<u64> = segments[0]
        .lines(2)
        .unwrap()
        .into_iter()
        .map(|line| line.seq.unwrap().0)
        .collect();
    assert_eq!(held, [2, 3, 4]);
    let line = match segments[0].lines(1) {
        Ok(_) => panic!("a corrupt line at from is unreadable"),
        Err(Error::Unreadable { line, .. }) => line,
        Err(other) => panic!("expected Unreadable, got {other}"),
    };
    assert_eq!(line, 2);
}

fn seq_of(dir: &std::path::Path) -> u64 {
    match last_seq(dir) {
        Ok(Some(seq)) => seq.0,
        other => panic!("expected Ok(Some), got {other:?}"),
    }
}

fn io_error(dir: &std::path::Path) -> String {
    match last_seq(dir) {
        Err(error @ Error::Io { .. }) => error.to_string(),
        other => panic!("expected Io, got {other:?}"),
    }
}

fn pointer_error(dir: &std::path::Path) -> String {
    match last_seq(dir) {
        Err(error @ Error::Pointer { .. }) => error.to_string(),
        other => panic!("expected Pointer, got {other:?}"),
    }
}

#[test]
fn last_seq_returns_the_last_complete_lines_seq() {
    let home = fakes::TempDir::new("log-last-seq-last");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    write_lines(
        &sessions,
        id,
        &[
            started(id, 0, None),
            plain(id, 1, "turn_started"),
            plain(id, 2, "turn_completed"),
        ],
    );
    assert_eq!(seq_of(&sessions.join(id)), 2);
}

#[test]
fn last_seq_drops_a_torn_tail() {
    let home = fakes::TempDir::new("log-last-seq-torn");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    write_lines(
        &sessions,
        id,
        &[started(id, 0, None), plain(id, 1, "turn_started")],
    );
    let dir = sessions.join(id);
    fs::write(
        dir.join(EVENTS),
        format!(
            "{}{}",
            started(id, 0, None),
            "{\"kind\": \"turn_started\", torn"
        ),
    )
    .unwrap();
    // The torn tail is dropped: the boundary is the complete line's seq.
    assert_eq!(seq_of(&dir), 0);
}

#[test]
fn last_seq_reads_a_last_line_past_one_backward_step() {
    let home = fakes::TempDir::new("log-last-seq-long");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    let big = "x".repeat(70_000);
    let line = serde_json::json!({
        "kind": "turn_completed",
        "session_id": id,
        "ts": 1_759_150_000_007_u64,
        "schema_version": 1,
        "seq": 7,
        "payload": {"text": big},
    });
    write_lines(&sessions, id, &[started(id, 0, None), format!("{line}\n")]);
    assert_eq!(seq_of(&sessions.join(id)), 7);
}

#[test]
fn last_seq_on_an_empty_log_is_none() {
    let home = fakes::TempDir::new("log-last-seq-empty");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(EVENTS), b"").unwrap();
    assert_eq!(last_seq(&dir).unwrap(), None);
}

#[test]
fn last_seq_on_a_missing_log_or_directory_is_an_io_error_naming_the_log() {
    let home = fakes::TempDir::new("log-last-seq-missing");
    let sessions = home.path().join("sessions");
    // Neither the directory nor the log exists.
    let missing = io_error(&sessions.join("s_aaaaaaaaaaaaaaaa"));
    assert!(missing.contains("events.jsonl"), "{missing}");
    // The directory exists but the log does not.
    let dir = sessions.join("s_bbbbbbbbbbbbbbbb");
    fs::create_dir_all(&dir).unwrap();
    let no_log = io_error(&dir);
    assert!(no_log.contains("events.jsonl"), "{no_log}");
}

#[test]
fn last_seq_without_a_boundary_is_a_pointer_error_naming_the_log() {
    let home = fakes::TempDir::new("log-last-seq-pointer");
    let sessions = home.path().join("sessions");
    // The log is a directory: this stands in for an unreadable log for
    // any user, root included.
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(dir.join(EVENTS)).unwrap();
    let said = pointer_error(&dir);
    assert!(said.contains("events.jsonl"), "{said}");
    // The log is a symlink to a non-regular file.
    let linked = sessions.join("s_bbbbbbbbbbbbbbbb");
    fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink("/dev/null", linked.join(EVENTS)).unwrap();
    let said = pointer_error(&linked);
    assert!(said.contains("events.jsonl"), "{said}");
    // The log holds only a torn line.
    let torn = sessions.join("s_cccccccccccccccc");
    fs::create_dir_all(&torn).unwrap();
    fs::write(torn.join(EVENTS), "{\"kind\": \"turn_started\", torn").unwrap();
    let said = pointer_error(&torn);
    assert!(said.contains("events.jsonl"), "{said}");
    // The last line does not parse.
    let corrupt = sessions.join("s_dddddddddddddddd");
    write_lines(
        &sessions,
        "s_dddddddddddddddd",
        &[started("s_dddddddddddddddd", 0, None)],
    );
    fs::write(
        corrupt.join(EVENTS),
        format!(
            "{}{}",
            started("s_dddddddddddddddd", 0, None),
            "{not json}\n"
        ),
    )
    .unwrap();
    let said = pointer_error(&corrupt);
    assert!(said.contains("events.jsonl"), "{said}");
    // The last line carries no seq.
    let no_seq = sessions.join("s_eeeeeeeeeeeeeeee");
    let line = serde_json::json!({
        "kind": "turn_completed",
        "session_id": "s_eeeeeeeeeeeeeeee",
        "ts": 1_759_150_000_007_u64,
        "schema_version": 1,
        "payload": {},
    });
    write_lines(
        &sessions,
        "s_eeeeeeeeeeeeeeee",
        &[started("s_eeeeeeeeeeeeeeee", 0, None), format!("{line}\n")],
    );
    let said = pointer_error(&no_seq);
    assert!(said.contains("events.jsonl"), "{said}");
}

#[test]
fn a_boundary_read_fails_closed_on_a_log_it_cannot_read_and_reads_it_once_restored() {
    let home = fakes::TempDir::new("log-last-seq-restored");
    let sessions = home.path().join("sessions");
    let id = "s_aaaaaaaaaaaaaaaa";
    write_lines(
        &sessions,
        id,
        &[started(id, 0, None), plain(id, 1, "fiber_exited")],
    );
    let dir = sessions.join(id);
    let log = dir.join(EVENTS);
    let held = fs::read(&log).unwrap();
    // Access denied: the log's path in its place is a directory, which
    // reads as no boundary for any user, root included.
    fs::rename(&log, dir.join("events.jsonl.held")).unwrap();
    fs::create_dir_all(&log).unwrap();
    let denied = pointer_error(&dir);
    assert!(denied.contains("events.jsonl"), "{denied}");
    // Restored before any spawn: the boundary reads again.
    fs::remove_dir(&log).unwrap();
    fs::write(&log, held).unwrap();
    assert_eq!(seq_of(&dir), 1);
}

#[test]
fn history_on_a_missing_log_is_not_found() {
    let home = fakes::TempDir::new("log-history-first-missing");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    let error = history(&dir).unwrap_err();
    let Error::NotFound(path) = error else {
        panic!("a missing log has a history");
    };
    assert_eq!(path, dir);
}

#[test]
fn a_moved_history_resolves_each_ancestor_in_its_recorded_project() {
    // Root R in `-work-old`; child C in `-work-new` names R with its
    // project; grandchild G in `-work-new` names C without one, beside
    // itself.
    let home = fakes::TempDir::new("log-history-projects");
    let root = project_dir(&home, "-work-old", "s_rrrrrrrrrrrrrrrr");
    let root_first = started("s_rrrrrrrrrrrrrrrr", 0, None);
    session_log(&root, root_first.as_bytes());
    let child = project_dir(&home, "-work-new", "s_cccccccccccccccc");
    let child_first = started_in(
        "s_cccccccccccccccc",
        0,
        "s_rrrrrrrrrrrrrrrr",
        0,
        Some("-work-old"),
    );
    session_log(&child, child_first.as_bytes());
    let grandchild = project_dir(&home, "-work-new", "s_gggggggggggggggg");
    let grandchild_first = started_in("s_gggggggggggggggg", 0, "s_cccccccccccccccc", 0, None);
    session_log(&grandchild, grandchild_first.as_bytes());
    assert_eq!(
        history(&grandchild).unwrap(),
        vec![
            Segment {
                session_id: SessionId("s_rrrrrrrrrrrrrrrr".into()),
                dir: root,
                to: Some(Seq(0)),
            },
            Segment {
                session_id: SessionId("s_cccccccccccccccc".into()),
                dir: child,
                to: Some(Seq(0)),
            },
            Segment {
                session_id: SessionId("s_gggggggggggggggg".into()),
                dir: grandchild,
                to: None,
            },
        ]
    );
}

#[test]
fn a_moved_parent_in_an_unknown_project_fails_closed() {
    let home = fakes::TempDir::new("log-history-unknown-project");
    let child = project_dir(&home, "-work-new", "s_cccccccccccccccc");
    let child_first = started_in(
        "s_cccccccccccccccc",
        0,
        "s_rrrrrrrrrrrrrrrr",
        0,
        Some("-work-gone"),
    );
    session_log(&child, child_first.as_bytes());
    let error = history(&child).unwrap_err();
    // The parent log is missing where recorded: not found, never scanned.
    let Error::NotFound(path) = error else {
        panic!("an unknown project has a history");
    };
    assert_eq!(
        path,
        crate::session_dir(
            home.path(),
            "-work-gone",
            &SessionId("s_rrrrrrrrrrrrrrrr".into())
        )
    );
}

#[test]
fn an_empty_parent_log_reads_as_missing() {
    let home = fakes::TempDir::new("log-history-empty-parent");
    let sessions = home.path().join("sessions");
    write_lines(
        &sessions,
        "s_cccccccccccccccc",
        &[started(
            "s_cccccccccccccccc",
            0,
            Some(("s_rrrrrrrrrrrrrrrr", 0)),
        )],
    );
    session_log(&sessions.join("s_rrrrrrrrrrrrrrrr"), b"");
    let error = history(&sessions.join("s_cccccccccccccccc")).unwrap_err();
    let Error::NotFound(path) = error else {
        panic!("an empty parent has a history");
    };
    assert_eq!(path, sessions.join("s_rrrrrrrrrrrrrrrr"));
}

#[test]
fn an_invalid_moved_parent_stays_a_pointer() {
    // A `project` that is not a string is corrupt, not absent: the walk
    // fails closed exactly as a bad session or seq does.
    let home = fakes::TempDir::new("log-history-bad-project");
    let sessions = home.path().join("sessions");
    let mut line = started("s_cccccccccccccccc", 0, Some(("s_rrrrrrrrrrrrrrrr", 0)));
    let mut parsed: Value = serde_json::from_str(&line).unwrap();
    parsed["payload"]["forked_from"]["project"] = json!(7);
    line = parsed.to_string() + "\n";
    write_lines(&sessions, "s_cccccccccccccccc", &[line]);
    let error = history(&sessions.join("s_cccccccccccccccc")).unwrap_err();
    assert!(matches!(error, Error::Pointer { .. }));
    assert_eq!(error.code(), contract::ErrorCode::LogCorrupt);
}

#[test]
fn a_project_pointer_outside_projects_is_a_pointer() {
    // A flat `sessions/` layout has no projects layer to climb: a
    // recorded project cannot resolve from stored locations.
    let home = fakes::TempDir::new("log-history-flat-project");
    let sessions = home.path().join("sessions");
    write_lines(
        &sessions,
        "s_cccccccccccccccc",
        &[started_in(
            "s_cccccccccccccccc",
            0,
            "s_rrrrrrrrrrrrrrrr",
            0,
            Some("-work-old"),
        )],
    );
    let error = history(&sessions.join("s_cccccccccccccccc")).unwrap_err();
    assert!(matches!(error, Error::Pointer { .. }));
    assert_eq!(error.code(), contract::ErrorCode::LogCorrupt);
}

#[test]
fn a_decoy_parent_in_the_childs_project_is_never_a_parent() {
    // The parent id exists beside the child, but the recorded project
    // is `-work-old`: the walk reads the recorded project only, and a
    // sibling with the same id is not a fallback.
    let home = fakes::TempDir::new("log-history-decoy");
    let new_sessions = home
        .path()
        .join("projects")
        .join("-work-new")
        .join("sessions");
    write_lines(
        &new_sessions,
        "s_rrrrrrrrrrrrrrrr",
        &[started("s_rrrrrrrrrrrrrrrr", 0, None)],
    );
    let old_sessions = home
        .path()
        .join("projects")
        .join("-work-old")
        .join("sessions");
    write_lines(
        &old_sessions,
        "s_oooooooooooooooo",
        &[started("s_oooooooooooooooo", 0, None)],
    );
    write_lines(
        &new_sessions,
        "s_cccccccccccccccc",
        &[started_in(
            "s_cccccccccccccccc",
            0,
            "s_rrrrrrrrrrrrrrrr",
            0,
            Some("-work-old"),
        )],
    );
    let error = history(&new_sessions.join("s_cccccccccccccccc")).unwrap_err();
    let Error::NotFound(path) = error else {
        panic!("a decoy sibling is not the recorded parent");
    };
    assert_eq!(path, old_sessions.join("s_rrrrrrrrrrrrrrrr"));
}

#[test]
fn an_alternating_project_chain_links_each_hop_in_its_recorded_project() {
    // Root R in `-work-old`; M in `-work-new` names R with its project;
    // G back in `-work-old` names M with its project. Each segment reads
    // from its own directory, bounded by its own point.
    let home = fakes::TempDir::new("log-history-alternating");
    let root = project_dir(&home, "-work-old", "s_rrrrrrrrrrrrrrrr");
    session_log(
        &root,
        (started("s_rrrrrrrrrrrrrrrr", 0, None) + &plain("s_rrrrrrrrrrrrrrrr", 1, "turn_started"))
            .as_bytes(),
    );
    // Past M's point the parent holds bytes that do not parse: the walk
    // and the bounded read leave them unread.
    corrupt(&root, 1);
    let middle = project_dir(&home, "-work-new", "s_bbbbbbbbbbbbbbbb");
    session_log(
        &middle,
        (started_in(
            "s_bbbbbbbbbbbbbbbb",
            0,
            "s_rrrrrrrrrrrrrrrr",
            0,
            Some("-work-old"),
        ) + &plain("s_bbbbbbbbbbbbbbbb", 1, "turn_started"))
            .as_bytes(),
    );
    let grandchild = project_dir(&home, "-work-old", "s_gggggggggggggggg");
    session_log(
        &grandchild,
        started_in(
            "s_gggggggggggggggg",
            0,
            "s_bbbbbbbbbbbbbbbb",
            1,
            Some("-work-new"),
        )
        .as_bytes(),
    );
    let segments = history(&grandchild).unwrap();
    assert_eq!(
        segments,
        vec![
            Segment {
                session_id: SessionId("s_rrrrrrrrrrrrrrrr".into()),
                dir: root.clone(),
                to: Some(Seq(0)),
            },
            Segment {
                session_id: SessionId("s_bbbbbbbbbbbbbbbb".into()),
                dir: middle.clone(),
                to: Some(Seq(1)),
            },
            Segment {
                session_id: SessionId("s_gggggggggggggggg".into()),
                dir: grandchild.clone(),
                to: None,
            },
        ]
    );
    let root_lines = segments[0].lines(0).unwrap();
    assert_eq!(root_lines.len(), 1);
    assert_eq!(root_lines[0].seq, Some(Seq(0)));
    let middle_lines = segments[1].lines(0).unwrap();
    assert_eq!(
        middle_lines
            .iter()
            .map(|line| line.seq.as_ref().unwrap().0)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn a_cross_project_cycle_is_a_pointer() {
    // A in `-work-new` names B in `-work-old`, and B names A back: the
    // walk stops on the repeat instead of climbing forever.
    let home = fakes::TempDir::new("log-history-cross-cycle");
    let a = project_dir(&home, "-work-new", "s_aaaaaaaaaaaaaaaa");
    session_log(
        &a,
        started_in(
            "s_aaaaaaaaaaaaaaaa",
            0,
            "s_bbbbbbbbbbbbbbbb",
            0,
            Some("-work-old"),
        )
        .as_bytes(),
    );
    let b = project_dir(&home, "-work-old", "s_bbbbbbbbbbbbbbbb");
    session_log(
        &b,
        started_in(
            "s_bbbbbbbbbbbbbbbb",
            0,
            "s_aaaaaaaaaaaaaaaa",
            0,
            Some("-work-new"),
        )
        .as_bytes(),
    );
    let error = history(&a).unwrap_err();
    let code = error.code();
    let Error::Pointer { path, reason } = error else {
        panic!("a cross-project cycle has a history");
    };
    assert_eq!(path, a.join(EVENTS));
    assert_eq!(reason, "the forked_from chain repeats a session");
    assert_eq!(code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn a_recorded_project_outside_projects_is_a_pointer() {
    // A project key that is not one path component cannot name a project
    // directory: the walk fails closed without reading past `projects/`.
    let home = fakes::TempDir::new("log-history-project-traversal");
    let child = project_dir(&home, "-work-new", "s_cccccccccccccccc");
    for key in ["", ".", "..", "../escape", "a/b", "/abs"] {
        session_log(
            &child,
            started_in("s_cccccccccccccccc", 0, "s_rrrrrrrrrrrrrrrr", 0, Some(key)).as_bytes(),
        );
        let error = history(&child).unwrap_err();
        let code = error.code();
        let Error::Pointer { path, reason } = error else {
            panic!("project {key:?} has a history");
        };
        assert_eq!(path, child.join(EVENTS), "project {key:?}");
        assert_eq!(
            reason, "the recorded parent project is not one path component",
            "project {key:?}"
        );
        assert_eq!(code, contract::ErrorCode::LogCorrupt);
    }
}

#[test]
fn a_recorded_parent_id_outside_its_sessions_dir_is_a_pointer() {
    // A parent session that is not one path component cannot name a
    // session directory, beside the child or in a recorded project.
    let home = fakes::TempDir::new("log-history-parent-traversal");
    let child = project_dir(&home, "-work-new", "s_cccccccccccccccc");
    for (parent, project) in [
        ("..", None),
        ("../s_rrrrrrrrrrrrrrrr", None),
        ("s_rrrrrrrrrrrrrrrr/x", None),
        ("..", Some("-work-old")),
        ("../s_rrrrrrrrrrrrrrrr", Some("-work-old")),
        ("s_rrrrrrrrrrrrrrrr/x", Some("-work-old")),
    ] {
        session_log(
            &child,
            started_in("s_cccccccccccccccc", 0, parent, 0, project).as_bytes(),
        );
        let error = history(&child).unwrap_err();
        let code = error.code();
        let Error::Pointer { path, reason } = error else {
            panic!("parent {parent:?} has a history");
        };
        assert_eq!(path, child.join(EVENTS), "parent {parent:?}");
        assert_eq!(
            reason, "the recorded parent session is not one path component",
            "parent {parent:?}"
        );
        assert_eq!(code, contract::ErrorCode::LogCorrupt);
    }
}

#[test]
fn history_with_a_first_line_with_no_newline_is_a_pointer_naming_the_missing_line() {
    let home = fakes::TempDir::new("log-history-first-nonl");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(EVENTS),
        started("s_aaaaaaaaaaaaaaaa", 0, None).trim_end(),
    )
    .unwrap();
    let error = history(&dir).unwrap_err();
    let Error::Pointer { path, reason } = error else {
        panic!("a first line with no newline has a history");
    };
    assert_eq!(path, dir.join(EVENTS));
    assert_eq!(reason, "the log's first line is missing");
}

#[test]
fn history_with_a_first_line_that_is_not_an_event_is_a_pointer() {
    let home = fakes::TempDir::new("log-history-first-event");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(EVENTS), "{\"kind\":\"session_started\"}\n").unwrap();
    let error = history(&dir).unwrap_err();
    let Error::Pointer { path, reason } = error else {
        panic!("a first line that is not an event has a history");
    };
    assert_eq!(path, dir.join(EVENTS));
    assert_eq!(reason, "the log's first line is not an event");
}
