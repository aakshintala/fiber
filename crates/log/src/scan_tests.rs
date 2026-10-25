//! Tests for the prune scan: the session list, the last `ts`, the byte
//! size, what remains, and the lock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use contract::SessionId;
use serde_json::json;

use super::*;
use crate::fixtures::session_log as write_log;

fn session(home: &Path, project: &str, id: &str, first: &str) {
    let dir = home
        .join("projects")
        .join(project)
        .join("sessions")
        .join(id);
    write_log(&dir, format!("{first}\n").as_bytes());
}

fn started(workspace: Option<&str>, from: Option<&str>) -> String {
    let mut payload = serde_json::Map::new();
    if let Some(workspace) = workspace {
        payload.insert("workspace".to_owned(), json!(workspace));
    }
    if let Some(from) = from {
        payload.insert(
            "forked_from".to_owned(),
            json!({"session_id": from, "seq": 3}),
        );
    }
    json!({"kind": "session_started", "seq": 0, "payload": payload}).to_string()
}

fn ids(found: &[Started]) -> Vec<&str> {
    found.iter().map(|s| s.id.0.as_str()).collect()
}

#[test]
fn two_projects_sessions_are_found_sorted_by_id() {
    let home = fakes::TempDir::new("log-scan-list");
    let home = home.path();
    session(home, "-b", "s_0000000000000002", &started(Some("/w"), None));
    session(home, "-a", "s_0000000000000001", &started(Some("/w"), None));
    let found = started_sessions(home);
    assert_eq!(ids(&found), ["s_0000000000000001", "s_0000000000000002"]);
    assert_eq!(found[0].workspace.as_deref(), Some("/w"));
    assert!(found[0].forked_from.is_none());
}

#[test]
fn a_directory_whose_first_line_is_not_session_started_is_left_out() {
    let home = fakes::TempDir::new("log-scan-skip");
    let home = home.path();
    session(home, "-a", "s_0000000000000001", &started(Some("/w"), None));
    session(
        home,
        "-a",
        "s_0000000000000002",
        &json!({"kind": "turn_started"}).to_string(),
    );
    session(home, "-a", "s_0000000000000003", "not json");
    let empty = home.join("projects/-a/sessions/s_0000000000000004");
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join("events.jsonl"), b"").unwrap();
    fs::create_dir_all(home.join("projects/-a/sessions/s_0000000000000005")).unwrap();
    assert_eq!(ids(&started_sessions(home)), ["s_0000000000000001"]);
}

#[test]
fn a_symlinked_session_directory_is_left_out() {
    let home = fakes::TempDir::new("log-scan-link");
    let home = home.path();
    session(home, "-a", "s_0000000000000001", &started(Some("/w"), None));
    let outside = home.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("events.jsonl"),
        format!("{}\n", started(Some("/w"), None)),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        &outside,
        home.join("projects/-a/sessions/s_0000000000000002"),
    )
    .unwrap();
    assert_eq!(ids(&started_sessions(home)), ["s_0000000000000001"]);
}

#[test]
fn started_sessions_in_lists_only_that_project() {
    let home = fakes::TempDir::new("log-scan-in-project");
    let home = home.path();
    session(home, "-a", "s_0000000000000001", &started(Some("/w"), None));
    session(home, "-a", "s_0000000000000003", &started(Some("/w"), None));
    session(home, "-b", "s_0000000000000002", &started(Some("/w"), None));
    let found = started_sessions_in(&home.join("projects/-a/sessions"));
    assert_eq!(ids(&found), ["s_0000000000000001", "s_0000000000000003"]);
}

#[test]
fn started_sessions_in_with_a_missing_dir_or_a_link_gives_nothing() {
    let home = fakes::TempDir::new("log-scan-in-missing");
    let home = home.path();
    assert!(started_sessions_in(&home.join("projects/-a/sessions")).is_empty());
    let outside = home.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("events.jsonl"),
        format!("{}\n", started(Some("/w"), None)),
    )
    .unwrap();
    let sessions = home.join("projects/-a/sessions");
    fs::create_dir_all(&sessions).unwrap();
    std::os::unix::fs::symlink(&outside, sessions.join("s_0000000000000001")).unwrap();
    assert!(started_sessions_in(&sessions).is_empty());
}

