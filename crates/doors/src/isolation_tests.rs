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
use contract::shapes::{Failure, Worktree};
use contract::{ErrorCode, SessionId};
use log::Log;

use super::{RecreateError, isolate, same_worktree};
use crate::project;
use fakes::Deadline;
use fakes::git::git;

/// One budget for the whole wait: setup, the code under test and the
/// assertions run on one thread under one deadline (`docs/testing.md`,
/// "Waits and timeouts").
const BUDGET: Duration = Duration::from_secs(40);

/// A re-run child's own bound: it fires before the parent's wait, so the
/// two no longer contradict.
const CHILD_BUDGET: Duration = Duration::from_secs(20);

/// A parent's wait for a re-run child: inside half of nextest's per-test
/// timeout (`.config/nextest.toml`, `docs/testing.md`, "Waits and timeouts").
const RERUN_WITHIN: Duration = Duration::from_secs(30);

/// The outer wait around a test that re-runs a child: at least twice the
/// parent's wait (`docs/testing.md`, "Waits and timeouts").
const RERUN_OUTER: Duration = Duration::from_secs(60);

/// Re-run bounds keep their margins: the child's budget is below the
/// parent's wait, the wait stays inside half of nextest's per-test timeout,
/// and the outer wrapper is at least twice the wait.
const _: () = assert!(
    CHILD_BUDGET.as_secs() < RERUN_WITHIN.as_secs()
        && RERUN_WITHIN.as_secs() <= 30
        && RERUN_OUTER.as_secs() >= 2 * RERUN_WITHIN.as_secs()
);

/// The plain `add` runner: `Command::output`, as the session passes a
/// cancellable one.
fn direct(command: &mut Command) -> io::Result<Output> {
    command.output()
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
    held_with_worktree(sessions, id, workspace, None)
}

