//! Tests for the worktree crate: inspection and removal through `git`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use contract::ErrorCode;

use super::*;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A repository with one commit on `main`, in a temporary directory.
fn repo(prefix: &str) -> fakes::TempDir {
    let home = fakes::TempDir::new(prefix);
    git(home.path(), &["init", "--quiet"]);
    fs::write(home.path().join("file.txt"), "x").unwrap();
    git(home.path(), &["add", "."]);
    git(home.path(), &["commit", "--quiet", "-m", "first"]);
    home
}

/// Adds a worktree on a new branch `branch` at `name` under the repository.
fn add(repo: &Path, branch: &str, name: &str) -> PathBuf {
    let path = repo.join(name);
    git(
        repo,
        &["worktree", "add", "-b", branch, path.to_str().unwrap()],
    );
    path
}

/// Whether `refs/heads/<branch>` still exists in the repository.
fn branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The linked worktree `inspect` found, or the test fails.
fn worktree_of(found: Inspection) -> Inspected {
    match found {
        Inspection::Worktree(inspected) => inspected,
        Inspection::NotAWorktree | Inspection::Detached => {
            panic!("expected a worktree, found {found:?}")
        }
    }
}

/// The common directory of the repository: its `.git`.
fn common_dir(repo: &Path) -> PathBuf {
    repo.canonicalize().unwrap().join(".git")
}

#[test]
fn a_clean_merged_worktree_reports_nothing_to_lose() {
    let home = repo("worktree-clean");
    let path = add(home.path(), "fiber/x", "wt");
    let inspected = worktree_of(inspect(&path).unwrap());
    assert_eq!(inspected.branch, "fiber/x");
    assert_eq!(inspected.common_dir, common_dir(home.path()));
    assert!(!inspected.uncommitted);
    assert_eq!(inspected.unique_commits, 0);
}

#[test]
fn an_untracked_file_means_uncommitted() {
    let home = repo("worktree-untracked");
    let path = add(home.path(), "fiber/x", "wt");
    fs::write(path.join("notes.txt"), "scratch").unwrap();
    let inspected = worktree_of(inspect(&path).unwrap());
    assert!(inspected.uncommitted);
    assert_eq!(inspected.unique_commits, 0);
}

#[test]
fn a_commit_only_on_the_branch_counts_as_unique() {
    let home = repo("worktree-unique");
    let path = add(home.path(), "fiber/x", "wt");
    fs::write(path.join("more.txt"), "y").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "--quiet", "-m", "second"]);
    let inspected = worktree_of(inspect(&path).unwrap());
    assert!(!inspected.uncommitted);
    assert_eq!(inspected.unique_commits, 1);
}

#[test]
fn the_same_commit_on_another_branch_counts_as_merged() {
    let home = repo("worktree-other-branch");
    let path = add(home.path(), "fiber/x", "wt");
    fs::write(path.join("more.txt"), "y").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "--quiet", "-m", "second"]);
    git(home.path(), &["branch", "other", "fiber/x"]);
    let inspected = worktree_of(inspect(&path).unwrap());
    assert_eq!(inspected.unique_commits, 0);
}

#[test]
fn the_same_commit_on_a_remote_counts_as_merged() {
    let home = repo("worktree-remote");
    let path = add(home.path(), "fiber/x", "wt");
    fs::write(path.join("more.txt"), "y").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "--quiet", "-m", "second"]);
    let sha = git(&path, &["rev-parse", "HEAD"]);
    git(home.path(), &["update-ref", "refs/remotes/o/x", &sha]);
    let inspected = worktree_of(inspect(&path).unwrap());
    assert_eq!(inspected.unique_commits, 0);
}

#[test]
fn a_detached_head_is_detached() {
    let home = repo("worktree-detached");
    let path = home.path().join("wt");
    git(
        home.path(),
        &["worktree", "add", "--detach", path.to_str().unwrap()],
    );
    match inspect(&path).unwrap() {
        Inspection::Detached => {}
        Inspection::Worktree(_) | Inspection::NotAWorktree => {
            panic!("expected detached, inspected {path:?}")
        }
    }
}

#[test]
fn a_plain_directory_is_not_a_worktree_without_running_git() {
    // `fiber-no-such-git` cannot be spawned, so reaching this result proves
    // no `git` command ran.
    let home = fakes::TempDir::new("worktree-plain");
    let outside = home.path().join("plain");
    fs::create_dir(&outside).unwrap();
    match inspect_with("fiber-no-such-git", &outside).unwrap() {
        Inspection::NotAWorktree => {}
        Inspection::Worktree(_) | Inspection::Detached => panic!("plain outside is a worktree"),
    }
    let holder = repo("worktree-plain-inside");
    let inside = holder.path().join("sub");
    fs::create_dir(&inside).unwrap();
    match inspect_with("fiber-no-such-git", &inside).unwrap() {
        Inspection::NotAWorktree => {}
        Inspection::Worktree(_) | Inspection::Detached => panic!("plain inside is a worktree"),
    }
}

#[test]
fn a_main_worktree_is_not_a_worktree() {
    let home = repo("worktree-main");
    match inspect(home.path()).unwrap() {
        Inspection::NotAWorktree => {}
        Inspection::Worktree(_) | Inspection::Detached => panic!("main is a worktree"),
    }
}

