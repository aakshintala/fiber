//! Creating a worktree on a new branch at a repository's HEAD.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::{Error, git_command, git_failed, hooked_command, run};

/// A worktree `create` made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// The worktree's path, symlinks resolved.
    pub path: PathBuf,
    /// The new branch's short name, such as `fiber/s_0123456789abcdef`.
    pub branch: String,
    /// The full commit id the branch was made at.
    pub base: String,
}

/// Makes `branch` at the launch repository's HEAD commit and checks it out
/// at `path`, returning the canonical path, the branch and the base commit.
/// `add` runs the one hook-running command, `git worktree add`: like
/// `Command::output`, or a cancellable runner.
///
/// Refuses an existing path or branch before `git` runs, so a failed `add`
/// can only have made the two names it was given: whatever exists under
/// them afterwards is removed again, with the branch deleted, before the
/// original failure is returned.
pub fn create(
    launch: &Path,
    path: &Path,
    branch: &str,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
) -> Result<Created, Error> {
    create_with("git", launch, path, branch, add)
}

/// [`create`] with `program` as `git`, so tests can point at a missing one.
fn create_with(
    program: &str,
    launch: &Path,
    path: &Path,
    branch: &str,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
) -> Result<Created, Error> {
    // Anything but `true`: a plain directory fails the command, and the
    // inside of `.git` prints `false`.
    let out = run(
        program,
        launch,
        launch,
        &["rev-parse", "--is-inside-work-tree"],
    )?;
    if !out.status.success() || String::from_utf8_lossy(&out.stdout).trim() != "true" {
        return Err(Error::NotARepository {
            path: launch.to_owned(),
        });
    }
    // `symlink_metadata` does not follow the final link: a dangling
    // symlink is an existing path too.
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(Error::Exists {
                path: path.to_owned(),
            });
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::Io {
                path: path.to_owned(),
                source,
            });
        }
    }
    let head = format!("refs/heads/{branch}");
    let out = run(
        program,
        launch,
        launch,
        &["rev-parse", "--verify", "--quiet", &head],
    )?;
    if out.status.success() {
        return Err(Error::Exists { path: head.into() });
    }
    // The base is read once, and the branch is made exactly at it: an
    // unborn HEAD fails here, before anything is created.
    let out = run(
        program,
        launch,
        launch,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )?;
    if !out.status.success() {
        return Err(git_failed(launch, "rev-parse", &out));
    }
    let base = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    // The one command that runs a hook (`post-checkout`, which Git LFS
    // needs): its output is piped, never the caller's. Every other git
    // command in this crate runs hook-free through `git_command`.
    let mut command = hooked_command(program);
    command
        .arg("-C")
        .arg(launch)
        .args(["worktree", "add", "-b", branch])
        .arg(path)
        .arg(&base)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = match add(&mut command) {
        Ok(out) => out,
        Err(source) => {
            rollback(program, launch, path, branch);
            return Err(Error::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if !out.status.success() {
        rollback(program, launch, path, branch);
        return Err(git_failed(path, "worktree add", &out));
    }
    let canonical = std::fs::canonicalize(path).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(Created {
        path: canonical,
        branch: branch.to_owned(),
        base,
    })
}

/// Removes what a failed `git worktree add` may have made: the worktree it
/// was given, then the branch this same call proved absent beforehand. Each
/// runs hook-free and keeps its own result: a cleanup step that fails is
/// left for prune. Never runs after [`create`] has returned `Ok`.
fn rollback(program: &str, launch: &Path, path: &Path, branch: &str) {
    match git_command(program)
        .arg("-C")
        .arg(launch)
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
    {
        Ok(_) | Err(_) => {}
    }
    match git_command(program)
        .arg("-C")
        .arg(launch)
        .args(["branch", "-D", branch])
        .stdin(Stdio::null())
        .output()
    {
        Ok(_) | Err(_) => {}
    }
}

/// How many commits on `branch` are not reachable from `base`: a commit
/// that also reached another branch still counts, so a worktree with one
/// is kept.
pub fn commits_beyond(path: &Path, branch: &str, base: &str) -> Result<u64, Error> {
    let range = format!("{base}..refs/heads/{branch}");
    let out = run("git", path, path, &["rev-list", "--count", &range])?;
    if !out.status.success() {
        return Err(git_failed(path, "rev-list", &out));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|_| git_failed(path, "rev-list", &out))
}

#[cfg(test)]
#[path = "create_tests.rs"]
mod tests;
