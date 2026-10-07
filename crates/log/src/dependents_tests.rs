use std::fs;
use std::path::Path;

use super::*;

/// Writes `first` as the first line of session `id`'s log in `project`.
fn session(home: &Path, project: &str, id: &str, first: &str) {
    let dir = home
        .join("projects")
        .join(project)
        .join("sessions")
        .join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        format!("{first}\n{{\"kind\":\"x\"}}\n"),
    )
    .unwrap();
}

/// A `session_started` first line, forked from `from` when given.
fn started(from: Option<&str>) -> String {
    let mut payload = serde_json::json!({"workspace": "/w"});
    if let Some(from) = from {
        payload["forked_from"] = serde_json::json!({"session_id": from, "seq": 3});
    }
    serde_json::json!({"kind": "session_started", "seq": 0, "payload": payload}).to_string()
}

fn sid(id: &str) -> SessionId {
    SessionId(id.to_owned())
}

fn ids(found: &[SessionId]) -> Vec<&str> {
    found.iter().map(|id| id.0.as_str()).collect()
}

#[test]
fn forks_rewinds_and_their_forks_across_projects_are_found_breadth_first() {
    let home = fakes::TempDir::new("log-dependents");
    let home = home.path();
    session(home, "-a", "s_root", &started(None));
    // A fork in another project, and a rewind (same pointer) in this one.
    session(home, "-b", "s_fork", &started(Some("s_root")));
    session(home, "-a", "s_rewind", &started(Some("s_root")));
    // A fork of the fork: transitive, found after both direct ones.
    session(home, "-a", "s_afork", &started(Some("s_fork")));
    session(home, "-a", "s_other", &started(None));
    session(home, "-b", "s_elsewhere", &started(Some("s_other")));
    assert_eq!(
        ids(&dependents(home, &sid("s_root"))),
        ["s_fork", "s_rewind", "s_afork"]
    );
    assert_eq!(ids(&dependents(home, &sid("s_fork"))), ["s_afork"]);
    assert!(dependents(home, &sid("s_afork")).is_empty());
}

#[test]
fn a_first_line_that_is_not_session_started_or_unreadable_is_skipped() {
    let home = fakes::TempDir::new("log-dependents-skip");
    let home = home.path();
    session(home, "-a", "s_root", &started(None));
    let pointer = serde_json::json!({
        "kind": "turn_started",
        "payload": {"forked_from": {"session_id": "s_root", "seq": 1}},
    });
    session(home, "-a", "s_wrong_kind", &pointer.to_string());
    session(home, "-a", "s_garbage", "not json");
    session(
        home,
        "-a",
        "s_no_string",
        r#"{"kind":"session_started","payload":{"forked_from":{"session_id":7}}}"#,
    );
    // An empty log, and a directory with no log at all.
    let empty = home.join("projects/-a/sessions/s_empty");
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join("events.jsonl"), b"").unwrap();
    fs::create_dir_all(home.join("projects/-a/sessions/s_nolog")).unwrap();
    // A project with no `sessions/`, and a stray file in `projects/`.
    fs::create_dir_all(home.join("projects/-empty")).unwrap();
    fs::write(home.join("projects/stray"), b"x").unwrap();
    session(home, "-a", "s_real", &started(Some("s_root")));
    assert_eq!(ids(&dependents(home, &sid("s_root"))), ["s_real"]);
}

#[test]
fn a_cycle_or_a_self_pointer_terminates_and_never_names_the_session() {
    let home = fakes::TempDir::new("log-dependents-cycle");
    let home = home.path();
    session(home, "-a", "s_a", &started(Some("s_b")));
    session(home, "-a", "s_b", &started(Some("s_a")));
    session(home, "-a", "s_self", &started(Some("s_self")));
    assert_eq!(ids(&dependents(home, &sid("s_a"))), ["s_b"]);
    assert!(dependents(home, &sid("s_self")).is_empty());
}

#[test]
fn a_session_directory_that_is_a_link_is_not_a_session() {
    let home = fakes::TempDir::new("log-dependents-link");
    let home = home.path();
    session(home, "-a", "s_root", &started(None));
    let outside = home.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("events.jsonl"),
        format!("{}\n", started(Some("s_root"))),
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, home.join("projects/-a/sessions/s_link")).unwrap();
    assert!(dependents(home, &sid("s_root")).is_empty());
}

#[test]
fn a_home_with_no_projects_has_no_dependents() {
    let home = fakes::TempDir::new("log-dependents-none");
    assert!(dependents(home.path(), &sid("s_root")).is_empty());
}
