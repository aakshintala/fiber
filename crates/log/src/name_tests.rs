//! Tests for the sender-name lookup: the latest `session_named` in the
//! sender's log, read from the end backwards.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;

use contract::SessionId;
use serde_json::json;

use super::{session_name, session_parent};

const SENDER: &str = "s_sender00000001";

fn id(text: &str) -> SessionId {
    SessionId(text.into())
}

/// Writes `lines` as the sender's log under project `project` of `home`.
fn plant(home: &std::path::Path, project: &str, sender: &str, lines: &[String]) {
    let dir = home
        .join("projects")
        .join(project)
        .join("sessions")
        .join(sender);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
}

fn named(name: serde_json::Value) -> String {
    json!({"kind": "session_named", "payload": {"name": name, "by": "person"}}).to_string()
}

fn other() -> String {
    json!({"kind": "turn_started", "payload": {"name": "not a name"}}).to_string()
}

#[test]
fn with_no_projects_there_is_no_name() {
    let home = fakes::TempDir::new("log-name-none");
    assert_eq!(session_name(home.path(), &id(SENDER)), None);
}

#[test]
fn the_latest_session_named_wins() {
    let home = fakes::TempDir::new("log-name-latest");
    plant(
        home.path(),
        "-a",
        SENDER,
        &[named(json!("a")), other(), named(json!("b"))],
    );
    assert_eq!(session_name(home.path(), &id(SENDER)), Some("b".to_owned()));
}

#[test]
fn a_cleared_name_goes_by_the_id_alone() {
    let home = fakes::TempDir::new("log-name-cleared");
    plant(
        home.path(),
        "-a",
        SENDER,
        &[named(json!("a")), named(serde_json::Value::Null)],
    );
    assert_eq!(session_name(home.path(), &id(SENDER)), None);
}

#[test]
fn a_session_never_named_goes_by_its_id_alone() {
    let home = fakes::TempDir::new("log-name-never");
    plant(home.path(), "-a", SENDER, &[other()]);
    assert_eq!(session_name(home.path(), &id(SENDER)), None);
}

#[test]
fn a_torn_trailing_name_is_skipped() {
    let home = fakes::TempDir::new("log-name-torn");
    let dir = home.path().join("projects/-a/sessions").join(SENDER);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        format!("{}\n{}", named(json!("a")), named(json!("b"))),
    )
    .unwrap();
    assert_eq!(session_name(home.path(), &id(SENDER)), Some("a".to_owned()));
}

#[test]
fn a_malformed_line_is_skipped_going_back() {
    let home = fakes::TempDir::new("log-name-bad");
    plant(
        home.path(),
        "-a",
        SENDER,
        &[named(json!("a")), "not json".to_owned()],
    );
    assert_eq!(session_name(home.path(), &id(SENDER)), Some("a".to_owned()));
}

#[test]
fn a_name_before_more_than_one_step_of_other_lines_is_found() {
    let home = fakes::TempDir::new("log-name-far");
    let mut lines = vec![named(json!("a"))];
    lines.extend(vec![other(); 6000]);
    plant(home.path(), "-a", SENDER, &lines);
    assert_eq!(session_name(home.path(), &id(SENDER)), Some("a".to_owned()));
}

#[test]
fn the_sender_is_found_in_the_second_of_two_projects() {
    let home = fakes::TempDir::new("log-name-second");
    plant(home.path(), "-a", "s_other00000002", &[named(json!("x"))]);
    plant(home.path(), "-b", SENDER, &[named(json!("b"))]);
    assert_eq!(session_name(home.path(), &id(SENDER)), Some("b".to_owned()));
}

#[test]
fn an_id_that_is_not_one_normal_component_reaches_no_log() {
    let home = fakes::TempDir::new("log-name-traversal");
    // Planted where the joined paths would reach, proving they are never
    // read.
    plant(home.path(), "-a", "a/b", &[named(json!("nested"))]);
    plant(home.path(), "-a", "..", &[named(json!("dotdot"))]);
    for odd in ["..", "a/b", "/abs", ""] {
        assert_eq!(session_name(home.path(), &id(odd)), None, "{odd}");
    }
}

