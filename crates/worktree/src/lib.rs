//! Creates and removes the git worktrees sessions and delegates run in, by
//! running the `git` program (`docs/architecture.md`).

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use contract::ErrorCode;

/// What can go wrong inspecting or removing a worktree.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `git` is not installed.
    #[error("git is not installed")]
    GitMissing,
    /// A `git` command on a worktree failed.
    #[error("{path}: git {command}: {stderr}")]
    Git {
        /// The worktree the command ran for.
        path: PathBuf,
        /// The subcommand, such as `rev-parse`.
        command: String,
        /// The command's stderr.
        stderr: String,
    },
    /// A file system call failed.
    #[error("{path}: {source}")]
    Io {
        /// The path the call was for.
        path: PathBuf,
        /// The failure.
        source: std::io::Error,
    },
}

impl Error {
    /// The stable code a caller switches on (`docs/errors.md`, "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::GitMissing => ErrorCode::Usage,
            Self::Git { .. } => ErrorCode::IoFailed,
            Self::Io { .. } => ErrorCode::IoFailed,
        }
    }
}

/// What `inspect` found in a linked worktree.
#[derive(Debug)]
pub struct Inspected {
    /// The short branch the worktree checks out.
    pub branch: String,
    /// The repository's common directory, to run removals from.
    pub common_dir: PathBuf,
    /// Whether `git status` reports anything, ignored files included.
    pub uncommitted: bool,
    /// How many commits on the branch are on no other branch or remote.
    pub unique_commits: u64,
}

/// What `inspect` found at a path.
#[derive(Debug)]
pub enum Inspection {
    /// A linked worktree on a branch.
    Worktree(Inspected),
    /// Not a linked worktree: no `.git` entry, a toplevel that differs, or
    /// a main worktree.
    NotAWorktree,
    /// A worktree with a detached HEAD.
    Detached,
}

/// Inspects the worktree at `path` through `git`.
pub fn inspect(path: &Path) -> Result<Inspection, Error> {
    inspect_with("git", path)
}

/// Inspects with `program` as `git`, so tests can point at a missing one.
fn inspect_with(program: &str, path: &Path) -> Result<Inspection, Error> {
    // Without a `.git` entry the path is not a worktree, and no `git`
    // command runs.
    match std::fs::symlink_metadata(path.join(".git")) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(Inspection::NotAWorktree);
        }
        Err(source) => {
            return Err(Error::Io {
                path: path.to_owned(),
                source,
            });
        }
    }
    let out = run(
        program,
        path,
        path,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ],
    )?;
    if !out.status.success() {
        return Err(git_failed(path, "rev-parse", &out));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let toplevel = PathBuf::from(lines.next().unwrap_or(""));
    let common = PathBuf::from(lines.next().unwrap_or(""));
    // Toplevel is the worktree's own directory: anything else, such as a
    // plain directory under a repository, is not one. A main worktree
    // points at its own `.git` as the common directory.
    let canonical = std::fs::canonicalize(path).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })?;
    if toplevel != canonical || common == canonical.join(".git") {
        return Ok(Inspection::NotAWorktree);
    }
    let out = run(
        program,
        path,
        path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?;
    let branch = if out.status.success() {
        let branch = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if branch.is_empty() {
            return Err(git_failed(path, "symbolic-ref", &out));
        }
        branch
    } else if out.status.code() == Some(1) {
        return Ok(Inspection::Detached);
    } else {
        return Err(git_failed(path, "symbolic-ref", &out));
    };
    // `git worktree remove` deletes ignored files too, so any output,
    // ignored files included, counts as uncommitted. `--untracked-files=all`
    // pins `status.showUntrackedFiles`: `no` in the config would otherwise
    // suppress ignored files too, hiding an ignored-only worktree.
    // `--ignore-submodules=none` pins `submodule.*.ignore`: `all` would
    // otherwise hide a modified submodule. The Git command builder turns
    // `core.fsmonitor` off: a hook that reports nothing changed would
    // otherwise hide a modified tracked file.
    let out = run(
        program,
        path,
        path,
        &[
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    if !out.status.success() {
        return Err(git_failed(path, "status", &out));
    }
    let uncommitted = !out.stdout.is_empty();
    // The `--exclude` pattern names the branch without `refs/heads/`,
    // because a pattern before `--branches` matches with that prefix
    // removed. It applies only to the `--branches` right after it, so
    // every remote-tracking ref still counts.
    let head = format!("refs/heads/{branch}");
    let exclude = format!("--exclude={branch}");
    let out = run(
        program,
        path,
        path,
        &[
            "rev-list",
            "--count",
            &head,
            "--not",
            &exclude,
            "--branches",
            "--remotes",
        ],
    )?;
    if !out.status.success() {
        return Err(git_failed(path, "rev-list", &out));
    }
    let count = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|_| git_failed(path, "rev-list", &out))?;
    Ok(Inspection::Worktree(Inspected {
        branch,
        common_dir: common,
        uncommitted,
        unique_commits: count,
    }))
}

/// How `remove` ended.
#[derive(Debug)]
pub enum Removed {
    /// The worktree and its branch are gone.
    Whole,
    /// The worktree is gone but its branch stays, with why.
    BranchKept(Error),
}

/// Removes the worktree at `path` and its branch, through `git` run from
/// the common directory.
pub fn remove(path: &Path, inspected: &Inspected, force: bool) -> Result<Removed, Error> {
    let mut remove = git_command("git");
    remove
        .arg("-C")
        .arg(&inspected.common_dir)
        .arg("worktree")
        .arg("remove");
    if force {
        remove.arg("--force");
    }
    remove.arg(path).stdin(Stdio::null());
    let out = remove
        .output()
        .map_err(|source| spawn_failed(path, source))?;
    if !out.status.success() {
        return Err(git_failed(path, "worktree remove", &out));
    }
    let out = git_command("git")
        .arg("-C")
        .arg(&inspected.common_dir)
        .args(["branch", "-D", &inspected.branch])
        .stdin(Stdio::null())
        .output()
        .map_err(|source| spawn_failed(path, source))?;
    if out.status.success() {
        Ok(Removed::Whole)
    } else {
        Ok(Removed::BranchKept(git_failed(path, "branch -D", &out)))
    }
}

/// Runs a `git` read for `path` in `dir`: stdin is null, and no lock is
/// taken. A spawn failure of kind `NotFound` means `git` is missing.
fn run(program: &str, dir: &Path, path: &Path, args: &[&str]) -> Result<Output, Error> {
    git_command(program)
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| spawn_failed(path, source))
}

/// Runs Git with `core.fsmonitor` off and without the environment that
/// can redirect reads or removals elsewhere.
fn git_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .args(["-c", "core.fsmonitor=false"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE");
    command
}

/// A `git` command that failed: its stderr, or how it exited when stderr
/// is empty.
fn git_failed(path: &Path, command: &str, out: &Output) -> Error {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    let stderr = if stderr.is_empty() {
        format!("exited with {}", out.status)
    } else {
        stderr
    };
    Error::Git {
        path: path.to_owned(),
        command: command.to_owned(),
        stderr,
    }
}

/// A `git` spawn failure: kind `NotFound` means `git` is missing, and any
/// other failure is an I/O error on the worktree's path.
fn spawn_failed(path: &Path, source: io::Error) -> Error {
    if source.kind() == io::ErrorKind::NotFound {
        Error::GitMissing
    } else {
        Error::Io {
            path: path.to_owned(),
            source,
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
