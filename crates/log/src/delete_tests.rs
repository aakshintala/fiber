//! Tests for the session-directory helpers and their identity-safe
//! removal: the join, the followed existence check, the no-follow
//! plain-directory check, and `delete_session`'s log-first removal.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::SessionLock;
use contract::SessionId;

#[test]
fn session_dir_joins_home_projects_key_sessions_and_id() {
    let home = fakes::TempDir::new("log-delete-dir");
    let home = home.path();
    assert_eq!(
        super::super::session_dir(home, "-p", &SessionId("s_1".to_owned())),
        home.join("projects/-p/sessions/s_1")
    );
}

#[test]
fn dir_exists_follows_a_link_to_a_directory() {
    let home = fakes::TempDir::new("log-delete-exists");
    let home = home.path();
    let dir = home.join("dir");
    fs::create_dir(&dir).unwrap();
    assert!(super::super::dir_exists(&dir));
    let link = home.join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(super::super::dir_exists(&link));
    let file = home.join("file");
    fs::write(&file, b"x").unwrap();
    assert!(!super::super::dir_exists(&file));
    assert!(!super::super::dir_exists(&home.join("missing")));
}

#[test]
fn is_plain_dir_refuses_a_link_to_a_directory() {
    let home = fakes::TempDir::new("log-delete-plain");
    let home = home.path();
    let dir = home.join("dir");
    fs::create_dir(&dir).unwrap();
    assert!(is_plain_dir(&dir));
    let link = home.join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(!is_plain_dir(&link));
    let file = home.join("file");
    fs::write(&file, b"x").unwrap();
    assert!(!is_plain_dir(&file));
    assert!(!is_plain_dir(&home.join("missing")));
}

/// A session directory with a log, an `artifacts/` subtree and a lock
/// held through `try_hold`.
fn session(home: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = home.join(name);
    fs::create_dir_all(dir.join("artifacts/nested")).unwrap();
    fs::write(dir.join("events.jsonl"), b"{}\n").unwrap();
    fs::write(dir.join("artifacts/a.txt"), b"bytes").unwrap();
    fs::write(dir.join("artifacts/nested/b.txt"), b"bytes").unwrap();
    dir
}

/// Holds `dir`'s lock, as a session process does.
fn held(dir: &std::path::Path) -> SessionLock {
    match crate::try_hold(dir).unwrap() {
        crate::Hold::Held(lock) => lock,
        crate::Hold::Busy => panic!("the lock is held"),
    }
}

fn not_found(error: &crate::Error, dir: &std::path::Path) -> bool {
    matches!(error, crate::Error::NotFound(path) if path == dir)
}

#[test]
fn delete_session_removes_the_log_the_tree_the_lock_and_the_directory() {
    let home = fakes::TempDir::new("log-delete-remove");
    let home = home.path();
    let dir = session(home, "s_1");
    // A link inside the directory is unlinked; its target keeps files.
    let outside = home.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("kept.txt"), b"kept").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("artifacts/link")).unwrap();
    let sibling = session(home, "s_2");
    let lock = held(&dir);
    delete_session(&dir, &lock).unwrap();
    assert!(
        !dir.exists(),
        "the directory is gone while the lock is alive"
    );
    assert!(outside.join("kept.txt").is_file());
    // The sibling's lock is unaffected by the removal.
    assert!(matches!(
        crate::try_hold(&sibling).unwrap(),
        crate::Hold::Held(_)
    ));
    drop(lock);
}

#[test]
fn delete_session_refuses_a_link_to_a_directory_and_touches_nothing() {
    let home = fakes::TempDir::new("log-delete-link");
    let home = home.path();
    let target = session(home, "target");
    let dir = home.join("s_1");
    std::os::unix::fs::symlink(&target, &dir).unwrap();
    let holder = session(home, "holder");
    let lock = held(&holder);
    let error = delete_session(&dir, &lock).unwrap_err();
    assert!(not_found(&error, &dir), "{error:?}");
    assert!(target.join("events.jsonl").is_file());
    assert!(target.join("artifacts/a.txt").is_file());
}

#[test]
fn delete_session_refuses_a_regular_file_and_does_not_remove_it() {
    let home = fakes::TempDir::new("log-delete-file");
    let home = home.path();
    let dir = home.join("s_1");
    fs::write(&dir, b"bytes").unwrap();
    let holder = session(home, "holder");
    let lock = held(&holder);
    let error = delete_session(&dir, &lock).unwrap_err();
    assert!(not_found(&error, &dir), "{error:?}");
    assert!(dir.is_file());
}

#[test]
fn delete_session_refuses_a_swap_after_the_open_and_removes_nothing() {
    let home = fakes::TempDir::new("log-delete-swap");
    let home = home.path();
    let dir = session(home, "s_1");
    let lock = held(&dir);
    let (fd, identity) = open_session_dir(&dir).unwrap();
    let moved = home.join("s_1_moved");
    fs::rename(&dir, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &dir).unwrap();
    let error = delete_opened(&dir, &fd, identity).unwrap_err();
    assert!(not_found(&error, &dir), "{error:?}");
    assert!(moved.join("events.jsonl").is_file());
    drop(lock);
}

#[test]
fn delete_session_without_a_log_is_io_naming_the_log_and_removes_nothing() {
    let home = fakes::TempDir::new("log-delete-nolog");
    let home = home.path();
    let dir = home.join("s_1");
    fs::create_dir(&dir).unwrap();
    let lock = held(&dir);
    let error = delete_session(&dir, &lock).unwrap_err();
    match &error {
        crate::Error::Io { path, .. } => assert_eq!(path, &dir.join("events.jsonl")),
        crate::Error::NotFound(_)
        | crate::Error::Held { .. }
        | crate::Error::Poisoned { .. }
        | crate::Error::Ended { .. }
        | crate::Error::Ambiguous { .. }
        | crate::Error::Exists(_)
        | crate::Error::Pointer { .. }
        | crate::Error::Unreadable { .. }
        | crate::Error::Encode(_) => panic!("a missing log is Io: {error:?}"),
    }
    assert!(dir.is_dir());
    assert!(dir.join("session.lock").is_file());
    drop(lock);
}

#[test]
fn delete_session_in_a_read_only_directory_is_io_and_keeps_the_log() {
    let home = fakes::TempDir::new("log-delete-readonly");
    let home = home.path();
    let dir = session(home, "s_1");
    let lock = held(&dir);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let error = delete_session(&dir, &lock).unwrap_err();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(error, crate::Error::Io { .. }), "{error:?}");
    assert!(dir.join("events.jsonl").is_file());
    drop(lock);
}