/// An open session in `sessions` whose first line records `workspace`
/// and `worktree`, holding its lock as a running session would.
fn held_with_worktree(
    sessions: &Path,
    id: &str,
    workspace: &str,
    worktree: Option<Worktree>,
) -> Log {
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
            worktree,
            supervised: false,
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
fn delegate_creation_holds_before_add_and_child_adoption_overlaps() {
    fakes::within("delegate lock handoff", Deadline::after(BUDGET), || {
        let home = fakes::TempDir::new("delegate-handoff-home");
        let repo = repo("delegate-handoff-repo");
        let id = SessionId("s_0123456789abcdef".into());
        let path = worktree_path(home.path(), &key_of(repo.path()), &id.0);
        let (created, parent) =
            super::create_delegate_worktree(repo.path(), home.path(), &id, &|command| {
                assert!(!path.exists(), "share precedes git worktree add");
                assert!(path.with_extension("lock").exists());
                assert!(super::try_hold_removal(&path).unwrap().is_none());
                command.output()
            })
            .unwrap();
        let (record, child) = super::adopt_delegate_worktree(&created.path, &id)
            .unwrap()
            .unwrap();
        assert_eq!(record.branch, created.branch);
        assert!(record.repository.is_some());
        drop(parent);
        assert!(
            super::try_hold_removal(&created.path).unwrap().is_none(),
            "child holds independently"
        );
        drop(child);
        assert!(super::try_hold_removal(&created.path).unwrap().is_some());
        assert!(
            super::adopt_delegate_worktree(&created.path, &SessionId("s_other".into()))
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn delegate_end_removes_only_clean_unadvanced_unshared_worktrees() {
    fakes::within("delegate end matrix", Deadline::after(BUDGET), || {
        for mode in ["clean", "dirty", "advanced", "shared", "report"] {
            let home = fakes::TempDir::new("delegate-end-home");
            let repo = repo("delegate-end-repo");
            let id = SessionId("s_0123456789abcdef".into());
            let (created, parent) =
                super::create_delegate_worktree(repo.path(), home.path(), &id, &direct).unwrap();
            match mode {
                "dirty" => fs::write(created.path.join("new.txt"), "keep").unwrap(),
                "advanced" => {
                    git(&created.path, &["commit", "--allow-empty", "-m", "child"]);
                }
                _ => {}
            }
            let child = (mode == "shared").then(|| super::share(&created.path).unwrap());
            drop(parent);
            let finished = super::end_delegate_worktree(
                home.path(),
                &id,
                fakes::clock::FakeClock::new(),
                &created.path,
                &created.branch,
                Some(&created.base),
                mode != "report",
            );
            assert_eq!(finished.worktree.path, created.path.to_string_lossy());
            assert_eq!(finished.worktree.branch, created.branch);
            assert_eq!(finished.worktree.repository, None);
            assert_eq!(finished.dirty, mode == "dirty");
            assert_eq!(created.path.exists(), mode != "clean");
            drop(child);
        }
    });
}

#[test]
fn delegate_creation_preserves_repository_and_git_failure_kinds() {
    fakes::within("delegate creation errors", Deadline::after(BUDGET), || {
        let home = fakes::TempDir::new("delegate-create-home");
        let plain = fakes::TempDir::new("delegate-nonrepo");
        let id = SessionId("s_0123456789abcdef".into());
        assert!(matches!(
            super::create_delegate_worktree(plain.path(), home.path(), &id, &direct),
            Err(super::DelegateCreateError::NotARepository { .. })
        ));
        let repo = repo("delegate-create-repo");
        let result = super::create_delegate_worktree(repo.path(), home.path(), &id, &|_| {
            Err(io::Error::other("private output"))
        });
        let Err(super::DelegateCreateError::Failed { message }) = result else {
            panic!("typed git failure");
        };
        assert!(!message.contains("private output"));
        let path = worktree_path(home.path(), &key_of(repo.path()), &id.0);
        assert!(!path.exists());
        assert!(
            super::try_hold_removal(&path).unwrap().is_some(),
            "failed creation releases its share"
        );
    });
}

#[test]
fn isolate_makes_the_worktree_under_the_projects_worktrees() {
    fakes::within(
        "isolate_makes_the_worktree_under_the_projects_worktrees",
        Deadline::after(BUDGET),
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
                    repository: Some(
                        fs::canonicalize(repo.path().join(".git"))
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    ),
                }
            );
        },
    );
}

#[test]
fn isolate_from_a_subdirectory_uses_the_repository_s_project_and_root() {
    fakes::within(
        "isolate_from_a_subdirectory_uses_the_repository_s_project_and_root",
        Deadline::after(BUDGET),
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
        Deadline::after(BUDGET),
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
        Deadline::after(BUDGET),
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
        Deadline::after(BUDGET),
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
        fakes::within(
            "isolate_without_git_on_path_is_usage child",
            Deadline::after(CHILD_BUDGET),
            || {
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
            },
        );
        return;
    }
    fakes::within(
        "isolate_without_git_on_path_is_usage",
        Deadline::after(RERUN_OUTER),
        || {
            let home = fakes::TempDir::new("doors-isolate-nogit-home");
            let repo = repo("doors-isolate-nogit");
            let empty = home.path().join("empty-path");
            fs::create_dir(&empty).unwrap();
            let name = module_path!().split_once("::").unwrap().1;
            let out = fakes::rerun_within(
                &format!("{name}::isolate_without_git_on_path_is_usage"),
                &[
                    ("PATH", empty.to_str().unwrap()),
                    ("FIBER_DOORS_NO_GIT_CHILD", "1"),
                    ("FIBER_DOORS_NO_GIT_REPO", repo.path().to_str().unwrap()),
                    ("FIBER_DOORS_NO_GIT_HOME", home.path().to_str().unwrap()),
                ],
                fakes::Deadline::after(RERUN_WITHIN),
            );
            assert!(
                out.status.success(),
                "the child without git on PATH failed:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        },
    );
}

#[test]
fn a_fresh_worktree_is_removed_at_the_end() {
    fakes::within(
        "a_fresh_worktree_is_removed_at_the_end",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-fresh-home");
            let repo = repo("doors-end-fresh");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            isolation.end();
            assert!(!exists(&path), "the path is gone");
            assert!(!branch_exists(repo.path(), &format!("fiber/{id}")));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn an_untracked_file_keeps_it() {
    fakes::within(
        "an_untracked_file_keeps_it",
        Deadline::after(BUDGET),
        || {
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
        },
    );
}

#[test]
fn a_modified_tracked_file_keeps_it() {
    fakes::within(
        "a_modified_tracked_file_keeps_it",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-modified-home");
            let repo = repo("doors-end-modified");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            fs::write(path.join("file.txt"), "precious change").unwrap();
            isolation.end();
            assert!(exists(&path));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn a_commit_beyond_the_base_keeps_it() {
    fakes::within(
        "a_commit_beyond_the_base_keeps_it",
        Deadline::after(BUDGET),
        || {
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
        },
    );
}

#[test]
fn a_commit_also_on_another_branch_still_keeps_it() {
    fakes::within(
        "a_commit_also_on_another_branch_still_keeps_it",
        Deadline::after(BUDGET),
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
    fakes::within(
        "an_ignored_only_worktree_is_removed",
        Deadline::after(BUDGET),
        || {
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
        },
    );
}

#[test]
fn a_switched_branch_keeps_it() {
    fakes::within(
        "a_switched_branch_keeps_it",
        Deadline::after(BUDGET),
        || {
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
        },
    );
}

#[test]
fn a_detached_head_keeps_it() {
    fakes::within("a_detached_head_keeps_it", Deadline::after(BUDGET), || {
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
        Deadline::after(BUDGET),
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
        Deadline::after(BUDGET),
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
        Deadline::after(BUDGET),
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
        Deadline::after(BUDGET),
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
        Deadline::after(BUDGET),
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
    fakes::within(
        "a_session_elsewhere_does_not_block",
        Deadline::after(BUDGET),
        || {
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
        },
    );
}

#[test]
fn a_session_started_after_the_first_scan_keeps_it() {
    fakes::within(
        "a_session_started_after_the_first_scan_keeps_it",
        Deadline::after(BUDGET),
        || {
            // Starts before setup and expires before `within`'s outer guard, so a hang names this wait.
            let wait = fakes::Deadline::after(BUDGET / 2);
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
                wait.recv_or_fail(&release_rx.lock().unwrap(), "the test releases the end");
            }));
            let ending = std::thread::spawn(move || isolation.end());
            wait.recv_or_fail(&parked_rx, "the end parks");
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
        Deadline::after(BUDGET),
        || {
            // Starts before setup and expires before `within`'s outer guard, so a hang names this wait.
            let wait = fakes::Deadline::after(BUDGET / 2);
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
                wait.recv_or_fail(&release_rx.lock().unwrap(), "the test releases the end");
            }));
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let dir = sessions.join(id);
            // The creator's session is recorded but its lock is free: `end`
            // takes it, and one process holds a lock only once, so no open
            // log may stay while `end` parks.
            drop(held(&sessions, id, isolation.path().to_str().unwrap()));
            let ending = std::thread::spawn(move || isolation.end());
            wait.recv_or_fail(&parked_rx, "the end parks");
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
        Deadline::after(BUDGET),
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
            Deadline::after(CHILD_BUDGET),
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
        Deadline::after(RERUN_OUTER),
        || {
            let home = fakes::TempDir::new("doors-isolate-hostile-home");
            let repo_dir = repo("doors-isolate-hostile");
            let other = repo("doors-isolate-hostile-other");
            let junk = home.path().join("junk-index");
            fs::write(&junk, "not an index").unwrap();
            let name = module_path!().split_once("::").unwrap().1;
            let out = fakes::rerun_within(
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
                fakes::Deadline::after(RERUN_WITHIN),
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
    fakes::within(
        "no_message_or_log_line_carries_git_stderr",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-stderr-home");
            let failing = repo("doors-stderr-failing");
            let hooks =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../worktree/fixtures/failing-hook");
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
        },
    );
}

fn rewound(next: &str) -> Event {
    Event::Rewound(contract::events::Rewound {
        new_session_id: SessionId(next.into()),
        seq: contract::Seq(1),
        from_session_id: None,
        summary: None,
    })
}

fn exited() -> Event {
    Event::FiberExited(contract::events::FiberExited {
        exit_code: 0,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: Default::default(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        final_message: None,
        error: None,
        suspended_on: None,
        questions: None,
    })
}

#[test]
fn a_rewound_creator_keeps_a_clean_worktree() {
    fakes::within(
        "a_rewound_creator_keeps_a_clean_worktree",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-rewound-home");
            let repo = repo("doors-end-rewound");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            // The creator's log ends `rewound`, and its lock is free: `end`
            // takes it and reads the last line under it.
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let holder = held(&sessions, id, path.to_str().unwrap());
            holder
                .append(&rewound("s_ffffffffffffffff"), None, None)
                .unwrap();
            drop(holder);
            isolation.end();
            assert!(exists(&path));
            assert!(branch_exists(repo.path(), &format!("fiber/{id}")));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn a_creator_ending_otherwise_is_removed_as_before() {
    fakes::within(
        "a_creator_ending_otherwise_is_removed_as_before",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-exited-home");
            let repo = repo("doors-end-exited");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let holder = held(&sessions, id, path.to_str().unwrap());
            holder.append(&exited(), None, None).unwrap();
            drop(holder);
            isolation.end();
            assert!(!exists(&path), "the path is gone");
            assert!(!branch_exists(repo.path(), &format!("fiber/{id}")));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn another_session_ending_rewound_does_not_keep_it() {
    fakes::within(
        "another_session_ending_rewound_does_not_keep_it",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-other-rw-home");
            let repo = repo("doors-end-other-rw");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let holder = held(&sessions, id, path.to_str().unwrap());
            holder.append(&exited(), None, None).unwrap();
            drop(holder);
            // Another session in the same worktree ends `rewound`: only
            // the creator's last line keeps it.
            let other = held(&sessions, "s_0123456789abcde0", path.to_str().unwrap());
            other
                .append(&rewound("s_ffffffffffffffff"), None, None)
                .unwrap();
            drop(other);
            isolation.end();
            assert!(!exists(&path), "the path is gone");
            assert!(!branch_exists(repo.path(), &format!("fiber/{id}")));
        },
    );
}

#[test]
fn a_continued_session_keeps_a_clean_worktree_after_it_exits() {
    fakes::within(
        "a_continued_session_keeps_a_clean_worktree",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-continues-home");
            let repo = repo("doors-end-continues");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let recorded = isolation.worktree();
            let path = isolation.path().to_path_buf();
            let sessions = home.path().join("projects").join(&key).join("sessions");
            // The creator exits, and its lock is free: `end` takes it
            // and reads the last line under it.
            let holder = held(&sessions, id, path.to_str().unwrap());
            holder.append(&exited(), None, None).unwrap();
            drop(holder);
            // The new session ran where the old one did and exited
            // first: its `session_started` records the same worktree
            // path, and its lock is free too.
            let next = held_with_worktree(
                &sessions,
                "s_0123456789abcde0",
                path.to_str().unwrap(),
                Some(recorded),
            );
            next.append(&exited(), None, None).unwrap();
            drop(next);
            isolation.end();
            assert!(exists(&path));
            assert!(branch_exists(repo.path(), &format!("fiber/{id}")));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn a_session_merely_under_the_path_does_not_keep_it() {
    fakes::within(
        "a_session_merely_under_the_path_does_not_keep_it",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-continues-under-home");
            let repo = repo("doors-end-continues-under");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let holder = held(&sessions, id, path.to_str().unwrap());
            holder.append(&exited(), None, None).unwrap();
            drop(holder);
            // Another session works under the worktree but records a
            // different worktree: a workspace comparison would keep.
            let other = held_with_worktree(
                &sessions,
                "s_0123456789abcde0",
                path.join("sub").to_str().unwrap(),
                Some(Worktree {
                    path: "/elsewhere".to_owned(),
                    branch: "fiber/x".to_owned(),
                    repository: None,
                }),
            );
            other.append(&exited(), None, None).unwrap();
            drop(other);
            isolation.end();
            assert!(!exists(&path), "the path is gone");
            assert!(!branch_exists(repo.path(), &format!("fiber/{id}")));
        },
    );
}

#[test]
fn the_creators_own_row_does_not_keep_it() {
    fakes::within(
        "the_creators_own_row_does_not_keep_it",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-continues-own-home");
            let repo = repo("doors-end-continues-own");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let recorded = isolation.worktree();
            let path = isolation.path().to_path_buf();
            let sessions = home.path().join("projects").join(&key).join("sessions");
            // The creator's own `session_started` always records the
            // worktree it created: without excluding its id, every
            // `end` would keep.
            let holder = held_with_worktree(&sessions, id, path.to_str().unwrap(), Some(recorded));
            holder.append(&exited(), None, None).unwrap();
            drop(holder);
            isolation.end();
            assert!(!exists(&path), "the path is gone");
            assert!(!branch_exists(repo.path(), &format!("fiber/{id}")));
        },
    );
}

#[test]
fn a_rewound_creator_keeps_a_broken_worktree_without_a_diagnostic_line() {
    fakes::within(
        "a_rewound_creator_keeps_a_broken_worktree_without_a_diagnostic_line",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-broken-home");
            let repo = repo("doors-end-broken");
            let key = key_of(repo.path());
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let sessions = home.path().join("projects").join(&key).join("sessions");
            let holder = held(&sessions, id, path.to_str().unwrap());
            holder
                .append(&rewound("s_ffffffffffffffff"), None, None)
                .unwrap();
            drop(holder);
            // No `.git`: no git command could inspect it. Kept with no
            // line proves none ran after the scan.
            fs::remove_file(path.join(".git")).unwrap();
            isolation.end();
            assert!(exists(&path));
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

/// An open descriptor on the existing lock file at `path`.
fn open_lock(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

/// The lock file beside `path`.
fn lock_of(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    path.with_file_name(format!("{name}.lock"))
}

#[test]
fn recreate_makes_a_removed_worktree_again_at_the_new_head() {
    fakes::within(
        "recreate_makes_a_removed_worktree_again_at_the_new_head",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-recreate-home");
            let repo = repo("doors-recreate");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let recorded = isolation.worktree();
            let path = PathBuf::from(&recorded.path);
            let branch = recorded.branch.clone();
            let repository = PathBuf::from(recorded.repository.clone().unwrap());
            isolation.end();
            assert!(!exists(&path), "the worktree is gone");
            assert!(!branch_exists(repo.path(), &branch), "the branch is gone");
            fs::write(repo.path().join("second.txt"), "y").unwrap();
            git(repo.path(), &["add", "."]);
            git(repo.path(), &["commit", "--quiet", "-m", "second"]);
            let head = git(repo.path(), &["rev-parse", "HEAD"]);
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let made = match super::recreate(
                &repository,
                &path,
                &branch,
                home.path(),
                &SessionId(id.into()),
                clock,
                &direct,
            )
            .unwrap()
            {
                super::Recreated::Made(made) => made,
                super::Recreated::Present(_) => panic!("the path is absent"),
            };
            assert_eq!(made.path(), path);
            assert_eq!(made.worktree(), recorded);
            assert_eq!(git(repo.path(), &["rev-parse", &branch]), head);
            assert!(path.join("second.txt").is_file());
            // A clean recreated worktree ends against its new base.
            made.end();
            assert!(!exists(&path), "the worktree is gone again");
            assert!(!branch_exists(repo.path(), &branch));
            assert!(!exists(&lock_of(&path)), "the lock file is unlinked");
            assert!(!home.path().join("logs").exists(), "no diagnostic line");
        },
    );
}

#[test]
fn a_recreated_worktree_with_a_commit_beyond_its_base_is_kept() {
    fakes::within(
        "a_recreated_worktree_with_a_commit_beyond_its_base_is_kept",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-recreate-kept-home");
            let repo = repo("doors-recreate-kept");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let recorded = isolation.worktree();
            let path = PathBuf::from(&recorded.path);
            let branch = recorded.branch.clone();
            let repository = PathBuf::from(recorded.repository.clone().unwrap());
            isolation.end();
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let made = match super::recreate(
                &repository,
                &path,
                &branch,
                home.path(),
                &SessionId(id.into()),
                clock,
                &direct,
            )
            .unwrap()
            {
                super::Recreated::Made(made) => made,
                super::Recreated::Present(_) => panic!("the path is absent"),
            };
            fs::write(path.join("more.txt"), "y").unwrap();
            git(&path, &["add", "."]);
            git(&path, &["commit", "--quiet", "-m", "more"]);
            made.end();
            assert!(exists(&path), "the worktree stays");
            assert!(branch_exists(repo.path(), &branch));
            assert!(lock_of(&path).is_file(), "the lock file stays");
        },
    );
}

#[test]
fn recreate_with_a_missing_repository_makes_nothing() {
    fakes::within(
        "recreate_with_a_missing_repository_makes_nothing",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-recreate-norepo");
            let repository = home.path().join("no-such-repo");
            let path = home.path().join("wt");
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let error = match super::recreate(
                &repository,
                &path,
                "fiber/x",
                home.path(),
                &SessionId("s_0123456789abcdef".into()),
                clock,
                &direct,
            ) {
                Err(error) => error,
                Ok(_) => panic!("a missing repository recreates nothing"),
            };
            assert_eq!(error.failure.code, ErrorCode::IoFailed);
            assert_eq!(
                error.failure.message,
                format!(
                    "The repository {} is gone, so the worktree {} cannot be made again.",
                    repository.display(),
                    path.display()
                )
            );
            assert!(error.isolation.is_none());
            assert!(!exists(&path), "nothing was made");
            // The claim's hold was released: a share takes hold.
            let _shared = super::share(&path).unwrap();
        },
    );
}

#[test]
fn recreate_with_an_existing_branch_keeps_it_where_it_was() {
    fakes::within(
        "recreate_with_an_existing_branch_keeps_it_where_it_was",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-recreate-branch");
            let repo = repo("doors-recreate-branch");
            let branch = "fiber/x";
            git(repo.path(), &["branch", branch]);
            let sha = git(repo.path(), &["rev-parse", branch]);
            let repository = repo.path().join(".git");
            let path = home.path().join("wt");
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let error = match super::recreate(
                &repository,
                &path,
                branch,
                home.path(),
                &SessionId("s_0123456789abcdef".into()),
                clock,
                &direct,
            ) {
                Err(error) => error,
                Ok(_) => panic!("an existing branch recreates nothing"),
            };
            assert_eq!(error.failure.code, ErrorCode::IoFailed);
            assert_eq!(
                error.failure.message,
                format!(
                    "refs/heads/{branch} already exists, so Fiber did not make a worktree there."
                )
            );
            assert!(error.isolation.is_none());
            assert_eq!(git(repo.path(), &["rev-parse", branch]), sha);
            assert!(!exists(&path), "the path was left alone");
        },
    );
}

#[test]
fn two_concurrent_recreations_share_the_one_worktree() {
    fakes::within(
        "two_concurrent_recreations_share_the_one_worktree",
        Deadline::after(BUDGET),
        || {
            use std::sync::atomic::{AtomicBool, Ordering};

            let wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-recreate-race-home");
            let repo = repo("doors-recreate-race");
            let common = repo.path().join(".git");
            let path = home.path().join("wt");
            let branch = "fiber/x".to_owned();
            let head = git(repo.path(), &["rev-parse", "HEAD"]);
            let id = "s_0123456789abcdef";
            let (in_add_tx, in_add_rx) = mpsc::channel::<()>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let home_w = home.path().to_path_buf();
            let home_l = home.path().to_path_buf();
            let path_w = path.clone();
            let branch_w = branch.clone();
            let common_w = common.clone();
            let winner = std::thread::spawn(move || {
                let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
                // The winner's add parks inside `git worktree add` while
                // holding the exclusive claim.
                let add = |command: &mut Command| -> io::Result<Output> {
                    in_add_tx.send(()).unwrap();
                    wait.recv_or_fail(&release_rx, "the test releases the add");
                    command.output()
                };
                match super::recreate_with(
                    &common_w,
                    &path_w,
                    &branch_w,
                    &home_w,
                    &SessionId(id.into()),
                    clock,
                    &add,
                    &|| {},
                    &super::shared::Creating::downgrade,
                ) {
                    Ok(recreated) => recreated,
                    Err(error) => panic!("the winner creates: {:?}", error.failure),
                }
            });
            // The winner is inside its add: start the loser behind it.
            wait.recv_or_fail(&in_add_rx, "the winner adds");
            let (waiting_tx, waiting_rx) = mpsc::channel::<()>();
            let added = Arc::new(AtomicBool::new(false));
            let added_in = Arc::clone(&added);
            let path_l = path.clone();
            let branch_l = branch.clone();
            let common_l = common.clone();
            let loser = std::thread::spawn(move || {
                let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
                let add = |command: &mut Command| -> io::Result<Output> {
                    added_in.store(true, Ordering::SeqCst);
                    command.output()
                };
                let before_wait = || waiting_tx.send(()).unwrap();
                match super::recreate_with(
                    &common_l,
                    &path_l,
                    &branch_l,
                    &home_l,
                    &SessionId(id.into()),
                    clock,
                    &add,
                    &before_wait,
                    &super::shared::Creating::downgrade,
                ) {
                    Ok(recreated) => recreated,
                    Err(error) => panic!("the loser shares: {:?}", error.failure),
                }
            });
            wait.recv_or_fail(&waiting_rx, "the loser waits");
            release_tx.send(()).unwrap();
            let isolation = match winner.join().unwrap() {
                super::Recreated::Made(isolation) => isolation,
                super::Recreated::Present(_) => panic!("the winner creates"),
            };
            let loser_shared = match loser.join().unwrap() {
                super::Recreated::Present(shared) => shared,
                super::Recreated::Made(_) => panic!("the loser never adds"),
            };
            assert!(!added.load(Ordering::SeqCst), "the loser's add never ran");
            assert!(exists(&path), "the worktree stands");
            assert_eq!(git(repo.path(), &["rev-parse", &branch]), head);
            // A clean recreating winner ends while the loser runs: only
            // the flock keeps the worktree, with no session log at all.
            isolation.end();
            assert!(exists(&path), "the worktree is kept");
            assert!(branch_exists(repo.path(), &branch));
            drop(loser_shared);
            // With every share dropped, the lock takes exclusively again.
            let lock = lock_of(&path);
            open_lock(&lock).try_lock().unwrap();
        },
    );
}

#[test]
fn a_share_before_the_end_keeps_the_worktree() {
    fakes::within(
        "a_share_before_the_end_keeps_the_worktree",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-shared-home");
            let repo = repo("doors-end-shared");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let branch = isolation.worktree().branch.clone();
            // A kept-resume sharer runs in the creator's worktree.
            let _shared = super::share(&path).unwrap();
            isolation.end();
            assert!(exists(&path), "the worktree stays");
            assert!(branch_exists(repo.path(), &branch));
            assert!(
                !home.path().join("logs").exists(),
                "kept with no diagnostic line"
            );
        },
    );
}

#[test]
fn an_end_whose_lock_path_is_a_directory_keeps_and_logs() {
    fakes::within(
        "an_end_whose_lock_path_is_a_directory_keeps_and_logs",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-end-lockdir-home");
            let repo = repo("doors-end-lockdir");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let branch = isolation.worktree().branch.clone();
            let lock = lock_of(&path);
            fs::create_dir(&lock).unwrap();
            isolation.end();
            assert!(exists(&path), "the worktree stays");
            assert!(branch_exists(repo.path(), &branch));
            let line = warn_line(home.path(), id);
            assert_eq!(line["level"], "warn");
            assert_eq!(line["code"], "io_failed");
            assert_eq!(
                line["message"],
                format!("{}: {}.", lock.display(), io::ErrorKind::IsADirectory)
            );
        },
    );
}

#[test]
fn a_share_parked_behind_the_end_locks_the_new_file() {
    fakes::within(
        "a_share_parked_behind_the_end_locks_the_new_file",
        Deadline::after(BUDGET),
        || {
            let wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-end-handshake-home");
            let repo = repo("doors-end-handshake");
            let id = "s_0123456789abcdef";
            let isolation = isolate_now(repo.path(), home.path(), id);
            let path = isolation.path().to_path_buf();
            let (parked_tx, parked_rx) = mpsc::channel::<()>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let release_rx = Mutex::new(release_rx);
            let isolation = isolation.with_pause(Arc::new(move || {
                parked_tx.send(()).unwrap();
                wait.recv_or_fail(&release_rx.lock().unwrap(), "the test releases the end");
            }));
            let ending = std::thread::spawn(move || isolation.end());
            // The end holds its exclusive take while parked.
            wait.recv_or_fail(&parked_rx, "the end parks");
            let (waiting_tx, waiting_rx) = mpsc::channel::<()>();
            let sharing = path.clone();
            let sharer = std::thread::spawn(move || {
                let before_wait = || waiting_tx.send(()).unwrap();
                super::shared::share_with(&sharing, &before_wait)
            });
            wait.recv_or_fail(&waiting_rx, "the share waits");
            release_tx.send(()).unwrap();
            ending.join().unwrap();
            assert!(!exists(&path), "the clean worktree is gone");
            // The sharer blocked until the end finished: it holds the new
            // file the removal left behind, never the removed one.
            let shared = sharer.join().unwrap().unwrap();
            let lock = lock_of(&path);
            assert!(lock.is_file(), "the share reopened the lock file");
            assert!(open_lock(&lock).try_lock().is_err(), "the share holds it");
            drop(shared);
        },
    );
}

#[test]
fn a_downgrade_failure_after_a_successful_add_keeps_nothing() {
    fakes::within(
        "a_downgrade_failure_after_a_successful_add_keeps_nothing",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-recreate-downgrade");
            let repo = repo("doors-recreate-downgrade");
            let common = repo.path().join(".git");
            let path = home.path().join("wt");
            let branch = "fiber/x".to_owned();
            let lock = lock_of(&path);
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let id = SessionId("s_0123456789abcdef".into());
            // The downgrade fails after `worktree::recreate` succeeded:
            // the worktree stands, held by the returned isolation.
            let fail = |_creating: super::shared::Creating| {
                Err::<super::shared::Shared, contract::shapes::Failure>(crate::failure(
                    ErrorCode::IoFailed,
                    format!("{}: {}.", lock.display(), io::ErrorKind::PermissionDenied),
                ))
            };
            let error = match super::recreate_with(
                &common,
                &path,
                &branch,
                home.path(),
                &id,
                clock,
                &direct,
                &|| {},
                &fail,
            ) {
                Err(error) => error,
                Ok(_) => panic!("the downgrade fails"),
            };
            assert_eq!(error.failure.code, ErrorCode::IoFailed);
            assert_eq!(
                error.failure.message,
                format!("{}: {}.", lock.display(), io::ErrorKind::PermissionDenied)
            );
            let isolation = error.isolation.expect("the made worktree is kept");
            assert!(exists(&path), "the worktree stands");
            isolation.end();
            assert!(!exists(&path), "the end removes it");
            assert!(!branch_exists(repo.path(), &branch));
            assert!(!exists(&lock), "the lock file is unlinked");
            assert!(
                !home.path().join("logs").exists(),
                "no diagnostic line when the end itself succeeds"
            );
            // A failure of `worktree::recreate` itself keeps no isolation
            // and makes nothing.
            git(repo.path(), &["branch", "fiber/y"]);
            let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let error = match super::recreate_with(
                &common,
                &path,
                "fiber/y",
                home.path(),
                &id,
                clock,
                &direct,
                &|| {},
                &super::shared::Creating::downgrade,
            ) {
                Err(error) => error,
                Ok(_) => panic!("an existing branch recreates nothing"),
            };
            assert!(error.isolation.is_none());
            assert!(!exists(&path), "nothing was made");
        },
    );
}

#[test]
fn recreate_error_debug_names_the_isolation_state() {
    let failure = || Failure {
        code: ErrorCode::IoFailed,
        message: "the worktree could not be made".to_owned(),
        retry_after_ms: None,
        provider: None,
    };
    let without = RecreateError {
        failure: failure(),
        isolation: None,
    };
    let text = format!("{without:?}");
    assert!(text.contains("RecreateError"), "{text}");
    assert!(text.contains("isolation: false"), "{text}");

    fakes::within(
        "recreate_error_debug_names_the_isolation_state",
        Deadline::after(BUDGET),
        move || {
            let home = fakes::TempDir::new("doors-debug-home");
            let repo = repo("doors-debug-repo");
            let with = RecreateError {
                failure: failure(),
                isolation: Some(isolate_now(repo.path(), home.path(), "s_0123456789abcdef")),
            };
            let text = format!("{with:?}");
            assert!(text.contains("RecreateError"), "{text}");
            assert!(text.contains("isolation: true"), "{text}");
            if let Some(isolation) = with.isolation {
                isolation.end();
            }
        },
    );
}

#[test]
fn same_worktree_with_the_exact_spelling_is_true_even_when_unresolvable() {
    let missing = "/nonexistent-fiber-worktree-s_0123456789abcdef";
    assert!(same_worktree(missing, Path::new(missing)));
}

#[test]
fn same_worktree_with_an_alias_spelling_of_one_directory_is_true() {
    let dir = fakes::TempDir::new("doors-same-worktree-alias");
    let aliased = format!("{}/", dir.path().display());
    assert!(same_worktree(&aliased, dir.path()));
}

#[test]
fn same_worktree_with_two_different_directories_is_false() {
    let first = fakes::TempDir::new("doors-same-worktree-first");
    let second = fakes::TempDir::new("doors-same-worktree-second");
    assert!(!same_worktree(
        first.path().to_str().unwrap(),
        second.path()
    ));
}

#[test]
fn same_worktree_with_one_side_missing_is_false() {
    let dir = fakes::TempDir::new("doors-same-worktree-present");
    assert!(!same_worktree(
        "/nonexistent-fiber-worktree-s_0123456789abcdef",
        dir.path()
    ));
}
