//! Tests for the session's worktree lifecycle: creation under the
//! project's worktrees, and removal at the end only when nothing is
//! uncommitted, nothing is beyond the base, and no session works in it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::events::{Event, SessionStarted, Variables, VariablesSource};
use contract::shapes::Worktree;
use contract::{ErrorCode, SessionId};
use log::Log;

use super::isolate;
use crate::project;

/// One budget for the whole wait: setup, the code under test and the
/// assertions run on one thread under one deadline (`docs/testing.md`,
/// "Waits and timeouts").
const BUDGET: Duration = Duration::from_secs(40);

/// The plain `add` runner: `Command::output`, as the session passes a
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

/// `git` with the variables that redirect it elsewhere removed, for the
/// hostile-environment child's own checks.
fn git_clean(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE");
    command
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

/// Whether `path` names anything at all, without following a final link.
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// The failure an `isolate` that was expected to fail returned.
fn expect_failed(
    result: Result<super::Isolation, contract::shapes::Failure>,
) -> contract::shapes::Failure {
    match result {
        Err(failure) => failure,
        Ok(_) => panic!("expected isolation to fail"),
    }
}

/// The project's key for `repo`, naming its sessions and worktrees.
fn key_of(repo: &Path) -> String {
    log::project_key(&project(repo))
}

/// The worktree path `isolate` computes for `id` under `home`.
fn worktree_path(home: &Path, key: &str, id: &str) -> PathBuf {
    home.join("projects").join(key).join("worktrees").join(id)
}

/// An isolation for `id`, in `repo` under `home`.
fn isolate_now(repo: &Path, home: &Path, id: &str) -> super::Isolation {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    isolate(repo, home, &SessionId(id.into()), clock, &direct).unwrap()
}

/// An open session in `sessions` whose first line records `workspace`,
/// holding its lock as a running session would.
fn held(sessions: &Path, id: &str, workspace: &str) -> Log {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log = Log::create(sessions, SessionId(id.into()), Arc::clone(&clock)).unwrap();
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: workspace.to_owned(),
            variables: Variables {
                path: "/usr/bin:/bin".to_owned(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
        }),
        None,
        None,
    )
    .unwrap();
    log
}

/// The one diagnostic line for `id`, or the test fails.
fn warn_line(home: &Path, id: &str) -> serde_json::Value {
    let text = fs::read_to_string(home.join("logs").join(format!("session-{id}.log"))).unwrap();
    assert_eq!(text.lines().count(), 1, "one diagnostic line");
    serde_json::from_str(text.lines().next().unwrap()).unwrap()
}

#[test]
fn isolate_makes_the_worktree_under_the_projects_worktrees() {
    fakes::within(
        "isolate_makes_the_worktree_under_the_projects_worktrees",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-isolate-home");
            let repo = repo("doors-isolate-repo");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let expected = fs::canonicalize(home.path())
                .unwrap()
                .join("projects")
                .join(key_of(repo.path()))
                .join("worktrees")
                .join(id);
            assert_eq!(isolation.path(), expected);
            assert_eq!(
                isolation.worktree(),
                Worktree {
                    path: expected.to_string_lossy().into_owned(),
                    branch: format!("fiber/{id}"),
                }
            );
        },
    );
}

#[test]
fn isolate_from_a_subdirectory_uses_the_repository_s_project_and_root() {
    fakes::within(
        "isolate_from_a_subdirectory_uses_the_repository_s_project_and_root",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-isolate-sub-home");
            let repo = repo("doors-isolate-sub");
            let sub = repo.path().join("sub");
            fs::create_dir(&sub).unwrap();
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(&sub, home.path(), id);
            let expected = fs::canonicalize(home.path())
                .unwrap()
                .join("projects")
                .join(key_of(repo.path()))
                .join("worktrees")
                .join(id);
            assert_eq!(isolation.path(), expected);
            assert!(isolation.path().join("file.txt").is_file());
        },
    );
}

