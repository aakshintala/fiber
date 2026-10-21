//! Running the system `git` in tests: one committed repository per test,
//! and probes of its branches.

use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::MUST_SUCCEED_WITHIN;
use crate::TempDir;
use crate::deadline::Deadline;

/// Runs `git` with the test identity in `dir` and returns its trimmed
/// stdout. Git leads its own process group and runs under
/// [`MUST_SUCCEED_WITHIN`]: a stalled git or hook fails naming the wait
/// instead of hanging the runner.
///
/// # Panics
///
/// When `git` cannot run, exits non-zero, runs past its deadline, or its
/// stdout is not UTF-8.
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test helper; a failure is the test's"
)]
pub fn git(dir: &Path, args: &[&str]) -> String {
    let child = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let what = format!("git {args:?}");
    let out = crate::finished(&what, child, &[], Deadline::after(MUST_SUCCEED_WITHIN));
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A repository with one commit (`file.txt` holding `x`, message `first`)
/// on `main`, in a temporary directory.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
pub fn repo(prefix: &str) -> TempDir {
    let home = TempDir::new(prefix);
    git(home.path(), &["init", "--quiet"]);
    fs::write(home.path().join("file.txt"), "x").unwrap();
    git(home.path(), &["add", "."]);
    git(home.path(), &["commit", "--quiet", "-m", "first"]);
    home
}

/// Whether `refs/heads/<branch>` still exists in the repository. Git leads
/// its own process group and runs under [`MUST_SUCCEED_WITHIN`], so a stall
/// fails naming the wait; standard input is null, so a pager or prompt
/// never reaches the test.
pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    let child = match Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let what = format!("git rev-parse --verify --quiet refs/heads/{branch}");
    crate::finished(&what, child, &[], Deadline::after(MUST_SUCCEED_WITHIN))
        .status
        .success()
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
