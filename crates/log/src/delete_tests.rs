//! Tests for the session-directory helpers: the join, the followed
//! existence check, and the no-follow plain-directory check.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;

use super::*;

#[test]
fn session_dir_joins_home_projects_key_sessions_and_id() {
    let home = fakes::TempDir::new("log-delete-dir");
    let home = home.path();
    assert_eq!(
        super::super::session_dir(home, "-p", "s_1"),
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
