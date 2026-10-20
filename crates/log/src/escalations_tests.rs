//! Tests for the escalation read and its narrow scan path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
use crate::started_sessions;

fn write_log(dir: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), bytes).unwrap();
}

fn session_dir(home: &Path, project: &str, id: &str) -> PathBuf {
    home.join("projects")
        .join(project)
        .join("sessions")
        .join(id)
}

fn started_line(payload: &serde_json::Value) -> String {
    json!({"kind": "session_started", "payload": payload}).to_string() + "\n"
}

fn line(kind: &str, session: &str, seq: u64, payload: &serde_json::Value) -> String {
    json!({
        "kind": kind,
        "session_id": session,
        "ts": seq,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    })
    .to_string()
        + "\n"
}

fn request(
    id: &str,
    escalation: &serde_json::Value,
    rule: &serde_json::Value,
) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    payload.insert("request_id".into(), json!(id));
    payload.insert("step".into(), json!("review"));
    payload.insert("escalation".into(), escalation.clone());
    payload.insert("rule".into(), rule.clone());
    serde_json::Value::Object(payload)
}

fn blocks() -> serde_json::Value {
    json!({"cause": "consecutive_blocks", "reason": "blocked twice"})
}

fn session_blocks() -> serde_json::Value {
    json!({"cause": "session_blocks", "reason": "blocked often"})
}

fn reviewer_failed() -> serde_json::Value {
    json!({"cause": "reviewer_failed", "error": {"code": "io_failed", "message": "down"}})
}

fn rule(prefix: &str) -> serde_json::Value {
    json!({"subject": prefix, "prefix": prefix})
}

fn resolved(id: Option<&str>, decision: &str, decided_by: &str) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    if let Some(id) = id {
        payload.insert("request_id".into(), json!(id));
    }
    payload.insert("decision".into(), json!(decision));
    payload.insert("decided_by".into(), json!(decided_by));
    serde_json::Value::Object(payload)
}