#[test]
fn forked_from_is_read() {
    let home = fakes::TempDir::new("log-scan-fork");
    let home = home.path();
    session(
        home,
        "-a",
        "s_0000000000000002",
        &started(Some("/w"), Some("s_0000000000000001")),
    );
    let found = started_sessions(home);
    assert_eq!(ids(&found), ["s_0000000000000002"]);
    assert_eq!(
        found[0].forked_from.as_ref().map(|id| id.0.as_str()),
        Some("s_0000000000000001")
    );
}

#[test]
fn worktree_path_is_read() {
    let home = fakes::TempDir::new("log-scan-worktree");
    let home = home.path();
    session(
        home,
        "-a",
        "s_0000000000000001",
        &json!({"kind": "session_started", "seq": 0, "payload": {
            "workspace": "/wt-1",
            "worktree": {"path": "/wt-1", "branch": "fiber/s", "repository": "/r"},
        }})
        .to_string(),
    );
    session(home, "-a", "s_0000000000000002", &started(Some("/w"), None));
    let found = started_sessions(home);
    assert_eq!(found[0].worktree.as_deref(), Some("/wt-1"));
    assert_eq!(found[1].worktree, None);
    let view = started_first(&home.join("projects/-a/sessions/s_0000000000000001")).unwrap();
    assert_eq!(view.worktree.as_deref(), Some("/wt-1"));
}

#[test]
fn worktree_path_is_none_unless_an_object_with_a_string_path() {
    for payload in [
        json!({"workspace": "/w"}),
        json!({"workspace": "/w", "worktree": "/wt-1"}),
        json!({"workspace": "/w", "worktree": {"branch": "fiber/s"}}),
        json!({"workspace": "/w", "worktree": {"path": 7}}),
    ] {
        let first = json!({"kind": "session_started", "payload": payload}).to_string();
        let view = SessionStarted::parse(first.as_bytes()).expect("a session_started");
        assert!(view.worktree.is_none(), "{payload}");
    }
}

#[test]
fn worktree_record_is_none_when_absent() {
    let payload = json!({"workspace": "/w"});
    let first = json!({"kind": "session_started", "payload": payload}).to_string();
    let view = SessionStarted::parse(first.as_bytes()).expect("a session_started");
    assert_eq!(view.worktree_record().unwrap(), None, "{payload}");
}

#[test]
fn worktree_record_null_is_malformed() {
    // The payload rules forbid `null`: an optional key is absent, never
    // `null`, so a `null` worktree is foreign data and malformed.
    let payload = json!({"workspace": "/w", "worktree": Value::Null});
    let first = json!({"kind": "session_started", "payload": payload}).to_string();
    let view = SessionStarted::parse(first.as_bytes()).expect("a session_started");
    assert!(view.worktree_record().is_err(), "{payload}");
}

#[test]
fn worktree_record_carries_a_valid_record() {
    let home = fakes::TempDir::new("log-scan-record-valid");
    let dir = home.path().join("s");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        json!({"kind": "session_started", "payload": {
            "workspace": "/wt-1",
            "worktree": {"path": "/wt-1", "branch": "fiber/s", "repository": "/r"},
        }})
        .to_string()
            + "\n",
    )
    .unwrap();
    let record = started_first(&dir)
        .unwrap()
        .worktree_record()
        .unwrap()
        .unwrap();
    assert_eq!(record.path, "/wt-1");
    assert_eq!(record.branch, "fiber/s");
    assert_eq!(record.repository.as_deref(), Some("/r"));
}