#[test]
fn isolate_outside_a_repository_is_usage_and_creates_nothing() {
    fakes::within(
        "isolate_outside_a_repository_is_usage_and_creates_nothing",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-isolate-plain-home");
            let plain = fakes::TempDir::new("doors-isolate-plain");
            let launch = plain.path().join("plain");
            fs::create_dir(&launch).unwrap();
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let failure = expect_failed(isolate(
                &launch,
                home.path(),
                &SessionId("s_0123456789abcdef".into()),
                clock,
                &direct,
            ));
            assert_eq!(failure.code, ErrorCode::Usage);
            assert_eq!(
                failure.message,
                format!(
                    "{} is not in a git repository, so it cannot have a worktree.",
                    launch.display()
                )
            );
            assert!(!home.path().join("projects").exists());
        },
    );
}

#[test]
fn isolate_onto_an_existing_path_is_io_failed_and_leaves_it() {
    fakes::within(
        "isolate_onto_an_existing_path_is_io_failed_and_leaves_it",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-isolate-taken-home");
            let repo = repo("doors-isolate-taken");
            let id = "s_0123456789abcdef";
            let path = worktree_path(home.path(), &key_of(repo.path()), id);
            fs::create_dir_all(&path).unwrap();
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let failure = expect_failed(isolate(
                repo.path(),
                home.path(),
                &SessionId(id.into()),
                clock,
                &direct,
            ));
            assert_eq!(failure.code, ErrorCode::IoFailed);
            assert_eq!(
                failure.message,
                format!(
                    "{} already exists, so Fiber did not make a worktree there.",
                    path.display()
                )
            );
            assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
            assert!(!branch_exists(repo.path(), &format!("fiber/{id}")));
        },
    );
}

#[test]
fn isolate_with_an_interrupted_add_is_io_failed_and_creates_nothing() {
    fakes::within(
        "isolate_with_an_interrupted_add_is_io_failed_and_creates_nothing",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-isolate-interrupted-home");
            let repo = repo("doors-isolate-interrupted");
            let id = "s_0123456789abcdef";
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let runner =
                |_: &mut Command| -> io::Result<Output> { Err(io::ErrorKind::Interrupted.into()) };
            let failure = expect_failed(isolate(
                repo.path(),
                home.path(),
                &SessionId(id.into()),
                clock,
                &runner,
            ));
            assert_eq!(failure.code, ErrorCode::IoFailed);
            assert_eq!(
                failure.message,
                format!(
                    "{}: {}.",
                    worktree_path(home.path(), &key_of(repo.path()), id).display(),
                    io::ErrorKind::Interrupted
                )
            );
            assert!(!home.path().join("projects").exists());
            assert!(!branch_exists(repo.path(), &format!("fiber/{id}")));
        },
    );
}