#[test]
fn a_line_of_another_kind_with_a_string_name_is_no_name() {
    let home = fakes::TempDir::new("log-name-other");
    plant(home.path(), "-a", SENDER, &[other(), other()]);
    assert_eq!(session_name(home.path(), &id(SENDER)), None);
}

fn started(parent: Option<serde_json::Value>) -> String {
    let mut payload = json!({"workspace": "/w"});
    if let Some(parent) = parent {
        payload["parent"] = parent;
    }
    json!({"kind": "session_started", "payload": payload}).to_string()
}

fn delegated(parent: &str) -> serde_json::Value {
    json!({"session_id": parent, "delegate_id": "j_1"})
}

#[test]
fn a_delegate_reports_its_parent() {
    let home = fakes::TempDir::new("log-parent-delegate");
    plant(
        home.path(),
        "-a",
        SENDER,
        &[started(Some(delegated("s_parent00000001"))), other()],
    );
    assert_eq!(
        session_parent(home.path(), &id(SENDER)),
        Some(id("s_parent00000001"))
    );
}

#[test]
fn a_top_level_session_has_no_parent() {
    let home = fakes::TempDir::new("log-parent-top");
    plant(home.path(), "-a", SENDER, &[started(None), other()]);
    assert_eq!(session_parent(home.path(), &id(SENDER)), None);
}

#[test]
fn a_first_line_without_a_parent_reads_as_no_parent() {
    let home = fakes::TempDir::new("log-parent-absent");
    plant(home.path(), "-a", SENDER, &[named(json!("a"))]);
    assert_eq!(session_parent(home.path(), &id(SENDER)), None);
}

#[test]
fn a_missing_log_has_no_parent() {
    let home = fakes::TempDir::new("log-parent-missing");
    assert_eq!(session_parent(home.path(), &id(SENDER)), None);
}

#[test]
fn a_torn_first_line_reads_as_no_parent() {
    let home = fakes::TempDir::new("log-parent-torn-first");
    let dir = home.path().join("projects/-a/sessions").join(SENDER);
    fs::create_dir_all(&dir).unwrap();
    // No closing newline: a torn write reads as nothing, so the parent
    // it names is not read.
    fs::write(
        dir.join("events.jsonl"),
        started(Some(delegated("s_parent00000001"))),
    )
    .unwrap();
    assert_eq!(session_parent(home.path(), &id(SENDER)), None);
}

#[test]
fn an_unreadable_first_line_has_no_parent() {
    let home = fakes::TempDir::new("log-parent-unreadable");
    plant(home.path(), "-a", SENDER, &["not json".to_owned()]);
    assert_eq!(session_parent(home.path(), &id(SENDER)), None);
}

#[test]
fn a_non_normal_id_reaches_no_parent() {
    let home = fakes::TempDir::new("log-parent-traversal");
    plant(home.path(), "-a", "a/b", &[started(Some(delegated("s_p")))]);
    for odd in ["..", "a/b", "/abs", ""] {
        assert_eq!(session_parent(home.path(), &id(odd)), None, "{odd}");
    }
}

#[test]
fn a_symlinked_session_directory_is_no_session() {
    let home = fakes::TempDir::new("log-name-link");
    let outside = home.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("events.jsonl"),
        format!("{}\n", named(json!("linked"))),
    )
    .unwrap();
    fs::create_dir_all(home.path().join("projects/-a/sessions")).unwrap();
    std::os::unix::fs::symlink(
        &outside,
        home.path().join("projects/-a/sessions").join(SENDER),
    )
    .unwrap();
    assert_eq!(session_name(home.path(), &id(SENDER)), None);
}
