//! Tests for [`SessionDir`](super::SessionDir): the session directory
//! join and the directory check behind resume and rewind.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;

use super::*;
use contract::SessionId;

fn sessions_in(home: &std::path::Path) -> std::path::PathBuf {
    home.join("sessions")
}

#[test]
fn first_line_errors_display_a_readable_reason() {
    assert_eq!(
        FirstLineError::Incomplete.to_string(),
        "the session's first line is missing or incomplete"
    );
    assert_eq!(
        FirstLineError::Corrupt.to_string(),
        "the session's first line is corrupt"
    );
}

#[test]
fn sessions_dir_for_key_resolves_a_plain_key_under_projects() {
    let home = fakes::TempDir::new("log-sessions-dir-key");
    assert_eq!(
        sessions_dir_for_key(home.path(), "-tmp-repo"),
        Some(
            home.path()
                .join("projects")
                .join("-tmp-repo")
                .join("sessions")
        )
    );
}

#[test]
fn sessions_dir_for_key_matches_sessions_dir_for_a_real_project_key() {
    let home = fakes::TempDir::new("log-sessions-dir-key-match");
    let workspace = home.path().join("somewhere/repo");
    let key = project_key(&workspace);
    assert_eq!(
        sessions_dir_for_key(home.path(), &key),
        Some(sessions_dir(home.path(), &workspace))
    );
}

#[test]
fn sessions_dir_for_key_refuses_a_key_that_leaves_projects() {
    let home = fakes::TempDir::new("log-sessions-dir-key-bad");
    for key in ["", ".", "..", "a/b", "/abs", "a/../b"] {
        assert_eq!(
            sessions_dir_for_key(home.path(), key),
            None,
            "key {key:?} must not resolve"
        );
    }
}

fn id() -> SessionId {
    SessionId("s_1".to_owned())
}

fn not_found(error: &Error, dir: &std::path::Path) -> bool {
    matches!(error, Error::NotFound(path) if path == dir)
}

fn io_at(error: &Error, dir: &std::path::Path) -> bool {
    matches!(error, Error::Io { path, .. } if path == dir)
}

#[test]
fn new_names_the_plain_session_directory_join() {
    let home = fakes::TempDir::new("log-session-dir-join");
    let sessions = sessions_in(home.path());
    let dir = SessionDir::new(&sessions, &id());
    assert_eq!(dir.path(), sessions.join("s_1"));
}

#[test]
fn events_and_artifacts_name_the_log_and_the_tree() {
    let home = fakes::TempDir::new("log-session-dir-values");
    let sessions = sessions_in(home.path());
    let dir = SessionDir::new(&sessions, &id());
    assert_eq!(dir.events(), sessions.join("s_1").join("events.jsonl"));
    assert_eq!(dir.artifacts(), sessions.join("s_1").join("artifacts"));
}

#[test]
fn require_dir_passes_on_a_real_directory() {
    let home = fakes::TempDir::new("log-session-dir-ok");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    let dir = SessionDir::new(&sessions, &id());
    fs::create_dir(dir.path()).unwrap();
    dir.require_dir().unwrap();
}

#[test]
fn require_dir_on_a_missing_directory_is_not_found_naming_it() {
    let home = fakes::TempDir::new("log-session-dir-missing");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    let dir = SessionDir::new(&sessions, &id());
    let error = dir.require_dir().unwrap_err();
    assert!(
        not_found(&error, &sessions.join("s_1")),
        "a missing directory is NotFound: {error:?}"
    );
    assert_eq!(error.code(), contract::ErrorCode::SessionNotFound);
}

#[test]
fn require_dir_on_a_file_as_session_is_not_found_naming_it() {
    let home = fakes::TempDir::new("log-session-dir-file");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("s_1"), b"bytes").unwrap();
    let dir = SessionDir::new(&sessions, &id());
    let error = dir.require_dir().unwrap_err();
    assert!(
        not_found(&error, &sessions.join("s_1")),
        "a file as the session is NotFound: {error:?}"
    );
    assert_eq!(error.code(), contract::ErrorCode::SessionNotFound);
}