#[test]
fn isolate_without_git_on_path_is_usage() {
    if std::env::var("FIBER_DOORS_NO_GIT_CHILD").is_ok() {
        fakes::within("isolate_without_git_on_path_is_usage child", BUDGET, || {
            let repo = PathBuf::from(std::env::var("FIBER_DOORS_NO_GIT_REPO").unwrap());
            let home = PathBuf::from(std::env::var("FIBER_DOORS_NO_GIT_HOME").unwrap());
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let failure = expect_failed(isolate(
                &repo,
                &home,
                &SessionId("s_0123456789abcdef".into()),
                clock,
                &direct,
            ));
            // `project` falls back to the launch path with no git, so the
            // failure comes from `create`'s missing git.
            assert_eq!(failure.code, ErrorCode::Usage);
            assert_eq!(failure.message, "git is not installed.");
            assert!(!home.join("projects").exists());
        });
        return;
    }
    fakes::within("isolate_without_git_on_path_is_usage", BUDGET, || {
        let home = fakes::TempDir::new("doors-isolate-nogit-home");
        let repo = repo("doors-isolate-nogit");
        let empty = home.path().join("empty-path");
        fs::create_dir(&empty).unwrap();
        let name = module_path!().split_once("::").unwrap().1;
        let out = fakes::rerun(
            &format!("{name}::isolate_without_git_on_path_is_usage"),
            &[
                ("PATH", empty.to_str().unwrap()),
                ("FIBER_DOORS_NO_GIT_CHILD", "1"),
                ("FIBER_DOORS_NO_GIT_REPO", repo.path().to_str().unwrap()),
                ("FIBER_DOORS_NO_GIT_HOME", home.path().to_str().unwrap()),
            ],
        );
        assert!(
            out.status.success(),
            "the child without git on PATH failed:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    });
}

#[test]
fn a_fresh_worktree_is_removed_at_the_end() {
    fakes::within("a_fresh_worktree_is_removed_at_the_end", BUDGET, || {
        let home = fakes::TempDir::new("doors-end-fresh-home");
        let repo = repo("doors-end-fresh");
        let id = "s_0123456789abcdef";
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        isolation.end();
        assert!(!exists(&path), "the path is gone");
        assert!(!branch_exists(repo.path(), &format!("fiber/{id}")));
        assert!(!home.path().join("logs").exists(), "no diagnostic line");
    });
}

#[test]
fn an_untracked_file_keeps_it() {
    fakes::within("an_untracked_file_keeps_it", BUDGET, || {
        let home = fakes::TempDir::new("doors-end-untracked-home");
        let repo = repo("doors-end-untracked");
        let id = "s_0123456789abcdef";
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        fs::write(path.join("notes.txt"), "scratch").unwrap();
        isolation.end();
        assert!(exists(&path));
        assert!(path.join("notes.txt").is_file());
        assert!(branch_exists(repo.path(), &format!("fiber/{id}")));
        assert!(!home.path().join("logs").exists(), "no diagnostic line");
    });
}

#[test]
fn a_modified_tracked_file_keeps_it() {
    fakes::within("a_modified_tracked_file_keeps_it", BUDGET, || {
        let home = fakes::TempDir::new("doors-end-modified-home");
        let repo = repo("doors-end-modified");
        let id = "s_0123456789abcdef";
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        fs::write(path.join("file.txt"), "precious change").unwrap();
        isolation.end();
        assert!(exists(&path));
        assert!(!home.path().join("logs").exists(), "no diagnostic line");
    });
}

#[test]
fn a_commit_beyond_the_base_keeps_it() {
    fakes::within("a_commit_beyond_the_base_keeps_it", BUDGET, || {
        let home = fakes::TempDir::new("doors-end-commit-home");
        let repo = repo("doors-end-commit");
        let id = "s_0123456789abcdef";
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        fs::write(path.join("more.txt"), "y").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "--quiet", "-m", "second"]);
        isolation.end();
        assert!(exists(&path));
        assert!(!home.path().join("logs").exists(), "no diagnostic line");
    });
}

#[test]
fn a_commit_also_on_another_branch_still_keeps_it() {
    fakes::within(
        "a_commit_also_on_another_branch_still_keeps_it",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-merged-home");
            let repo = repo("doors-end-merged");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            fs::write(path.join("more.txt"), "y").unwrap();
            git(&path, &["add", "."]);
            git(&path, &["commit", "--quiet", "-m", "second"]);
            // Elsewhere too, but still beyond the base.
            git(repo.path(), &["branch", "other", &format!("fiber/{id}")]);
            isolation.end();
            assert!(exists(&path));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn an_ignored_only_worktree_is_removed() {
    fakes::within("an_ignored_only_worktree_is_removed", BUDGET, || {
        let home = fakes::TempDir::new("doors-end-ignored-home");
        let repo = repo("doors-end-ignored");
        fs::write(repo.path().join(".gitignore"), "build/\n").unwrap();
        git(repo.path(), &["add", ".gitignore"]);
        git(repo.path(), &["commit", "--quiet", "-m", "ignore"]);
        let id = "s_0123456789abcdef";
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        fs::create_dir_all(path.join("build")).unwrap();
        fs::write(path.join("build/out"), "build").unwrap();
        isolation.end();
        assert!(!exists(&path), "ignored files are not changes");
    });
}

#[test]
fn a_switched_branch_keeps_it() {
    fakes::within("a_switched_branch_keeps_it", BUDGET, || {
        let home = fakes::TempDir::new("doors-end-switched-home");
        let repo = repo("doors-end-switched");
        let id = "s_0123456789abcdef";
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        git(&path, &["switch", "--quiet", "-c", "other"]);
        isolation.end();
        assert!(exists(&path));
        assert!(branch_exists(repo.path(), "other"));
        assert!(branch_exists(repo.path(), &format!("fiber/{id}")));
        assert!(!home.path().join("logs").exists(), "no diagnostic line");
    });
}

#[test]
fn a_detached_head_keeps_it() {
    fakes::within("a_detached_head_keeps_it", BUDGET, || {
        let home = fakes::TempDir::new("doors-end-detached-home");
        let repo = repo("doors-end-detached");
        let id = "s_0123456789abcdef";
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        git(&path, &["switch", "--quiet", "--detach", "HEAD"]);
        isolation.end();
        assert!(exists(&path));
        assert!(!home.path().join("logs").exists(), "no diagnostic line");
    });
}

#[test]
fn a_deleted_worktree_directory_is_failed_and_logged() {
    fakes::within(
        "a_deleted_worktree_directory_is_failed_and_logged",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-deleted-home");
            let repo = repo("doors-end-deleted");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            fs::remove_dir_all(&path).unwrap();
            isolation.end();
            let line = warn_line(home.path(), id);
            assert_eq!(line["level"], "warn");
            assert_eq!(line["process"], "session");
            assert_eq!(line["session_id"], id);
            assert_eq!(line["code"], "io_failed");
            assert_eq!(
                line["message"],
                format!("{}: {}.", path.display(), io::ErrorKind::NotFound)
            );
        },
    );
}

#[test]
fn a_locked_worktree_is_failed_logged_and_kept() {
    fakes::within(
        "a_locked_worktree_is_failed_logged_and_kept",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-locked-home");
            let repo = repo("doors-end-locked");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            git(repo.path(), &["worktree", "lock", path.to_str().unwrap()]);
            isolation.end();
            assert!(exists(&path), "the worktree stays");
            let line = warn_line(home.path(), id);
            assert_eq!(line["level"], "warn");
            assert_eq!(line["code"], "io_failed");
            assert_eq!(
                line["message"],
                format!(
                    "git worktree remove failed for the worktree {}.",
                    path.display()
                )
            );
        },
    );
}

#[test]
fn a_branch_that_cannot_be_deleted_is_failed_and_logged() {
    fakes::within(
        "a_branch_that_cannot_be_deleted_is_failed_and_logged",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-branchlock-home");
            let repo = repo("doors-end-branchlock");
            let id = "s_0123456789abcdef";
            let branch = format!("fiber/{id}");
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let common = git(repo.path(), &["rev-parse", "--absolute-git-dir"]);
            fs::write(
                Path::new(&common).join(format!("refs/heads/{branch}.lock")),
                "",
            )
            .unwrap();
            isolation.end();
            assert!(!exists(&path), "the worktree goes");
            assert!(branch_exists(repo.path(), &branch), "but the branch stays");
            assert_eq!(warn_line(home.path(), id)["level"], "warn");
            assert_eq!(warn_line(home.path(), id)["code"], "io_failed");
        },
    );
}