#[test]
fn worktree_record_is_malformed_unless_a_record_with_an_absolute_repository() {
    for worktree in [
        json!([]),
        json!("/wt-1"),
        json!(7),
        json!({}),
        json!({"branch": "fiber/s", "repository": "/r"}),
        json!({"path": "/wt-1", "repository": "/r"}),
        json!({"path": "/wt-1", "branch": "fiber/s"}),
        json!({"path": 7, "branch": "fiber/s", "repository": "/r"}),
        json!({"path": "/wt-1", "branch": "fiber/s", "repository": "relative/r"}),
        json!({"path": "relative", "branch": "b", "repository": "/r"}),
        json!({"path": "/wt-1", "branch": "fiber/s", "repository": Value::Null}),
    ] {
        let payload = json!({"workspace": "/w", "worktree": worktree});
        let first = json!({"kind": "session_started", "payload": payload}).to_string();
        let view = SessionStarted::parse(first.as_bytes()).expect("a session_started");
        assert!(view.worktree_record().is_err(), "{worktree}");
    }
}

#[test]
fn a_missing_projects_gives_an_empty_list() {
    let home = fakes::TempDir::new("log-scan-none");
    assert!(started_sessions(home.path()).is_empty());
}

#[test]
fn a_child_with_forked_from_but_no_workspace_counts_for_both_walks() {
    let home = fakes::TempDir::new("log-scan-noworkspace");
    let home = home.path();
    session(home, "-a", "s_0000000000000001", &started(Some("/w"), None));
    session(
        home,
        "-a",
        "s_0000000000000002",
        &started(None, Some("s_0000000000000001")),
    );
    let found = started_sessions(home);
    assert_eq!(ids(&found), ["s_0000000000000001", "s_0000000000000002"]);
    let child = found
        .iter()
        .find(|s| s.id.0 == "s_0000000000000002")
        .unwrap();
    assert!(child.workspace.is_none());
    assert_eq!(
        child.forked_from.as_ref().map(|id| id.0.as_str()),
        Some("s_0000000000000001")
    );
    let dependents = crate::dependents(home, &SessionId("s_0000000000000001".to_owned()));
    assert_eq!(
        dependents
            .iter()
            .map(|id| id.0.as_str())
            .collect::<Vec<_>>(),
        ["s_0000000000000002"]
    );
}

#[test]
fn a_first_line_with_no_trailing_newline_is_included() {
    let home = fakes::TempDir::new("log-scan-nonl");
    let home = home.path();
    let dir = home.join("projects/-a/sessions/s_0000000000000001");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), started(Some("/w"), None)).unwrap();
    assert_eq!(ids(&started_sessions(home)), ["s_0000000000000001"]);
}

fn event(ts: u64) -> String {
    json!({"kind": "x", "ts": ts}).to_string()
}

#[test]
fn last_ts_uses_the_last_complete_line_past_a_torn_tail() {
    let home = fakes::TempDir::new("log-scan-torn");
    let dir = home.path().join("s");
    write_log(
        &dir,
        format!("{}\n{}\npartial", event(7), event(9)).as_bytes(),
    );
    assert_eq!(last_ts(&dir), Some(9));
}

#[test]
fn the_backward_step_is_64_kib() {
    assert_eq!(super::STEP, 64 * 1024);
}

#[test]
fn last_complete_line_handles_offsets_at_step_boundaries() {
    let step = usize::try_from(super::STEP).unwrap();
    let mut cases = vec![
        ("one-byte-line", b"x\n".to_vec(), Some(b"x".to_vec())),
        (
            "two-lines",
            b"first\nsecond\n".to_vec(),
            Some(b"second".to_vec()),
        ),
        (
            "one-byte-torn-tail",
            b"first\nlast\nx".to_vec(),
            Some(b"last".to_vec()),
        ),
        (
            "first-line-without-earlier-newline",
            b"first\n".to_vec(),
            Some(b"first".to_vec()),
        ),
        ("no-newline", b"one line".to_vec(), None),
    ];
    for (name, length) in [
        ("step-minus-one", step - 1),
        ("step", step),
        ("step-plus-one", step + 1),
    ] {
        let line = vec![b'x'; length];
        let mut contents = line.clone();
        contents.push(b'\n');
        cases.push((name, contents, Some(line)));
    }

    let home = fakes::TempDir::new("log-scan-offsets");
    for (name, contents, expected) in cases {
        let path = home.path().join(name);
        fs::write(&path, &contents).unwrap();
        let file = File::open(path).unwrap();
        let len = u64::try_from(contents.len()).unwrap();
        assert_eq!(super::last_complete_line(file, len), expected, "{name}");
    }
}