#[cfg(unix)]
#[test]
fn require_dir_follows_a_link_to_a_directory() {
    let home = fakes::TempDir::new("log-session-dir-link");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    let dir = SessionDir::new(&sessions, &id());
    fs::create_dir(dir.path()).unwrap();
    let link = sessions.join("s_link");
    std::os::unix::fs::symlink(dir.path(), &link).unwrap();
    SessionDir::new(&sessions, &SessionId("s_link".to_owned()))
        .require_dir()
        .unwrap();
}

#[cfg(unix)]
#[test]
fn require_dir_on_a_dangling_link_is_not_found_naming_it() {
    let home = fakes::TempDir::new("log-session-dir-dangling");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    let missing = sessions.join("gone");
    let link = SessionDir::new(&sessions, &SessionId("s_link".to_owned()));
    std::os::unix::fs::symlink(&missing, link.path()).unwrap();
    let error = link.require_dir().unwrap_err();
    assert!(
        not_found(&error, link.path()),
        "a dangling link is NotFound: {error:?}"
    );
    assert_eq!(error.code(), contract::ErrorCode::SessionNotFound);
}

#[test]
fn dir_is_at_matches_the_directory_at_its_expected_place() {
    let home = fakes::TempDir::new("log-dir-is-at-match");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    let dir = SessionDir::new(&sessions, &id());
    fs::create_dir(dir.path()).unwrap();
    let expected = fs::canonicalize(dir.path()).unwrap();
    assert!(dir_is_at(dir.path(), &expected));
}

#[test]
fn dir_is_at_refuses_a_different_directory() {
    let home = fakes::TempDir::new("log-dir-is-at-other");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    let dir = SessionDir::new(&sessions, &id());
    fs::create_dir(dir.path()).unwrap();
    let other = SessionDir::new(&sessions, &SessionId("s_2".to_owned()));
    fs::create_dir(other.path()).unwrap();
    let dir_expected = fs::canonicalize(dir.path()).unwrap();
    let other_expected = fs::canonicalize(other.path()).unwrap();
    assert!(!dir_is_at(dir.path(), &other_expected));
    assert!(!dir_is_at(other.path(), &dir_expected));
}

#[test]
fn dir_is_at_refuses_a_missing_directory() {
    let home = fakes::TempDir::new("log-dir-is-at-missing");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    let dir = SessionDir::new(&sessions, &id());
    let expected = fs::canonicalize(&sessions).unwrap().join("s_1");
    assert!(!dir_is_at(dir.path(), &expected));
}

#[cfg(unix)]
#[test]
fn dir_is_at_compares_the_link_target_with_the_expected_place() {
    let home = fakes::TempDir::new("log-dir-is-at-link");
    let sessions = sessions_in(home.path());
    fs::create_dir_all(&sessions).unwrap();
    let dir = SessionDir::new(&sessions, &id());
    fs::create_dir(dir.path()).unwrap();
    let other = SessionDir::new(&sessions, &SessionId("s_2".to_owned()));
    fs::create_dir(other.path()).unwrap();
    let expected = fs::canonicalize(dir.path()).unwrap();
    let other_expected = fs::canonicalize(other.path()).unwrap();
    let link = sessions.join("s_link");
    std::os::unix::fs::symlink(&expected, &link).unwrap();
    assert!(dir_is_at(&link, &expected));
    assert!(!dir_is_at(&link, &other_expected));
}

#[test]
fn require_dir_with_a_file_as_parent_is_io_naming_the_directory() {
    let home = fakes::TempDir::new("log-session-dir-parent");
    let sessions = sessions_in(home.path());
    fs::write(&sessions, b"bytes").unwrap();
    let dir = SessionDir::new(&sessions, &id());
    let error = dir.require_dir().unwrap_err();
    assert!(
        io_at(&error, &sessions.join("s_1")),
        "a file as the sessions parent is Io: {error:?}"
    );
    assert_eq!(error.code(), contract::ErrorCode::IoFailed);
}