fn setup_behavior() -> fakes::TempDir {
    let home = fakes::TempDir::new("log-escalations");
    let root = home.path();
    // Session one, in proj-a, records its workspace and holds every
    // skipped kind beside one counted escalation.
    let mut first = started_line(&json!({"workspace": "/repo-a"}));
    first.push_str(&line(
        "turn_started",
        "s_02",
        1,
        &json!({"prompt": "allow gh pr merge --squash"}),
    ));
    first.push_str(&line(
        "permission_requested",
        "s_02",
        2,
        &request("r1", &blocks(), &rule("gh pr merge")),
    ));
    // No escalation: skipped, even with an allow resolution after it
    // (appended at the end, in `seq` order).
    let mut no_escalation = serde_json::Map::new();
    no_escalation.insert("request_id".into(), json!("r2"));
    no_escalation.insert("step".into(), json!("review"));
    no_escalation.insert("rule".into(), rule("gh pr merge"));
    first.push_str(&line(
        "permission_requested",
        "s_02",
        3,
        &serde_json::Value::Object(no_escalation),
    ));
    // A reviewer failure answered allow: not counted.
    first.push_str(&line(
        "permission_requested",
        "s_02",
        4,
        &request("r3", &reviewer_failed(), &rule("gh pr merge")),
    ));
    let mut with_hosts = resolved(Some("r1"), "allow", "person");
    with_hosts
        .as_object_mut()
        .unwrap()
        .insert("hosts".into(), json!(["https://host.example"]));
    first.push_str(&line("permission_resolved", "s_02", 5, &with_hosts));
    first.push_str(&line(
        "permission_resolved",
        "s_02",
        6,
        &resolved(Some("r3"), "allow", "person"),
    ));
    // Answered deny: not counted.
    first.push_str(&line(
        "permission_requested",
        "s_02",
        7,
        &request("r4", &session_blocks(), &rule("npm test")),
    ));
    first.push_str(&line(
        "permission_resolved",
        "s_02",
        8,
        &resolved(Some("r4"), "deny", "person"),
    ));
    // Answered allow by the reviewer: not counted.
    first.push_str(&line(
        "permission_requested",
        "s_02",
        9,
        &request("r5", &blocks(), &rule("git push")),
    ));
    first.push_str(&line(
        "permission_resolved",
        "s_02",
        10,
        &resolved(Some("r5"), "allow", "reviewer"),
    ));
    // No rule: skipped, so its allow resolution counts nothing.
    let mut no_rule = serde_json::Map::new();
    no_rule.insert("request_id".into(), json!("r6"));
    no_rule.insert("step".into(), json!("review"));
    no_rule.insert("escalation".into(), blocks());
    first.push_str(&line(
        "permission_requested",
        "s_02",
        11,
        &serde_json::Value::Object(no_rule),
    ));
    first.push_str(&line(
        "permission_resolved",
        "s_02",
        12,
        &resolved(Some("r6"), "allow", "person"),
    ));
    // No matching request: skipped.
    first.push_str(&line(
        "permission_resolved",
        "s_02",
        13,
        &resolved(Some("r7"), "allow", "person"),
    ));
    first.push_str(&line(
        "permission_resolved",
        "s_02",
        14,
        &resolved(None, "allow", "person"),
    ));
    // A second resolution for the counted request counts nothing more.
    first.push_str(&line(
        "permission_resolved",
        "s_02",
        15,
        &resolved(Some("r1"), "allow", "person"),
    ));
    first.push_str(&line(
        "permission_resolved",
        "s_02",
        16,
        &resolved(Some("r2"), "allow", "person"),
    ));
    write_log(&session_dir(root, "proj-a", "s_02"), first.as_bytes());
    // Session two, in proj-b, records a worktree repository beside its
    // workspace: the repository is its identity.
    let mut second = started_line(&json!({
        "workspace": "/checkout",
        "worktree": {"path": "/checkout", "branch": "fiber/s", "repository": "/repo-b"},
    }));
    second.push_str(&line(
        "permission_requested",
        "s_01",
        1,
        &request("q1", &blocks(), &rule("cargo test")),
    ));
    second.push_str(&line(
        "permission_resolved",
        "s_01",
        2,
        &resolved(Some("q1"), "allow", "person"),
    ));
    write_log(&session_dir(root, "proj-b", "s_01"), second.as_bytes());
    // Session three records neither field: no identity.
    let mut third = started_line(&json!({}));
    third.push_str(&line(
        "permission_requested",
        "s_04",
        1,
        &request("t1", &blocks(), &rule("make lint")),
    ));
    third.push_str(&line(
        "permission_resolved",
        "s_04",
        2,
        &resolved(Some("t1"), "allow", "person"),
    ));
    write_log(&session_dir(root, "proj-c", "s_04"), third.as_bytes());
    // Session four ends mid-line: what came before still counts.
    let mut fourth = started_line(&json!({"workspace": "/repo-a"}));
    fourth.push_str(&line(
        "permission_requested",
        "s_03",
        1,
        &request("u1", &blocks(), &rule("gh pr merge")),
    ));
    fourth.push_str(&line(
        "permission_resolved",
        "s_03",
        2,
        &resolved(Some("u1"), "allow", "person"),
    ));
    fourth.push_str("{\"kind\": \"permission_requested\", \"pay");
    write_log(&session_dir(root, "proj-a", "s_03"), fourth.as_bytes());
    home
}

#[test]
fn only_allows_a_person_answered_come_back_with_project_and_identity() {
    let home = setup_behavior();
    let found = allowed_escalations(home.path());
    assert_eq!(
        found,
        vec![
            AllowedEscalation {
                project: "proj-b".into(),
                identity: Some("/repo-b".into()),
                prefix: "cargo test".into(),
            },
            AllowedEscalation {
                project: "proj-a".into(),
                identity: Some("/repo-a".into()),
                prefix: "gh pr merge".into(),
            },
            AllowedEscalation {
                project: "proj-a".into(),
                identity: Some("/repo-a".into()),
                prefix: "gh pr merge".into(),
            },
            AllowedEscalation {
                project: "proj-c".into(),
                identity: None,
                prefix: "make lint".into(),
            },
        ]
    );
}

#[test]
fn a_home_with_no_projects_directory_gives_nothing() {
    let home = fakes::TempDir::new("log-escalations-empty");
    assert!(allowed_escalations(home.path()).is_empty());
}