#[test]
fn last_ts_skips_a_torn_tail_longer_than_one_step() {
    // 70 000 torn bytes after the last newline: the first 64 KiB step
    // back holds no newline, so only the loop finds the last line.
    let home = fakes::TempDir::new("log-scan-torn-tail");
    let dir = home.path().join("s");
    write_log(
        &dir,
        format!("{}\n{}\n{}", event(1), event(9), "p".repeat(70 * 1024)).as_bytes(),
    );
    assert_eq!(last_ts(&dir), Some(9));
}

#[test]
fn last_ts_uses_the_last_complete_line_when_a_short_torn_tail_fits_in_one_step() {
    // A first line longer than one step leaves `start` past 0 when the
    // last newline is found, so `end - start` differs from `end + start`:
    // the `+` mutant clamps to the window and returns the torn tail.
    let home = fakes::TempDir::new("log-scan-torn-window");
    let dir = home.path().join("s");
    let big = format!(
        "{{\"kind\":\"x\",\"ts\":1,\"pad\":\"{}\"}}\n",
        "x".repeat(70 * 1024)
    );
    write_log(&dir, format!("{big}{}\npartial", event(9)).as_bytes());
    assert_eq!(last_ts(&dir), Some(9));
}

#[test]
fn last_ts_reads_a_last_line_longer_than_64_kib() {
    let home = fakes::TempDir::new("log-scan-long");
    let dir = home.path().join("s");
    let big = format!(
        "{{\"kind\":\"x\",\"ts\":42,\"pad\":\"{}\" }}\n",
        "x".repeat(70 * 1024)
    );
    write_log(&dir, format!("{}\n{big}", event(1)).as_bytes());
    assert_eq!(last_ts(&dir), Some(42));
}

#[test]
fn last_ts_with_one_line_and_no_newline_gives_none() {
    let home = fakes::TempDir::new("log-scan-oneline");
    let dir = home.path().join("s");
    write_log(&dir, event(5).as_bytes());
    assert_eq!(last_ts(&dir), None);
}

#[test]
fn last_ts_with_an_unparseable_last_line_gives_none() {
    let home = fakes::TempDir::new("log-scan-bad");
    let dir = home.path().join("s");
    write_log(&dir, format!("{}\nnot json\n", event(5)).as_bytes());
    assert_eq!(last_ts(&dir), None);
}

#[test]
fn last_ts_with_a_line_without_ts_gives_none() {
    let home = fakes::TempDir::new("log-scan-nots");
    let dir = home.path().join("s");
    write_log(
        &dir,
        format!("{}\n{{\"kind\":\"x\"}}\n", event(5)).as_bytes(),
    );
    assert_eq!(last_ts(&dir), None);
}

#[test]
fn last_ts_with_an_empty_file_gives_none() {
    let home = fakes::TempDir::new("log-scan-empty");
    let dir = home.path().join("s");
    write_log(&dir, b"");
    assert_eq!(last_ts(&dir), None);
}

#[test]
fn session_bytes_counts_regular_files_and_leaves_out_a_link() {
    let home = fakes::TempDir::new("log-scan-bytes");
    let dir = home.path().join("s");
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    fs::write(dir.join("events.jsonl"), b"12345").unwrap();
    fs::write(dir.join("artifacts").join("a.txt"), b"123").unwrap();
    std::os::unix::fs::symlink(dir.join("events.jsonl"), dir.join("artifacts").join("link"))
        .unwrap();
    assert_eq!(session_bytes(&dir), 8);
}

#[test]
fn remaining_reports_whole_smaller_and_gone() {
    let home = fakes::TempDir::new("log-scan-remaining");
    let dir = home.path().join("s");
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    fs::write(dir.join("events.jsonl"), b"12345").unwrap();
    fs::write(dir.join("artifacts").join("a.txt"), b"123").unwrap();
    let whole = remaining(&dir).unwrap();
    assert_eq!(whole, Some(8));
    fs::remove_file(dir.join("events.jsonl")).unwrap();
    let smaller = remaining(&dir).unwrap();
    assert_eq!(smaller, Some(3));
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(remaining(&dir).unwrap(), None);
}

