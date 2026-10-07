//! The `--resume` selector (`docs/invocation.md`, "Lifecycle").

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::fs;

use common::{event, tool_call_started};
use contract::SessionId;
use contract::events::Event;
use log::Log;
use serde_json::json;

fn setup() -> (fakes::TempDir, std::path::PathBuf) {
    let root = fakes::TempDir::new("fiber-resolve");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    (root, sessions)
}

/// Session `id` whose log begins with `session_started` in `workspace`.
fn session(sessions: &std::path::Path, id: &str, workspace: &str) {
    let log = Log::create(
        sessions,
        SessionId(id.into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(&started(workspace), None, None).unwrap();
}

fn started(workspace: &str) -> Event {
    event(
        "session_started",
        json!({"workspace": workspace,
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}}),
    )
}

/// Session `id` with an empty log: `Log::create` before any write, as a
/// crash between them leaves it.
fn empty(sessions: &std::path::Path, id: &str) {
    Log::create(
        sessions,
        SessionId(id.into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
}

#[test]
fn full_id_resolves() {
    let (_root, sessions) = setup();
    session(&sessions, "s_abc", "/w");
    session(&sessions, "s_def", "/w");

    assert_eq!(
        log::resolve(&sessions, "s_abc", &|_| true).unwrap().0,
        "s_abc"
    );
}

#[test]
fn unique_prefix_resolves() {
    let (_root, sessions) = setup();
    session(&sessions, "s_abc", "/w");
    session(&sessions, "s_def", "/w");

    assert_eq!(
        log::resolve(&sessions, "s_a", &|_| true).unwrap().0,
        "s_abc"
    );
}

#[test]
fn exact_id_wins_over_a_longer_name_it_prefixes() {
    let (_root, sessions) = setup();
    session(&sessions, "s_ab", "/w");
    session(&sessions, "s_abc", "/w");

    assert_eq!(
        log::resolve(&sessions, "s_ab", &|_| true).unwrap().0,
        "s_ab"
    );
}

#[test]
fn two_matches_are_ambiguous_sorted() {
    let (_root, sessions) = setup();
    session(&sessions, "s_b2", "/w");
    session(&sessions, "s_b1", "/w");
    session(&sessions, "s_c", "/w");

    match log::resolve(&sessions, "s_b", &|_| true) {
        Err(log::Error::Ambiguous { selector, matches }) => {
            assert_eq!(selector, "s_b");
            assert_eq!(matches, vec!["s_b1".to_owned(), "s_b2".to_owned()]);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn no_match_is_not_found() {
    let (_root, sessions) = setup();
    session(&sessions, "s_abc", "/w");

    assert!(matches!(
        log::resolve(&sessions, "s_zzz", &|_| true),
        Err(log::Error::NotFound(_))
    ));
}

#[test]
fn a_directory_without_events_jsonl_is_ignored() {
    let (_root, sessions) = setup();
    fs::create_dir_all(sessions.join("s_ghost")).unwrap();
    session(&sessions, "s_real", "/w");

    assert!(matches!(
        log::resolve(&sessions, "s_ghost", &|_| true),
        Err(log::Error::NotFound(_))
    ));
    assert_eq!(
        log::resolve(&sessions, "s_r", &|_| true).unwrap().0,
        "s_real"
    );
    assert_eq!(
        log::resolve(&sessions, "s_", &|_| true).unwrap().0,
        "s_real"
    );
}

#[test]
fn empty_and_path_selectors_match_nothing() {
    let (_root, sessions) = setup();
    session(&sessions, "s_abc", "/w");
    // A directory outside the sessions directory that a traversal could
    // reach. The selector is only compared with listed names, never joined
    // into a path, so these match nothing and read nothing outside.
    let outside = sessions.parent().unwrap().join("s_abc");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("events.jsonl"), "").unwrap();

    for selector in ["", "s_a/b", "../s_abc", "..", "s_abc/..", "s_..x", "\\"] {
        assert!(
            matches!(
                log::resolve(&sessions, selector, &|_| true),
                Err(log::Error::NotFound(_))
            ),
            "{selector:?}"
        );
    }
    // The traversal target is untouched.
    assert!(outside.is_dir());
}

#[test]
fn ambiguous_maps_to_usage() {
    let (_root, sessions) = setup();
    session(&sessions, "s_b1", "/w");
    session(&sessions, "s_b2", "/w");

    let Err(e) = log::resolve(&sessions, "s_b", &|_| true) else {
        panic!("expected Ambiguous");
    };
    assert_eq!(e.code(), contract::ErrorCode::Usage);
}

#[test]
fn an_empty_log_is_ignored() {
    let (_root, sessions) = setup();
    empty(&sessions, "s_ab1");
    session(&sessions, "s_ab2", "/w");

    // The empty log is not found by its own id, and does not make the
    // shared prefix ambiguous with the real session.
    assert!(matches!(
        log::resolve(&sessions, "s_ab1", &|_| true),
        Err(log::Error::NotFound(_))
    ));
    assert_eq!(
        log::resolve(&sessions, "s_ab", &|_| true).unwrap().0,
        "s_ab2"
    );
}

#[test]
fn a_log_whose_first_line_is_not_session_started_is_ignored() {
    let (_root, sessions) = setup();
    let log = Log::create(
        &sessions,
        SessionId("s_xx1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(&tool_call_started(), None, None).unwrap();
    session(&sessions, "s_xx2", "/w");

    assert!(matches!(
        log::resolve(&sessions, "s_xx1", &|_| true),
        Err(log::Error::NotFound(_))
    ));
    assert_eq!(
        log::resolve(&sessions, "s_xx", &|_| true).unwrap().0,
        "s_xx2"
    );
}

#[test]
fn only_sessions_in_this_project_resolve() {
    let (_root, sessions) = setup();
    // `/a-b` and `/a/b` share the slug `-a-b` (`docs/state.md`,
    // "Projects"), so both sessions sit in this one directory.
    session(&sessions, "s_x1", "/a-b");
    session(&sessions, "s_x2", "/a/b");
    let in_project = |started: &str| started == "/a/b";

    // The other workspace's id is not found, and the shared prefix is not
    // ambiguous: it resolves to the one session in this project.
    assert!(matches!(
        log::resolve(&sessions, "s_x1", &in_project),
        Err(log::Error::NotFound(_))
    ));
    assert_eq!(
        log::resolve(&sessions, "s_x", &in_project).unwrap().0,
        "s_x2"
    );
    assert_eq!(
        log::resolve(&sessions, "s_x2", &in_project).unwrap().0,
        "s_x2"
    );
}

#[test]
fn a_torn_first_line_is_ignored() {
    let (_root, sessions) = setup();
    let dir = sessions.join("s_t1");
    fs::create_dir_all(&dir).unwrap();
    // A crash mid-line: the first line has no newline, so there is no
    // first complete line.
    fs::write(dir.join("events.jsonl"), "{\"kind\": \"session_star").unwrap();
    session(&sessions, "s_t2", "/w");

    assert!(matches!(
        log::resolve(&sessions, "s_t1", &|_| true),
        Err(log::Error::NotFound(_))
    ));
    assert_eq!(log::resolve(&sessions, "s_t", &|_| true).unwrap().0, "s_t2");
}