fn setup_population() -> fakes::TempDir {
    let home = fakes::TempDir::new("log-escalations-place");
    let root = home.path();
    let dir = |project: &str, id: &str| session_dir(root, project, id);
    write_log(
        &dir("proj", "s_01"),
        started_line(&json!({"workspace": "/w"})).as_bytes(),
    );
    std::os::unix::fs::symlink(dir("proj", "s_01"), dir("proj", "s_02")).unwrap();
    std::fs::create_dir_all(root.join("projects").join("projx")).unwrap();
    write_log(
        &dir("proj", "s_03"),
        b"{\"kind\":\"turn_started\",\"payload\":{}}\n",
    );
    write_log(&dir("proj", "s_04"), b"{\"kind\":\"session_started\"}\n");
    write_log(
        &dir("proj", "s_05"),
        started_line(&json!({"workspace": 7})).as_bytes(),
    );
    write_log(&dir("proj", "s_06"), started_line(&json!({})).as_bytes());
    write_log(
        &dir("proj", "s_07"),
        b"{\"kind\":\"session_started\",\"payload\":null}\n",
    );
    write_log(
        &dir("proj", "s_08"),
        b"{\"kind\":\"session_started\",\"payload\":{\"worktree\":7}}\n",
    );
    write_log(
        &dir("proj", "s_09"),
        b"{\"kind\":\"session_started\",\"payload\":{\"workspace\":\"/repo\",\"worktree\":7}}\n",
    );
    write_log(
        &dir("proj", "s_10"),
        format!(
            "{}\n",
            json!({"kind": "session_started", "payload": {
                "workspace": "/p",
                "parent": {"session_id": "PARENT_MARKER_DISTINCT"},
                "forked_from": {"session_id": "FORK_MARKER_DISTINCT", "seq": 1},
            }})
        )
        .as_bytes(),
    );
    write_log(
        &dir("proj", "s_11"),
        started_line(&json!({"workspace": "/p"})).as_bytes(),
    );
    home
}

fn places(home: &Path) -> Vec<super::place::Place> {
    super::place::sessions(home)
}

#[test]
fn the_narrow_scan_lists_the_wide_scans_sessions_in_its_order() {
    let home = setup_population();
    let wide: Vec<PathBuf> = started_sessions(home.path())
        .iter()
        .map(|s| s.dir.clone())
        .collect();
    let narrow: Vec<PathBuf> = places(home.path()).iter().map(|p| p.dir.clone()).collect();
    assert_eq!(narrow, wide);
    assert_eq!(
        narrow
            .iter()
            .map(|dir| dir.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [
            "s_01", "s_05", "s_06", "s_07", "s_08", "s_09", "s_10", "s_11"
        ]
    );
}

#[test]
fn malformed_identities_read_as_no_identity_without_dropping_the_session() {
    let home = setup_population();
    let found = places(home.path());
    let identity = |id: &str| {
        found
            .iter()
            .find(|p| p.dir.file_name().unwrap() == id)
            .unwrap()
            .identity
            .clone()
    };
    assert_eq!(identity("s_01"), Some("/w".into()));
    for id in ["s_05", "s_06", "s_07", "s_08"] {
        assert_eq!(identity(id), None, "{id}");
    }
    assert_eq!(identity("s_09"), Some("/repo".into()));
}

#[test]
fn parent_and_forked_from_are_not_read_into_the_result() {
    let home = setup_population();
    let found = places(home.path());
    let pointed = found
        .iter()
        .find(|p| p.dir.file_name().unwrap() == "s_10")
        .unwrap();
    let plain = found
        .iter()
        .find(|p| p.dir.file_name().unwrap() == "s_11")
        .unwrap();
    assert_eq!(pointed.project, plain.project);
    assert_eq!(pointed.identity, plain.identity);
    assert_ne!(pointed.dir, plain.dir);
    let debug = format!("{pointed:?}");
    assert!(!debug.contains("PARENT_MARKER_DISTINCT"), "{debug}");
    assert!(!debug.contains("FORK_MARKER_DISTINCT"), "{debug}");
}