#[test]
fn remaining_on_an_unreadable_parent_is_an_error_other_than_not_found() {
    let home = fakes::TempDir::new("log-scan-noperm");
    let parent = home.path().join("p");
    let dir = parent.join("s");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), b"x").unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
    let result = remaining(&dir);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    let error = result.unwrap_err();
    assert_ne!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn try_hold_holds_then_refuses_while_held_and_frees_on_drop() {
    let home = fakes::TempDir::new("log-scan-hold");
    let dir = home.path().join("s");
    fs::create_dir_all(&dir).unwrap();
    let held = match try_hold(&dir).unwrap() {
        Hold::Held(lock) => lock,
        Hold::Busy => panic!("first hold is held"),
    };
    assert!(dir.join("session.lock").is_file());
    assert!(matches!(try_hold(&dir).unwrap(), Hold::Busy));
    drop(held);
    assert!(matches!(try_hold(&dir).unwrap(), Hold::Held(_)));
}

#[test]
fn try_hold_holds_through_another_descriptor_and_creates_a_missing_lock() {
    let home = fakes::TempDir::new("log-scan-hold2");
    let dir = home.path().join("s");
    fs::create_dir_all(&dir).unwrap();
    File::create(dir.join("events.jsonl")).unwrap();
    assert!(!dir.join("session.lock").exists());
    let guard = match try_hold(&dir).unwrap() {
        Hold::Held(lock) => lock,
        Hold::Busy => panic!("missing lock is created and held"),
    };
    drop(guard);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("session.lock"))
        .unwrap();
    file.try_lock().unwrap();
    assert!(matches!(try_hold(&dir).unwrap(), Hold::Busy));
    drop(file);
    assert!(matches!(try_hold(&dir).unwrap(), Hold::Held(_)));
}

#[test]
fn try_hold_existing_leaves_a_missing_lock_missing() {
    let home = fakes::TempDir::new("log-scan-existing-missing");
    let dir = home.path().join("s");
    fs::create_dir_all(&dir).unwrap();
    assert!(try_hold_existing(&dir).unwrap().is_none());
    assert!(!dir.join("session.lock").exists());
}

#[test]
fn try_hold_existing_reports_busy_while_held_and_holds_when_free() {
    let home = fakes::TempDir::new("log-scan-existing-held");
    let dir = home.path().join("s");
    fs::create_dir_all(&dir).unwrap();
    File::create(dir.join("session.lock")).unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("session.lock"))
        .unwrap();
    file.try_lock().unwrap();
    assert!(matches!(try_hold_existing(&dir).unwrap(), Some(Hold::Busy)));
    drop(file);
    let guard = match try_hold_existing(&dir).unwrap() {
        Some(Hold::Held(lock)) => lock,
        _ => panic!("a free existing lock is held"),
    };
    drop(guard);
}

#[test]
fn try_hold_existing_errors_when_the_lock_is_not_openable() {
    let home = fakes::TempDir::new("log-scan-existing-unopenable");
    let dir = home.path().join("s");
    // A directory where the lock file goes: opening it fails with a
    // non-`NotFound` error, which is an error, not a missing lock.
    fs::create_dir_all(dir.join("session.lock")).unwrap();
    assert!(matches!(try_hold_existing(&dir), Err(Error::Io { .. })));
}

#[test]
fn last_line_where_with_a_missing_or_empty_log_gives_none() {
    let home = fakes::TempDir::new("log-scan-where-missing");
    assert_eq!(
        last_line_where::<()>(home.path().join("s").as_path(), |_| Some(())),
        None
    );
    let dir = home.path().join("s");
    write_log(&dir, b"");
    assert_eq!(last_line_where::<()>(&dir, |_| Some(())), None);
}

#[test]
fn last_line_where_with_one_line_and_no_newline_gives_none() {
    let home = fakes::TempDir::new("log-scan-where-torn");
    let dir = home.path().join("s");
    write_log(&dir, b"only a torn tail");
    assert_eq!(last_line_where::<()>(&dir, |_| Some(())), None);
}