#[test]
fn a_foreign_gitdir_is_never_removed() {
    // A `.git` file pointing at another repository is not one of the
    // worktree's own, but git still reads it, so `inspect` reports a
    // worktree. Removal is still safe: git refuses a path that is not a
    // registered worktree, with `--force` too.
    let holder = repo("worktree-foreign");
    let elsewhere = fakes::TempDir::new("worktree-foreign-dir");
    let path = elsewhere.path().join("wt");
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("precious.txt"), "precious").unwrap();
    let gitdir = holder.path().join(".git");
    fs::write(path.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    let inspected = worktree_of(inspect(&path).unwrap());
    assert!(remove(&path, &inspected, false).is_err());
    assert!(remove(&path, &inspected, true).is_err());
    assert_eq!(
        fs::read_to_string(path.join("precious.txt")).unwrap(),
        "precious"
    );
}

#[test]
fn an_unreadable_worktree_is_an_io_error() {
    let home = fakes::TempDir::new("worktree-unreadable");
    let path = home.path().join("wt");
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join(".git"), "gitdir: /nonexistent-admin-dir/x\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let err = inspect(&path).unwrap_err();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::IoFailed);
}

#[test]
fn a_git_that_cannot_execute_is_an_io_error() {
    let home = repo("worktree-noexec");
    let path = add(home.path(), "fiber/x", "wt");
    let program = home.path().to_str().unwrap();
    let err = inspect_with(program, &path).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::IoFailed);
}

#[test]
fn broken_metadata_is_a_git_error() {
    let home = fakes::TempDir::new("worktree-broken");
    let path = home.path().join("wt");
    fs::create_dir(&path).unwrap();
    fs::write(path.join(".git"), "gitdir: /nonexistent-admin-dir/x\n").unwrap();
    let err = inspect(&path).unwrap_err();
    assert!(matches!(err, Error::Git { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::IoFailed);
}

#[test]
fn a_corrupt_index_is_a_git_error() {
    let home = repo("worktree-corrupt");
    let path = add(home.path(), "fiber/x", "wt");
    let admin = git(&path, &["rev-parse", "--absolute-git-dir"]);
    fs::write(Path::new(&admin).join("index"), "garbage").unwrap();
    let err = inspect(&path).unwrap_err();
    assert!(matches!(err, Error::Git { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::IoFailed);
}

#[test]
fn a_missing_git_is_git_missing() {
    let home = repo("worktree-no-git");
    let path = add(home.path(), "fiber/x", "wt");
    let err = inspect_with("fiber-no-such-git", &path).unwrap_err();
    assert!(matches!(err, Error::GitMissing), "{err}");
    assert_eq!(err.code(), ErrorCode::Usage);
    assert!(err.to_string().contains("git is not installed"), "{err}");
}

#[test]
fn a_clean_remove_takes_the_worktree_and_its_branch() {
    let home = repo("worktree-remove-clean");
    let path = add(home.path(), "fiber/x", "wt");
    let inspected = worktree_of(inspect(&path).unwrap());
    match remove(&path, &inspected, false).unwrap() {
        Removed::Whole => {}
        Removed::BranchKept(_) => panic!("branch kept"),
    }
    assert!(!path.exists());
    assert!(!branch_exists(home.path(), "fiber/x"));
}

#[test]
fn a_dirty_remove_without_force_keeps_both() {
    let home = repo("worktree-remove-dirty");
    let path = add(home.path(), "fiber/x", "wt");
    fs::write(path.join("notes.txt"), "scratch").unwrap();
    let inspected = worktree_of(inspect(&path).unwrap());
    assert!(inspected.uncommitted);
    assert!(remove(&path, &inspected, false).is_err());
    assert!(path.is_dir());
    assert!(branch_exists(home.path(), "fiber/x"));
}

#[test]
fn a_dirty_remove_with_force_takes_both() {
    let home = repo("worktree-remove-force");
    let path = add(home.path(), "fiber/x", "wt");
    fs::write(path.join("notes.txt"), "scratch").unwrap();
    let inspected = worktree_of(inspect(&path).unwrap());
    match remove(&path, &inspected, true).unwrap() {
        Removed::Whole => {}
        Removed::BranchKept(_) => panic!("branch kept"),
    }
    assert!(!path.exists());
    assert!(!branch_exists(home.path(), "fiber/x"));
}

#[test]
fn a_branch_checked_out_elsewhere_is_kept() {
    let home = repo("worktree-remove-kept");
    let path = home.path().join("wt");
    git(
        home.path(),
        &["worktree", "add", "--detach", path.to_str().unwrap()],
    );
    // `main` is checked out in the main worktree, so `branch -D` refuses
    // it after the detached worktree is gone.
    let inspected = Inspected {
        branch: "main".to_owned(),
        common_dir: common_dir(home.path()),
        uncommitted: false,
        unique_commits: 0,
    };
    let kept = match remove(&path, &inspected, false).unwrap() {
        Removed::BranchKept(err) => err,
        Removed::Whole => panic!("branch gone"),
    };
    assert_eq!(kept.code(), ErrorCode::IoFailed);
    assert!(!path.exists());
    assert!(branch_exists(home.path(), "main"));
}

#[test]
fn each_error_maps_to_its_code() {
    assert_eq!(Error::GitMissing.code(), ErrorCode::Usage);
    assert_eq!(
        Error::Git {
            path: PathBuf::from("/w"),
            command: "status".to_owned(),
            stderr: "fatal: x".to_owned(),
        }
        .code(),
        ErrorCode::IoFailed
    );
    assert_eq!(
        Error::Io {
            path: PathBuf::from("/w"),
            source: std::io::Error::other("boom"),
        }
        .code(),
        ErrorCode::IoFailed
    );
}
