//! Tests for `git worktree list`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use fakes::git::{git, repo};

/// Adds a worktree on a new branch `branch` at `name` under the repository.
fn add(repo: &Path, branch: &str, name: &str) -> PathBuf {
    let path = repo.join(name);
    git(
        repo,
        &["worktree", "add", "-b", branch, path.to_str().unwrap()],
    );
    path
}

#[test]
fn main_plus_a_linked_worktree_lists_both_main_first() {
    let home = repo("worktree-list-main");
    let linked = add(home.path(), "fiber/x", "wt");
    let listed = list(home.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed[0].path.canonicalize().unwrap(),
        home.path().canonicalize().unwrap()
    );
    assert!(!listed[0].bare);
    assert!(!listed[0].locked);
    assert_eq!(
        listed[1].path.canonicalize().unwrap(),
        linked.canonicalize().unwrap()
    );
    assert!(!listed[1].bare);
    assert!(!listed[1].locked);
}

#[test]
fn a_locked_worktree_reports_locked() {
    let home = repo("worktree-list-locked");
    let linked = add(home.path(), "fiber/x", "wt");
    git(
        home.path(),
        &[
            "worktree",
            "lock",
            "--reason",
            "a b",
            linked.to_str().unwrap(),
        ],
    );
    let listed = list(home.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(!listed[0].locked);
    assert!(listed[1].locked);
}

#[test]
fn a_removed_directory_is_still_listed_with_its_path() {
    let home = repo("worktree-list-removed");
    let linked = add(home.path(), "fiber/x", "wt");
    fs::remove_dir_all(&linked).unwrap();
    let listed = list(home.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].path.file_name().unwrap(), "wt");
    assert_eq!(
        listed[1].path.parent().unwrap().canonicalize().unwrap(),
        home.path().canonicalize().unwrap()
    );
    assert!(listed[1].path.symlink_metadata().is_err());
}

#[test]
fn a_bare_repository_lists_one_bare_entry() {
    let home = fakes::TempDir::new("worktree-list-bare");
    git(home.path(), &["init", "--quiet", "--bare"]);
    let listed = list(home.path()).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].bare);
}

#[test]
fn a_path_with_a_space_parses_whole() {
    let home = repo("worktree-list-space");
    let linked = add(home.path(), "fiber/x", "with space");
    let listed = list(home.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed[1].path.canonicalize().unwrap(),
        linked.canonicalize().unwrap()
    );
}

#[test]
fn run_from_a_subdirectory_lists_the_same_entries() {
    let home = repo("worktree-list-subdir");
    let linked = add(home.path(), "fiber/x", "wt");
    fs::create_dir_all(home.path().join("sub")).unwrap();
    let from_top = list(home.path()).unwrap();
    let from_sub = list(&home.path().join("sub")).unwrap();
    assert_eq!(from_top, from_sub);
    assert_eq!(from_top.len(), 2);
    assert_eq!(
        from_top[1].path.canonicalize().unwrap(),
        linked.canonicalize().unwrap()
    );
}

#[test]
fn outside_a_repository_is_a_git_error() {
    let home = fakes::TempDir::new("worktree-list-outside");
    let outside = home.path().join("plain");
    fs::create_dir(&outside).unwrap();
    match list(&outside).unwrap_err() {
        Error::Git { command, .. } => assert_eq!(command, "worktree list"),
        Error::GitMissing
        | Error::Io { .. }
        | Error::NotARepository { .. }
        | Error::Exists { .. } => {
            panic!("expected a worktree list error")
        }
    }
}

#[test]
fn a_missing_git_is_git_missing() {
    let home = repo("worktree-list-no-git");
    let err = list_with("fiber-no-such-git", home.path()).unwrap_err();
    assert!(matches!(err, Error::GitMissing), "{err:?}");
}

#[test]
fn an_empty_record_set_is_an_error_naming_worktree_list() {
    match parse(b"", Path::new("/w")).unwrap_err() {
        Error::Git {
            command, stderr, ..
        } => {
            assert_eq!(command, "worktree list");
            assert_eq!(stderr, "no worktrees listed");
        }
        Error::GitMissing
        | Error::Io { .. }
        | Error::NotARepository { .. }
        | Error::Exists { .. } => {
            panic!("expected a worktree list error")
        }
    }
}

#[test]
fn a_lock_reason_with_a_newline_does_not_shift_the_next_entry() {
    // The `-z` form ends the reason's record at NUL, so the newline
    // inside it must not start a new entry.
    let stdout = b"worktree /main\0HEAD abc\0branch refs/heads/main\0\0worktree /wt\0locked a\nb\0HEAD abc\0branch refs/heads/x\0\0";
    let listed = parse(stdout, Path::new("/w")).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(!listed[0].locked);
    assert!(listed[1].locked);
    assert_eq!(listed[1].path, PathBuf::from("/wt"));
}

#[test]
#[cfg(target_os = "linux")]
fn a_non_utf8_worktree_name_round_trips_as_bytes() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    let home = repo("worktree-list-nonutf8");
    let linked = home.path().join(OsStr::from_bytes(b"w\xff"));
    let child = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["worktree", "add", "-b", "fiber/x"])
        .arg(linked.as_os_str())
        .current_dir(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let out = fakes::finished(
        "git worktree add with a non-UTF-8 name",
        child,
        &[],
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert!(out.status.success(), "{out:?}");
    let listed = list(home.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed[1].path.as_os_str().as_bytes(),
        home.path()
            .join(OsStr::from_bytes(b"w\xff"))
            .as_os_str()
            .as_bytes()
    );
}