#[test]
fn last_line_where_reads_complete_lines_last_first_and_stops_at_its_first_some() {
    let home = fakes::TempDir::new("log-scan-where-order");
    let dir = home.path().join("s");
    write_log(&dir, b"one\ntwo\nthree\n");
    let mut seen = Vec::new();
    let found = last_line_where(&dir, |line| {
        seen.push(line.to_owned());
        (line == b"two").then_some(line.len())
    });
    assert_eq!(found, Some(3));
    assert_eq!(seen, [b"three".to_vec(), b"two".to_vec()]);
}

#[test]
fn last_line_where_hands_a_line_longer_than_one_step_over_whole() {
    let home = fakes::TempDir::new("log-scan-where-long");
    let dir = home.path().join("s");
    let big = "x".repeat(70 * 1024);
    write_log(&dir, format!("first\n{big}\nlast\n").as_bytes());
    let found = last_line_where(&dir, |line| (line.len() > 64 * 1024).then_some(line.len()));
    assert_eq!(found, Some(big.len()));
}

#[test]
fn last_line_where_finds_a_match_more_than_one_step_before_the_end() {
    let home = fakes::TempDir::new("log-scan-where-far");
    let dir = home.path().join("s");
    let filler = "{\"kind\":\"x\"}\n".repeat(6000);
    write_log(&dir, format!("match-me\n{filler}").as_bytes());
    let mut calls = 0;
    let found = last_line_where(&dir, |line| {
        calls += 1;
        (line == b"match-me").then_some(())
    });
    assert_eq!(found, Some(()));
    assert_eq!(calls, 6001);
}

/// `last_ts` over a small log completes: the backward scan's step moves
/// its offset every step. A `STEP` that cannot advance (the `/` mutant
/// makes it zero) hangs the scan until the mutant deadline, so the helper
/// thread carries a wall-clock bound and the hang fails here instead.
#[test]
fn last_ts_completes_within_a_wall_clock_bound() {
    let home = fakes::TempDir::new("log-scan-step-progress");
    let dir = home.path().join("s");
    write_log(&dir, format!("{}\n{}\n", event(7), event(9)).as_bytes());
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ts = last_ts(&dir);
        done.send(ts).unwrap();
    });
    let ts = fakes::Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&waited, "last_ts of the small log");
    assert_eq!(ts, Some(9));
}

