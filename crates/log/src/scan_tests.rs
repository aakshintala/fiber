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

use contract::SessionId;
use serde_json::json;

use super::*;

fn session(home: &Path, project: &str, id: &str, first: &str) {
    let dir = home
        .join("projects")
        .join(project)
        .join("sessions")
        .join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), format!("{first}\n")).unwrap();
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

fn write_log(dir: &Path, bytes: &[u8]) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("events.jsonl"), bytes).unwrap();
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
