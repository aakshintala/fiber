//! Tests for creating a worktree and counting commits beyond a base, through
//! real `git` in temporary directories.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use contract::ErrorCode;

use super::*;
use crate::{Error, Inspected, Inspection, Removed, ignored, inspect, remove};

/// One budget for the whole wait: setup, the code under test and the
/// assertions run on one thread under one deadline (`docs/testing.md`,
/// "Waits and timeouts").
const BUDGET: Duration = Duration::from_secs(40);

/// The plain `add` runner: `Command::output`, as `doors::isolate` passes a
/// cancellable one.
fn direct(command: &mut Command) -> io::Result<Output> {
    command.output()
}

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

/// Whether `refs/heads/<branch>` exists in the repository, with the
/// variables that redirect git elsewhere removed: the hostile-environment
/// child's own checks, after the creation under test ran under them.
fn branch_exists_without_redirect(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether `path` names anything at all, without following a final link.
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[test]
fn create_makes_the_branch_at_head_and_checks_it_out() {
    fakes::within(
        "create_makes_the_branch_at_head_and_checks_it_out",
        BUDGET,
        || {
            let home = repo("worktree-create");
            let path = home.path().join("wt");
            let created = create(home.path(), &path, "fiber/x", &direct).unwrap();
            let head = git(home.path(), &["rev-parse", "HEAD"]);
            assert_eq!(created.base, head);
            assert_eq!(created.branch, "fiber/x");
            assert_eq!(
                created.path,
                fs::canonicalize(&path).unwrap(),
                "the recorded path is canonical"
            );
            assert!(path.join("file.txt").is_file());
            assert_eq!(git(home.path(), &["rev-parse", "fiber/x"]), head);
            let inspected = worktree_of(inspect(&path).unwrap());
            assert_eq!(inspected.branch, "fiber/x");
        },
    );
}

#[test]
fn the_created_path_is_canonical_through_a_symlinked_parent() {
    fakes::within(
        "the_created_path_is_canonical_through_a_symlinked_parent",
        BUDGET,
        || {
            let home = repo("worktree-create-symlink");
            let real = home.path().join("real");
            fs::create_dir(&real).unwrap();
            std::os::unix::fs::symlink(&real, home.path().join("link")).unwrap();
            let path = home.path().join("link/wt");
            let created = create(home.path(), &path, "fiber/x", &direct).unwrap();
            // On Linux the temp directory holds no symlink of its own, so
            // only this link kills a dropped `canonicalize`.
            assert_eq!(created.path, fs::canonicalize(&real).unwrap().join("wt"),);
        },
    );
}

#[test]
fn create_from_a_subdirectory_makes_a_worktree_of_the_whole_repository() {
    fakes::within(
        "create_from_a_subdirectory_makes_a_worktree_of_the_whole_repository",
        BUDGET,
        || {
            let home = repo("worktree-create-sub");
            let sub = home.path().join("sub");
            fs::create_dir(&sub).unwrap();
            let path = home.path().join("wt");
            let created = create(&sub, &path, "fiber/x", &direct).unwrap();
            assert_eq!(created.base, git(home.path(), &["rev-parse", "HEAD"]));
            assert!(path.join("file.txt").is_file());
        },
    );
}

#[test]
fn a_plain_directory_is_not_a_repository() {
    fakes::within("a_plain_directory_is_not_a_repository", BUDGET, || {
        let home = fakes::TempDir::new("worktree-create-plain");
        let launch = home.path().join("plain");
        fs::create_dir(&launch).unwrap();
        let path = launch.join("wt");
        let err = create(&launch, &path, "fiber/x", &direct).unwrap_err();
        assert!(matches!(err, Error::NotARepository { .. }), "{err:?}");
        assert_eq!(err.code(), ErrorCode::Usage);
        assert!(err.to_string().contains(launch.to_str().unwrap()), "{err}");
        assert!(!exists(&path), "nothing was created");
    });
}

#[test]
fn the_inside_of_dot_git_is_not_a_repository() {
    fakes::within("the_inside_of_dot_git_is_not_a_repository", BUDGET, || {
        let home = repo("worktree-create-gitdir");
        let launch = home.path().join(".git");
        // `rev-parse --is-inside-work-tree` succeeds here, printing
        // `false`: only the output tells it apart.
        let err = create(&launch, &launch.join("wt"), "fiber/x", &direct).unwrap_err();
        assert!(matches!(err, Error::NotARepository { .. }), "{err:?}");
    });
}

#[test]
fn an_unborn_head_is_a_git_error_and_creates_nothing() {
    fakes::within(
        "an_unborn_head_is_a_git_error_and_creates_nothing",
        BUDGET,
        || {
            let home = fakes::TempDir::new("worktree-create-unborn");
            git(home.path(), &["init", "--quiet"]);
            let path = home.path().join("wt");
            let err = create(home.path(), &path, "fiber/x", &direct).unwrap_err();
            assert!(matches!(err, Error::Git { .. }), "{err:?}");
            assert_eq!(err.code(), ErrorCode::IoFailed);
            assert!(!exists(&path), "nothing was created");
            assert!(!branch_exists(home.path(), "fiber/x"));
        },
    );
}

#[test]
fn an_existing_branch_is_refused_and_left_where_it_was() {
    fakes::within(
        "an_existing_branch_is_refused_and_left_where_it_was",
        BUDGET,
        || {
            let home = repo("worktree-create-branch");
            git(home.path(), &["branch", "fiber/x"]);
            let sha = git(home.path(), &["rev-parse", "fiber/x"]);
            let path = home.path().join("wt");
            let err = create(home.path(), &path, "fiber/x", &direct).unwrap_err();
            assert!(matches!(err, Error::Exists { .. }), "{err:?}");
            assert_eq!(err.code(), ErrorCode::IoFailed);
            assert_eq!(git(home.path(), &["rev-parse", "fiber/x"]), sha);
            assert!(!exists(&path), "the path was left alone");
        },
    );
}

#[test]
fn an_existing_empty_directory_is_refused() {
    fakes::within("an_existing_empty_directory_is_refused", BUDGET, || {
        let home = repo("worktree-create-empty");
        let path = home.path().join("wt");
        fs::create_dir(&path).unwrap();
        let err = create(home.path(), &path, "fiber/x", &direct).unwrap_err();
        // Git itself accepts an empty directory: only the check refuses it.
        assert!(matches!(err, Error::Exists { .. }), "{err:?}");
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        assert!(!branch_exists(home.path(), "fiber/x"));
    });
}

#[test]
fn an_existing_non_empty_path_is_refused_and_left_alone() {
    fakes::within(
        "an_existing_non_empty_path_is_refused_and_left_alone",
        BUDGET,
        || {
            let home = repo("worktree-create-full");
            let path = home.path().join("wt");
            fs::create_dir(&path).unwrap();
            fs::write(path.join("precious.txt"), "precious").unwrap();
            let err = create(home.path(), &path, "fiber/x", &direct).unwrap_err();
            assert!(matches!(err, Error::Exists { .. }), "{err:?}");
            assert_eq!(
                fs::read_to_string(path.join("precious.txt")).unwrap(),
                "precious"
            );
            assert!(!branch_exists(home.path(), "fiber/x"));
        },
    );
}

#[test]
fn a_dangling_symlink_at_the_path_is_refused() {
    fakes::within("a_dangling_symlink_at_the_path_is_refused", BUDGET, || {
        let home = repo("worktree-create-dangling");
        let path = home.path().join("wt");
        std::os::unix::fs::symlink(home.path().join("no-such-target"), &path).unwrap();
        let err = create(home.path(), &path, "fiber/x", &direct).unwrap_err();
        // `Path::exists` follows the link and says missing: only
        // `symlink_metadata` refuses it.
        assert!(matches!(err, Error::Exists { .. }), "{err:?}");
        assert_eq!(
            fs::read_link(&path).unwrap(),
            home.path().join("no-such-target")
        );
        assert!(!branch_exists(home.path(), "fiber/x"));
    });
}

#[test]
fn an_unreadable_parent_is_an_io_error() {
    fakes::within("an_unreadable_parent_is_an_io_error", BUDGET, || {
        let home = repo("worktree-create-noperm");
        let parent = home.path().join("dark");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
        let err = create(home.path(), &parent.join("wt"), "fiber/x", &direct).unwrap_err();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        // A lookup error other than `NotFound` is an I/O error, never
        // "absent".
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        assert!(!branch_exists(home.path(), "fiber/x"));
    });
}

#[test]
fn a_failing_post_checkout_hook_leaves_nothing_behind() {
    fakes::within(
        "a_failing_post_checkout_hook_leaves_nothing_behind",
        BUDGET,
        || {
            let home = repo("worktree-create-hook");
            git(home.path(), &["branch", "keep"]);
            let keep = git(home.path(), &["rev-parse", "keep"]);
            let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/failing-hook");
            git(
                home.path(),
                &["config", "core.hooksPath", hooks.to_str().unwrap()],
            );
            let path = home.path().join("wt");
            // A failing `post-checkout` exits non-zero after making both
            // names: the rollback removes what this call made.
            let err = create(home.path(), &path, "fiber/x", &direct).unwrap_err();
            match err {
                Error::Git { command, .. } => assert_eq!(command, "worktree add"),
                Error::GitMissing
                | Error::NotARepository { .. }
                | Error::Exists { .. }
                | Error::Io { .. } => panic!("expected a worktree add error, found {err:?}"),
            }
            assert!(!exists(&path), "the path is gone");
            assert!(!branch_exists(home.path(), "fiber/x"), "the branch is gone");
            assert_eq!(git(home.path(), &["worktree", "list"]).lines().count(), 1);
            assert_eq!(git(home.path(), &["rev-parse", "keep"]), keep);
        },
    );
}

#[test]
fn a_missing_git_is_git_missing() {
    fakes::within("a_missing_git_is_git_missing", BUDGET, || {
        let home = repo("worktree-create-nogit");
        let err = create_with(
            "fiber-no-such-git",
            home.path(),
            &home.path().join("wt"),
            "fiber/x",
            &direct,
        )
        .unwrap_err();
        assert!(matches!(err, Error::GitMissing), "{err:?}");
        assert_eq!(err.code(), ErrorCode::Usage);
    });
}

#[test]
fn a_git_that_cannot_execute_is_an_io_error() {
    fakes::within("a_git_that_cannot_execute_is_an_io_error", BUDGET, || {
        let home = repo("worktree-create-noexec");
        let err = create_with(
            home.path().to_str().unwrap(),
            home.path(),
            &home.path().join("wt"),
            "fiber/x",
            &direct,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        assert_eq!(err.code(), ErrorCode::IoFailed);
    });
}

#[test]
fn an_interrupted_add_creates_nothing() {
    fakes::within("an_interrupted_add_creates_nothing", BUDGET, || {
        let home = repo("worktree-create-interrupted");
        let path = home.path().join("wt");
        let runner =
            |_: &mut Command| -> io::Result<Output> { Err(io::ErrorKind::Interrupted.into()) };
        let err = create(home.path(), &path, "fiber/x", &runner).unwrap_err();
        match err {
            Error::Io { source, .. } => assert_eq!(source.kind(), io::ErrorKind::Interrupted),
            Error::GitMissing
            | Error::NotARepository { .. }
            | Error::Exists { .. }
            | Error::Git { .. } => panic!("expected an I/O error, found {err:?}"),
        }
        assert!(!exists(&path), "no path");
        assert!(!branch_exists(home.path(), "fiber/x"), "no branch");
    });
}

#[test]
fn an_add_cancelled_after_it_ran_is_rolled_back() {
    fakes::within(
        "an_add_cancelled_after_it_ran_is_rolled_back",
        BUDGET,
        || {
            let home = repo("worktree-create-cancelled");
            let path = home.path().join("wt");
            // A cancel that arrived after git made both names: the call
            // ran, and only the error stands in for the kill.
            let runner = |command: &mut Command| -> io::Result<Output> {
                let _ = command.output()?;
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
            };
            let err = create(home.path(), &path, "fiber/x", &runner).unwrap_err();
            assert!(matches!(err, Error::Io { .. }), "{err:?}");
            assert!(!exists(&path), "the path is gone");
            assert!(!branch_exists(home.path(), "fiber/x"), "the branch is gone");
        },
    );
}

#[test]
fn no_command_but_add_runs_a_hook() {
    if std::env::var("FIBER_WORKTREE_HOOKS_CHILD").is_ok() {
        fakes::within("no_command_but_add_runs_a_hook child", BUDGET, || {
            hooks_child();
        });
        return;
    }
    fakes::within("no_command_but_add_runs_a_hook", BUDGET, || {
        let home = fakes::TempDir::new("worktree-hooks");
        let log = home.path().join("hooks.log");
        fs::write(&log, "").unwrap();
        let name = module_path!().split_once("::").unwrap().1;
        let out = fakes::rerun(
            &format!("{name}::no_command_but_add_runs_a_hook"),
            &[
                ("FIBER_WORKTREE_HOOKS_CHILD", home.path().to_str().unwrap()),
                ("FIBER_TEST_HOOK_LOG", log.to_str().unwrap()),
            ],
        );
        assert!(
            out.status.success(),
            "the hook child failed:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    });
}

/// The hook test's body, in the re-run child with `FIBER_TEST_HOOK_LOG`
/// set: every git command the crate runs, and the rollback alone, must
/// leave the log empty, while a hook run directly writes to it.
fn hooks_child() {
    let base = PathBuf::from(std::env::var("FIBER_WORKTREE_HOOKS_CHILD").unwrap());
    let log = PathBuf::from(std::env::var("FIBER_TEST_HOOK_LOG").unwrap());
    hooks_child_in(&base, &log);
}

/// Builds the repository under `base` and runs the hook assertions there.
fn hooks_child_in(base: &Path, log: &Path) {
    let dir = base.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet"]);
    fs::write(dir.join("file.txt"), "x").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "first"]);
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/logging-hooks");
    git(&dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    let read_log = || fs::read_to_string(log).unwrap();
    // `create` runs `add` with hooks on: truncate what it legitimately
    // wrote, then nothing else may run one.
    let path = dir.join("wt");
    let created = create(&dir, &path, "fiber/x", &direct).unwrap();
    fs::write(log, "").unwrap();
    let inspected = worktree_of(inspect(&path).unwrap());
    assert_eq!(commits_beyond(&path, "fiber/x", &created.base).unwrap(), 0);
    assert!(ignored(&path).unwrap().is_empty());
    match remove(&path, &inspected, false).unwrap() {
        Removed::Whole => {}
        Removed::BranchKept(_) => panic!("the branch was kept"),
    }
    assert_eq!(read_log(), "", "only `add` runs a hook");
    // The rollback alone: the runner runs `add` (which legitimately runs
    // its hooks), truncates, and only then fails.
    let second = dir.join("wt2");
    let runner = |command: &mut Command| -> io::Result<Output> {
        command.output()?;
        fs::write(log, "").unwrap();
        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
    };
    assert!(create(&dir, &second, "fiber/y", &runner).is_err());
    assert_eq!(read_log(), "", "the rollback runs no hook");
    // The control: a hook run directly writes to the log, so the test
    // above bites.
    git(&dir, &["branch", "control"]);
    git(&dir, &["branch", "-D", "control"]);
    assert!(
        read_log().contains("reference-transaction"),
        "the control hook ran"
    );
}

#[test]
fn git_environment_cannot_redirect_creation() {
    if let Ok(launch) = std::env::var("FIBER_WORKTREE_CREATE_ENV_CHILD") {
        fakes::within(
            "git_environment_cannot_redirect_creation child",
            BUDGET,
            || {
                let launch = PathBuf::from(launch);
                let path = launch.join("wt");
                create(&launch, &path, "fiber/x", &direct).unwrap();
                // The test's own checks run with the redirect removed; the
                // creation above ran under it.
                assert!(
                    branch_exists_without_redirect(&launch, "fiber/x"),
                    "the branch is in the launch repository"
                );
                let other =
                    PathBuf::from(std::env::var("FIBER_WORKTREE_CREATE_ENV_OTHER").unwrap());
                assert!(
                    !branch_exists_without_redirect(&other, "fiber/x"),
                    "and not in the other one"
                );
            },
        );
        return;
    }
    fakes::within("git_environment_cannot_redirect_creation", BUDGET, || {
        let home = repo("worktree-create-env");
        let other = repo("worktree-create-env-other");
        let wrong_index = home.path().join("wrong-index");
        fs::write(&wrong_index, "not an index").unwrap();
        let name = module_path!().split_once("::").unwrap().1;
        let out = fakes::rerun(
            &format!("{name}::git_environment_cannot_redirect_creation"),
            &[
                ("GIT_DIR", other.path().join(".git").to_str().unwrap()),
                ("GIT_WORK_TREE", other.path().to_str().unwrap()),
                (
                    "GIT_COMMON_DIR",
                    other.path().join(".git").to_str().unwrap(),
                ),
                ("GIT_INDEX_FILE", wrong_index.to_str().unwrap()),
                (
                    "FIBER_WORKTREE_CREATE_ENV_CHILD",
                    home.path().to_str().unwrap(),
                ),
                (
                    "FIBER_WORKTREE_CREATE_ENV_OTHER",
                    other.path().to_str().unwrap(),
                ),
            ],
        );
        assert!(
            out.status.success(),
            "creation must see past the redirect:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    });
}

#[test]
fn commits_beyond_counts_zero_then_one_then_two() {
    fakes::within(
        "commits_beyond_counts_zero_then_one_then_two",
        BUDGET,
        || {
            let home = repo("worktree-create-count");
            let path = home.path().join("wt");
            let created = create(home.path(), &path, "fiber/x", &direct).unwrap();
            assert_eq!(commits_beyond(&path, "fiber/x", &created.base).unwrap(), 0);
            fs::write(path.join("a.txt"), "a").unwrap();
            git(&path, &["add", "."]);
            git(&path, &["commit", "--quiet", "-m", "one"]);
            assert_eq!(commits_beyond(&path, "fiber/x", &created.base).unwrap(), 1);
            fs::write(path.join("b.txt"), "b").unwrap();
            git(&path, &["add", "."]);
            git(&path, &["commit", "--quiet", "-m", "two"]);
            assert_eq!(commits_beyond(&path, "fiber/x", &created.base).unwrap(), 2);
        },
    );
}

#[test]
fn a_commit_merged_elsewhere_still_counts_beyond_the_base() {
    fakes::within(
        "a_commit_merged_elsewhere_still_counts_beyond_the_base",
        BUDGET,
        || {
            let home = repo("worktree-create-merged");
            let path = home.path().join("wt");
            let created = create(home.path(), &path, "fiber/x", &direct).unwrap();
            fs::write(path.join("more.txt"), "y").unwrap();
            git(&path, &["add", "."]);
            git(&path, &["commit", "--quiet", "-m", "second"]);
            git(home.path(), &["branch", "other", "fiber/x"]);
            let inspected = worktree_of(inspect(&path).unwrap());
            assert_eq!(inspected.unique_commits, 0);
            assert_eq!(commits_beyond(&path, "fiber/x", &created.base).unwrap(), 1);
        },
    );
}

#[test]
fn a_bad_base_is_a_git_error() {
    fakes::within("a_bad_base_is_a_git_error", BUDGET, || {
        let home = repo("worktree-create-badbase");
        let path = home.path().join("wt");
        create(home.path(), &path, "fiber/x", &direct).unwrap();
        let err = commits_beyond(&path, "fiber/x", "no-such-base").unwrap_err();
        match err {
            Error::Git { command, .. } => assert_eq!(command, "rev-list"),
            Error::GitMissing
            | Error::NotARepository { .. }
            | Error::Exists { .. }
            | Error::Io { .. } => panic!("expected a rev-list error, found {err:?}"),
        }
    });
}