#[test]
fn parse_reads_a_minimal_first_line_with_no_newline_or_ts_or_seq() {
    let view = SessionStarted::parse(br#"{"kind":"session_started","payload":{"workspace":"/w"}}"#)
        .unwrap();
    assert_eq!(view.workspace.as_deref(), Some("/w"));
    assert!(view.forked_from().is_none());
}

#[test]
fn parse_rejects_a_wrong_kind() {
    assert!(
        SessionStarted::parse(br#"{"kind":"turn_started","payload":{"workspace":"/w"}}"#).is_none()
    );
}

#[test]
fn parse_rejects_garbage() {
    assert!(SessionStarted::parse(b"not json").is_none());
}

#[test]
fn parse_rejects_a_first_line_with_no_payload() {
    assert!(SessionStarted::parse(br#"{"kind":"session_started"}"#).is_none());
}

#[test]
fn parse_rejects_a_non_string_kind() {
    assert!(SessionStarted::parse(br#"{"kind":7,"payload":{"workspace":"/w"}}"#).is_none());
}

#[test]
fn a_pointer_with_no_seq_is_lenient_but_not_strict() {
    let view = SessionStarted::parse(
        br#"{"kind":"session_started","payload":{"workspace":"/w","forked_from":{"session_id":"s_p"}}}"#,
    )
    .unwrap();
    assert_eq!(view.forked_from(), Some(SessionId("s_p".to_owned())));
    assert!(view.fork_point().is_err());
}

#[test]
fn a_pointer_with_a_string_seq_is_lenient_but_not_strict() {
    let view = SessionStarted::parse(
        br#"{"kind":"session_started","payload":{"forked_from":{"session_id":"s_p","seq":"3"}}}"#,
    )
    .unwrap();
    assert_eq!(view.forked_from(), Some(SessionId("s_p".to_owned())));
    assert!(view.fork_point().is_err());
}

#[test]
fn a_pointer_with_a_session_and_a_seq_reads_both_ways() {
    let view = SessionStarted::parse(
        br#"{"kind":"session_started","payload":{"workspace":"/w","forked_from":{"session_id":"s_p","seq":3}}}"#,
    )
    .unwrap();
    assert_eq!(view.forked_from(), Some(SessionId("s_p".to_owned())));
    assert_eq!(
        view.fork_point().unwrap(),
        Some((SessionId("s_p".to_owned()), Seq(3)))
    );
}

#[test]
fn a_null_pointer_is_absent_leniently_and_malformed_strictly() {
    let view =
        SessionStarted::parse(br#"{"kind":"session_started","payload":{"forked_from":null}}"#)
            .unwrap();
    assert!(view.forked_from().is_none());
    assert!(view.fork_point().is_err());
}

#[test]
fn a_pointer_with_a_non_string_session_is_absent_leniently_and_malformed_strictly() {
    let view = SessionStarted::parse(
        br#"{"kind":"session_started","payload":{"forked_from":{"session_id":7,"seq":3}}}"#,
    )
    .unwrap();
    assert!(view.forked_from().is_none());
    assert!(view.fork_point().is_err());
}

#[test]
fn no_pointer_is_absent_both_ways() {
    let view = SessionStarted::parse(br#"{"kind":"session_started","payload":{"workspace":"/w"}}"#)
        .unwrap();
    assert!(view.forked_from().is_none());
    assert_eq!(view.fork_point().unwrap(), None);
}

#[test]
fn first_line_on_a_missing_log_is_not_found() {
    let home = fakes::TempDir::new("log-first-line-missing");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    let Err(Error::NotFound(path)) = first_line(&dir) else {
        panic!("a missing log has a first line");
    };
    assert_eq!(path, dir);
}

#[test]
fn first_line_with_no_trailing_newline_is_a_pointer_naming_the_missing_line() {
    let home = fakes::TempDir::new("log-first-line-nonl");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        br#"{"kind":"session_started","payload":{"workspace":"/w"}}"#,
    )
    .unwrap();
    let Err(Error::Pointer { path, reason }) = first_line(&dir) else {
        panic!("a first line with no newline reads");
    };
    assert_eq!(path, dir.join(EVENTS));
    assert_eq!(reason, "the log's first line is missing");
}

#[test]
fn first_line_that_is_not_an_event_is_a_pointer() {
    let home = fakes::TempDir::new("log-first-line-event");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        br#"{"kind":"session_started","payload":{}}
"#,
    )
    .unwrap();
    let Err(Error::Pointer { path, reason }) = first_line(&dir) else {
        panic!("a first line that is not an event reads");
    };
    assert_eq!(path, dir.join(EVENTS));
    assert_eq!(reason, "the log's first line is not an event");
}

#[test]
fn first_line_of_another_kind_is_a_pointer() {
    let home = fakes::TempDir::new("log-first-line-kind");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        br#"{"kind":"turn_started","session_id":"s_aaaaaaaaaaaaaaaa","ts":1,"schema_version":1,"payload":{}}
"#,
    )
    .unwrap();
    let Err(Error::Pointer { path, reason }) = first_line(&dir) else {
        panic!("a first line of another kind reads");
    };
    assert_eq!(path, dir.join(EVENTS));
    assert_eq!(reason, "the log's first line is not session_started");
}

#[test]
fn first_line_of_a_session_started_gives_the_view() {
    let home = fakes::TempDir::new("log-first-line-view");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        br#"{"kind":"session_started","session_id":"s","ts":1,"schema_version":1,"seq":0,"payload":{"workspace":"/w","forked_from":{"session_id":"s_p","seq":3}}}
"#,
    )
    .unwrap();
    let view = first_line(&dir).unwrap();
    assert_eq!(view.workspace.as_deref(), Some("/w"));
    assert_eq!(view.forked_from(), Some(SessionId("s_p".to_owned())));
    assert_eq!(
        view.fork_point().unwrap(),
        Some((SessionId("s_p".to_owned()), Seq(3)))
    );
}

#[test]
fn started_first_reads_the_workspace_repository_and_delegate() {
    let home = fakes::TempDir::new("log-started-first");
    let dir = home.path().join("s");
    fs::create_dir_all(&dir).unwrap();
    // Missing, empty and newline-less logs are incomplete.
    assert!(matches!(
        started_first(&dir),
        Err(FirstLineError::Incomplete)
    ));
    fs::write(dir.join("events.jsonl"), b"").unwrap();
    assert!(matches!(
        started_first(&dir),
        Err(FirstLineError::Incomplete)
    ));
    fs::write(dir.join("events.jsonl"), b"{\"kind\":\"session_started\"").unwrap();
    assert!(matches!(
        started_first(&dir),
        Err(FirstLineError::Incomplete)
    ));
    // A line torn inside a multi-byte character is incomplete until its
    // newline: completeness is decided on bytes before any decoding.
    let mut torn = br#"{"kind":"session_started","payload":{"workspace":"/w"#.to_vec();
    torn.extend_from_slice(&[0xC3]);
    fs::write(dir.join("events.jsonl"), &torn).unwrap();
    assert!(matches!(
        started_first(&dir),
        Err(FirstLineError::Incomplete)
    ));
    // Corrupt: not UTF-8, not JSON, the wrong kind, no workspace.
    let mut bad_utf8 = torn.clone();
    bad_utf8.push(b'\n');
    fs::write(dir.join("events.jsonl"), &bad_utf8).unwrap();
    assert!(matches!(started_first(&dir), Err(FirstLineError::Corrupt)));
    for first in [
        "not json\n".to_owned(),
        json!({"kind": "fiber_started", "payload": {"workspace": "/w"}}).to_string() + "\n",
        json!({"kind": "session_started", "payload": {}}).to_string() + "\n",
        json!({"kind": "session_started", "payload": {"workspace": 7}}).to_string() + "\n",
    ] {
        fs::write(dir.join("events.jsonl"), first.as_bytes()).unwrap();
        assert!(
            matches!(started_first(&dir), Err(FirstLineError::Corrupt)),
            "{first}"
        );
    }
    // Minimal envelopes with no `ts` or `session_id` are still `Ok`.
    fs::write(
        dir.join("events.jsonl"),
        format!(
            "{}\n",
            json!({"kind": "session_started", "payload": {"workspace": "/w"}})
        ),
    )
    .unwrap();
    let view = started_first(&dir).unwrap();
    assert_eq!(view.workspace.as_deref(), Some("/w"));
    assert_eq!(view.repository, None);
    assert!(!view.delegate);
    // With a recorded repository and a parent object.
    fs::write(
        dir.join("events.jsonl"),
        json!({"kind": "session_started", "payload": {
            "workspace": "/w",
            "worktree": {"repository": "/r"},
            "parent": {"session_id": "s", "delegate_id": "j"},
        }})
        .to_string()
            + "\n",
    )
    .unwrap();
    let view = started_first(&dir).unwrap();
    assert_eq!(view.repository.as_deref(), Some("/r"));
    assert!(view.delegate);
    // `parent: null` is not a delegate; any other shape is.
    for (parent, delegate) in [(json!(null), false), (json!(7), true)] {
        fs::write(
            dir.join("events.jsonl"),
            json!({"kind": "session_started", "payload": {"workspace": "/w", "parent": parent}})
                .to_string()
                + "\n",
        )
        .unwrap();
        assert_eq!(started_first(&dir).unwrap().delegate, delegate, "{parent}");
    }
}

#[test]
fn started_first_with_an_unopenable_log_is_corrupt() {
    let home = fakes::TempDir::new("log-started-first-loop");
    let dir = home.path().join("s");
    fs::create_dir_all(&dir).unwrap();
    // A symlink loop makes `File::open` fail with an error other than
    // `NotFound` (ELOOP), without root or permissions.
    std::os::unix::fs::symlink("events.jsonl", dir.join("events.jsonl")).unwrap();
    assert!(matches!(started_first(&dir), Err(FirstLineError::Corrupt)));
}
