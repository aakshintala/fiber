//! The `--resume` selector (`docs/invocation.md`, "Lifecycle").

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::fs;

use contract::SessionId;
use log::Log;

fn setup() -> (fakes::TempDir, std::path::PathBuf) {
    let root = fakes::TempDir::new("fiber-resolve");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    (root, sessions)
}

fn session(sessions: &std::path::Path, id: &str) {
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
    session(&sessions, "s_abc");
    session(&sessions, "s_def");

    assert_eq!(log::resolve(&sessions, "s_abc").unwrap().0, "s_abc");
}

#[test]
fn unique_prefix_resolves() {
    let (_root, sessions) = setup();
    session(&sessions, "s_abc");
    session(&sessions, "s_def");

    assert_eq!(log::resolve(&sessions, "s_a").unwrap().0, "s_abc");
}

#[test]
fn exact_id_wins_over_a_longer_name_it_prefixes() {
    let (_root, sessions) = setup();
    session(&sessions, "s_ab");
    session(&sessions, "s_abc");

    assert_eq!(log::resolve(&sessions, "s_ab").unwrap().0, "s_ab");
}

#[test]
fn two_matches_are_ambiguous_sorted() {
    let (_root, sessions) = setup();
    session(&sessions, "s_b2");
    session(&sessions, "s_b1");
    session(&sessions, "s_c");

    match log::resolve(&sessions, "s_b") {
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
    session(&sessions, "s_abc");

    assert!(matches!(
        log::resolve(&sessions, "s_zzz"),
        Err(log::Error::NotFound(_))
    ));
}

#[test]
fn a_directory_without_events_jsonl_is_ignored() {
    let (_root, sessions) = setup();
    fs::create_dir_all(sessions.join("s_ghost")).unwrap();
    session(&sessions, "s_real");

    assert!(matches!(
        log::resolve(&sessions, "s_ghost"),
        Err(log::Error::NotFound(_))
    ));
    assert_eq!(log::resolve(&sessions, "s_r").unwrap().0, "s_real");
    assert_eq!(log::resolve(&sessions, "s_").unwrap().0, "s_real");
}

#[test]
fn empty_and_path_selectors_match_nothing() {
    let (_root, sessions) = setup();
    session(&sessions, "s_abc");
    // A directory outside the sessions directory that a traversal could reach.
    let outside = sessions.parent().unwrap().join("s_abc");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("events.jsonl"), "").unwrap();

    for selector in ["", "s_a/b", "../s_abc", "..", "s_abc/..", "s_..x"] {
        if selector == "s_..x" {
            // Contains ".." so refused even though nothing matches anyway.
            assert!(matches!(
                log::resolve(&sessions, selector),
                Err(log::Error::NotFound(_))
            ));
            continue;
        }
        assert!(
            matches!(
                log::resolve(&sessions, selector),
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
    session(&sessions, "s_b1");
    session(&sessions, "s_b2");

    let Err(e) = log::resolve(&sessions, "s_b") else {
        panic!("expected Ambiguous");
    };
    assert_eq!(e.code(), contract::ErrorCode::Usage);
}
