//! Tests for project resolution through real `git` in temporary
//! directories.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use super::{project, resolve_project};
use fakes::Deadline;
use fakes::git::git;

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

#[test]
fn project_ignores_a_hostile_git_environment() {
    if std::env::var("FIBER_WORKTREE_PROJECT_ENV_CHILD").is_ok() {
        fakes::within(
            "the child's project checks",
            Deadline::after(CHILD_BUDGET),
            || {
                let repo = PathBuf::from(std::env::var("FIBER_WORKTREE_PROJECT_ENV_REPO").unwrap());
                let common = fs::canonicalize(repo.join(".git")).unwrap();
                assert_eq!(project(&repo), common);
                assert_eq!(project(&repo.join("docs")), common);
                let resolved = resolve_project(&repo);
                assert_eq!(resolved.path, common);
                assert!(resolved.in_repository);
            },
        );
        return;
    }
    fakes::within(
        "project_ignores_a_hostile_git_environment",
        Deadline::after(RERUN_OUTER),
        || {
            let temp = fakes::TempDir::new("worktree-project");
            let repo = temp.path().join("repo");
            fs::create_dir_all(repo.join("docs")).unwrap();
            git(&repo, &["init", "--quiet"]);
            let other = temp.path().join("other");
            fs::create_dir_all(&other).unwrap();
            git(&other, &["init", "--quiet"]);
            let other_git = other.join(".git").to_str().unwrap().to_owned();
            let other_work = other.to_str().unwrap().to_owned();
            let other_index = other
                .join(".git")
                .join("index")
                .to_str()
                .unwrap()
                .to_owned();
            let name = module_path!().split_once("::").unwrap().1;
            let out = fakes::rerun_within(
                &format!("{name}::project_ignores_a_hostile_git_environment"),
                &[
                    ("GIT_DIR", other_git.as_str()),
                    ("GIT_WORK_TREE", other_work.as_str()),
                    ("GIT_COMMON_DIR", other_git.as_str()),
                    ("GIT_INDEX_FILE", other_index.as_str()),
                    ("FIBER_WORKTREE_PROJECT_ENV_CHILD", "1"),
                    ("FIBER_WORKTREE_PROJECT_ENV_REPO", repo.to_str().unwrap()),
                ],
                Deadline::after(RERUN_WITHIN),
            );
            assert!(
                out.status.success(),
                "project must see past the redirect:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        },
    );
}

#[test]
fn discovery_runs_git_with_the_scrubbed_environment() {
    if std::env::var("FIBER_WORKTREE_SCRUB_CHILD").is_ok() {
        fakes::within(
            "the child's scrub checks",
            Deadline::after(CHILD_BUDGET),
            || {
                let repo = PathBuf::from(std::env::var("FIBER_WORKTREE_SCRUB_REPO").unwrap());
                let resolved = resolve_project(&repo);
                let common = fs::canonicalize(repo.join(".git")).unwrap();
                assert_eq!(resolved.path, common);
                assert!(resolved.in_repository);
            },
        );
        return;
    }
    fakes::within(
        "discovery_runs_git_with_the_scrubbed_environment",
        Deadline::after(RERUN_OUTER),
        || {
            let temp = fakes::TempDir::new("worktree-project");
            let repo = temp.path().join("repo");
            fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "--quiet"]);
            let decoy = temp.path().join("decoy");
            fs::create_dir_all(&decoy).unwrap();
            git(&decoy, &["init", "--quiet"]);
            // An executable `git` that logs every discovery invocation's
            // argv and watched environment, then runs the real git: the
            // log pins the scrub set discovery invoked it with.
            let bin = temp.path().join("stub-bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(
                bin.join("git"),
                r#"#!/bin/sh
{
printf 'argv:'
for arg in "$@"; do printf ' %s' "$arg"; done
printf '\n'
for name in GIT_OPTIONAL_LOCKS GIT_TERMINAL_PROMPT GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE; do
 eval "present=\${$name+x}"
 if [ -n "$present" ]; then eval "printf '%s=%s\n' \"$name\" \"\$$name\""; fi
done
} >> "$FIBER_WORKTREE_SCRUB_LOG"
# The stub directory is first on PATH, so dropping it reaches the real git.
PATH=${PATH#*:} exec git "$@"
"#,
            )
            .unwrap();
            let stub = bin.join("git");
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
            let log = temp.path().join("git.log");
            let path = format!(
                "{}:{}",
                bin.to_str().unwrap(),
                std::env::var("PATH").unwrap_or_default()
            );
            let repo_str = repo.to_str().unwrap().to_owned();
            let log_str = log.to_str().unwrap().to_owned();
            let decoy_git_str = decoy.join(".git").to_str().unwrap().to_owned();
            let decoy_str = decoy.to_str().unwrap().to_owned();
            let name = module_path!().split_once("::").unwrap().1;
            let out = fakes::rerun_prepared(
                &format!("{name}::discovery_runs_git_with_the_scrubbed_environment"),
                &[
                    ("FIBER_WORKTREE_SCRUB_CHILD", "1"),
                    ("FIBER_WORKTREE_SCRUB_REPO", repo_str.as_str()),
                    ("FIBER_WORKTREE_SCRUB_LOG", log_str.as_str()),
                    ("PATH", path.as_str()),
                    ("GIT_DIR", decoy_git_str.as_str()),
                    ("GIT_WORK_TREE", decoy_str.as_str()),
                    ("GIT_OPTIONAL_LOCKS", "9"),
                    ("GIT_TERMINAL_PROMPT", "ask"),
                ],
                Deadline::after(RERUN_WITHIN),
                |_| {},
            );
            assert!(
                out.status.success(),
                "discovery must judge the workspace repository:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
            let logged = fs::read_to_string(&log).unwrap();
            let lines: Vec<&str> = logged.lines().collect();
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("-c core.fsmonitor=false")),
                "discovery runs git with fsmonitor off:\n{logged}"
            );
            assert!(
                lines.contains(&"GIT_OPTIONAL_LOCKS=0"),
                "discovery pins the lock guard:\n{logged}"
            );
            assert!(
                lines.contains(&"GIT_TERMINAL_PROMPT=0"),
                "discovery pins the prompt guard:\n{logged}"
            );
            for name in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_COMMON_DIR",
                "GIT_INDEX_FILE",
            ] {
                assert!(
                    !lines.iter().any(|line| line.starts_with(name)),
                    "discovery removes {name}:\n{logged}"
                );
            }
        },
    );
}

#[test]
fn the_project_is_gits_shared_directory_or_the_launch_directory() {
    let temp = fakes::TempDir::new("worktree-project");
    let plain = temp.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert_eq!(project(&plain), fs::canonicalize(&plain).unwrap());

    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("docs")).unwrap();
    let init = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&repo)
        .status();
    assert!(init.unwrap().success());
    let common = fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(project(&repo), common);
    assert_eq!(project(&repo.join("docs")), common);
}

#[test]
fn a_project_reports_whether_git_found_a_repository() {
    let temp = fakes::TempDir::new("worktree-project");
    let git = |args: &[&str], dir: &std::path::Path| {
        let init = Command::new("git").args(args).arg(dir).status();
        assert!(init.unwrap().success());
    };
    let plain = temp.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert!(!resolve_project(&plain).in_repository);
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("docs")).unwrap();
    git(&["init", "-q"], &repo);
    assert!(resolve_project(&repo).in_repository);
    assert!(resolve_project(&repo.join("docs")).in_repository);
    assert!(resolve_project(&repo.join(".git")).in_repository);
    let bare = temp.path().join("bare.git");
    git(&["init", "-q", "--bare"], &bare);
    assert!(resolve_project(&bare).in_repository);
}