#[test]
fn a_session_a_resume_holds_keeps_its_worktree() {
    fakes::within(
        "a_session_a_resume_holds_keeps_its_worktree",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-held-home");
            let repo = repo("doors-end-held");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let holder = held(&sessions, id, path.to_str().unwrap());
            isolation.end();
            assert!(exists(&path));
            assert!(branch_exists(repo.path(), &format!("fiber/{id}")));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
            drop(holder);
            // The lock was the only reason: another fresh isolation in the
            // same layout is removed.
            let other = isolate_now(repo.path(), home.path(), "s_0123456789abcde0");
            let other_path = other.path().to_path_buf();
            other.end();
            assert!(!exists(&other_path), "the path is gone");
        },
    );
}

#[test]
fn another_running_session_in_the_worktree_keeps_it() {
    fakes::within(
        "another_running_session_in_the_worktree_keeps_it",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-other-home");
            let repo = repo("doors-end-other");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            // Another project entirely: the scan crosses projects.
            let sessions = home.path().join("projects").join("other").join("sessions");
            let _holder = held(
                &sessions,
                "s_0123456789abcde0",
                &format!("{}/sub", path.display()),
            );
            isolation.end();
            assert!(exists(&path));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn a_session_elsewhere_does_not_block() {
    fakes::within("a_session_elsewhere_does_not_block", BUDGET, || {
        let home = fakes::TempDir::new("doors-end-elsewhere-home");
        let repo = repo("doors-end-elsewhere");
        let id = "s_0123456789abcdef";
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        // Shares the worktree's prefix, but is a sibling, not under it.
        let sessions = home
            .path()
            .join("projects")
            .join(key_of(repo.path()))
            .join("sessions");
        let _holder = held(
            &sessions,
            "s_0123456789abcde0",
            &format!("{}x", path.display()),
        );
        isolation.end();
        assert!(!exists(&path));
    });
}

#[test]
fn a_session_started_after_the_first_scan_keeps_it() {
    fakes::within(
        "a_session_started_after_the_first_scan_keeps_it",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-late-home");
            let repo = repo("doors-end-late");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let (parked_tx, parked_rx) = mpsc::channel::<()>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let release_rx = Mutex::new(release_rx);
            let isolation = isolation.with_pause(Arc::new(move || {
                parked_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }));
            let ending = std::thread::spawn(move || isolation.end());
            parked_rx.recv().unwrap();
            // While `end` is parked, a new session starts in the worktree.
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let _late = held(&sessions, "s_0123456789abcde0", path.to_str().unwrap());
            release_tx.send(()).unwrap();
            ending.join().unwrap();
            assert!(exists(&path));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn end_holds_every_users_lock_while_it_removes() {
    fakes::within(
        "end_holds_every_users_lock_while_it_removes",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-locks-home");
            let repo = repo("doors-end-locks");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let (parked_tx, parked_rx) = mpsc::channel::<()>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let release_rx = Mutex::new(release_rx);
            let isolation = isolation.with_pause(Arc::new(move || {
                parked_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }));
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let dir = sessions.join(id);
            // The creator's session is recorded but its lock is free: `end`
            // takes it, and one process holds a lock only once, so no open
            // log may stay while `end` parks.
            drop(held(&sessions, id, isolation.path().to_str().unwrap()));
            let ending = std::thread::spawn(move || isolation.end());
            parked_rx.recv().unwrap();
            assert!(
                matches!(log::try_hold(&dir).unwrap(), log::Hold::Busy),
                "the creator's lock is held while parked"
            );
            release_tx.send(()).unwrap();
            ending.join().unwrap();
            assert!(!exists(&path), "the path is gone");
            assert!(matches!(log::try_hold(&dir).unwrap(), log::Hold::Held(_)));
        },
    );
}

#[test]
fn a_never_prompted_creator_without_a_directory_still_removes_the_worktree() {
    fakes::within(
        "a_never_prompted_creator_without_a_directory_still_removes_the_worktree",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-end-nodir-home");
            let repo = repo("doors-end-nodir");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            isolation.end();
            assert!(!exists(&path));
        },
    );
}

#[test]
fn isolate_ignores_a_hostile_git_environment_end_to_end() {
    if std::env::var("FIBER_DOORS_HOSTILE_CHILD").is_ok() {
        fakes::within(
            "isolate_ignores_a_hostile_git_environment_end_to_end child",
            BUDGET,
            || {
                let repo = PathBuf::from(std::env::var("FIBER_DOORS_HOSTILE_REPO").unwrap());
                let home = PathBuf::from(std::env::var("FIBER_DOORS_HOSTILE_HOME").unwrap());
                let isolation = isolate_now(&repo, &home, "s_0123456789abcdef");
                let key = key_of(&repo);
                // One scrubbed discovery: the worktree and the session share
                // the repository's project.
                assert_eq!(project(isolation.path()), project(&repo));
                let canonical_home = fs::canonicalize(&home).unwrap();
                assert!(
                    isolation
                        .path()
                        .starts_with(canonical_home.join("projects").join(&key))
                );
                let sessions = home.join("projects").join(&key).join("sessions");
                let holder = held(
                    &sessions,
                    "s_0123456789abcdef",
                    isolation.path().to_str().unwrap(),
                );
                let held_path = isolation.path().to_path_buf();
                isolation.end();
                assert!(exists(&held_path));
                assert!(!home.join("logs").exists(), "no diagnostic line");
                drop(holder);
                let other = isolate_now(&repo, &home, "s_0123456789abcde0");
                let path = other.path().to_path_buf();
                other.end();
                let listed = git_clean(&repo, &["worktree", "list"])
                    .output()
                    .unwrap()
                    .stdout;
                assert!(
                    !String::from_utf8_lossy(&listed).contains(path.to_str().unwrap()),
                    "the removed worktree is gone, keeping the held one"
                );
                assert!(!exists(&path));
            },
        );
        return;
    }
    fakes::within(
        "isolate_ignores_a_hostile_git_environment_end_to_end",
        BUDGET,
        || {
            let home = fakes::TempDir::new("doors-isolate-hostile-home");
            let repo_dir = repo("doors-isolate-hostile");
            let other = repo("doors-isolate-hostile-other");
            let junk = home.path().join("junk-index");
            fs::write(&junk, "not an index").unwrap();
            let name = module_path!().split_once("::").unwrap().1;
            let out = fakes::rerun(
                &format!("{name}::isolate_ignores_a_hostile_git_environment_end_to_end"),
                &[
                    ("GIT_COMMON_DIR", "/nonexistent"),
                    ("GIT_DIR", other.path().join(".git").to_str().unwrap()),
                    ("GIT_WORK_TREE", other.path().to_str().unwrap()),
                    ("GIT_INDEX_FILE", junk.to_str().unwrap()),
                    ("FIBER_DOORS_HOSTILE_CHILD", "1"),
                    (
                        "FIBER_DOORS_HOSTILE_REPO",
                        repo_dir.path().to_str().unwrap(),
                    ),
                    ("FIBER_DOORS_HOSTILE_HOME", home.path().to_str().unwrap()),
                ],
            );
            assert!(
                out.status.success(),
                "the hostile-environment child failed:\nstdout:{}\nstderr:{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        },
    );
}

#[test]
fn no_message_or_log_line_carries_git_stderr() {
    fakes::within("no_message_or_log_line_carries_git_stderr", BUDGET, || {
        let home = fakes::TempDir::new("doors-stderr-home");
        let failing = repo("doors-stderr-failing");
        let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../worktree/fixtures/failing-hook");
        git(
            failing.path(),
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        let id = "s_0123456789abcdef";
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let failure = expect_failed(isolate(
            failing.path(),
            home.path(),
            &SessionId(id.into()),
            clock,
            &direct,
        ));
        let expected = format!(
            "git worktree add failed for the worktree {}.",
            worktree_path(home.path(), &key_of(failing.path()), id).display()
        );
        assert_eq!(failure.message, expected);
        for marker in ["fatal:", "hook-marker-out", "hook-marker-err"] {
            assert!(!failure.message.contains(marker), "{marker} leaked");
        }
        let repo = repo("doors-stderr-locked");
        let isolation = isolate_now(repo.path(), home.path(), id);
        let path = isolation.path().to_path_buf();
        git(repo.path(), &["worktree", "lock", path.to_str().unwrap()]);
        isolation.end();
        let line = warn_line(home.path(), id);
        assert_eq!(line["level"], "warn");
        assert_eq!(line["code"], "io_failed");
        assert_eq!(
            line["message"],
            format!(
                "git worktree remove failed for the worktree {}.",
                path.display()
            )
        );
        for marker in ["fatal:", "hook-marker-out", "hook-marker-err"] {
            assert!(
                !line["message"].as_str().unwrap().contains(marker),
                "{marker} leaked"
            );
        }
    });
}
