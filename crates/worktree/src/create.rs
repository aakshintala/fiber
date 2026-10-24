//! Creating a worktree on a new branch at a repository's HEAD.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::{Error, git_failed, run};

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

/// Makes `branch` at the repository's HEAD commit and checks it out at
/// `path`, returning the canonical path, the branch and the base commit.
/// `repository` is the repository's git common directory, such as
/// `<repo>/.git`: it is validated before anything is made, by reading
/// the common directory `git` resolves there and comparing it against
/// `repository` with symlinks resolved, so a plain directory, a
/// work-tree root or a missing directory is `NotARepository`. After that
/// the call behaves exactly like [`create`]: the path and branch checks,
/// the base read at the current HEAD, the one hook-running
/// `git worktree add -b`, the rollback on failure and the canonical path.
/// `add` runs the one hook-running command, like `Command::output`, or a
/// cancellable runner.
pub fn recreate(
    repository: &Path,
    path: &Path,
    branch: &str,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
) -> Result<Created, Error> {
    recreate_with("git", repository, path, branch, add)
}

/// [`recreate`] with `program` as `git`, so tests can point at a missing one.
fn recreate_with(
    program: &str,
    repository: &Path,
    path: &Path,
    branch: &str,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
) -> Result<Created, Error> {
    let out = run(
        program,
        repository,
        repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !out.status.success() {
        return Err(Error::NotARepository {
            path: repository.to_owned(),
        });
    }
    let resolved = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_owned());
    let matches = std::fs::canonicalize(&resolved)
        .and_then(|common| std::fs::canonicalize(repository).map(|given| common == given))
        .unwrap_or(false);
    if !matches {
        return Err(Error::NotARepository {
            path: repository.to_owned(),
        });
    }
    shared_body(program, repository, path, branch, add)
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
    shared_body(program, launch, path, branch, add)
}

/// The path check, branch check, base read, the one hook-running
/// `worktree add -b`, the rollback on failure and the canonical path.
/// Both [`create`] and [`recreate`] run this after their own repository
/// check, with the directory their git commands run from.
fn shared_body(
    program: &str,
    repo: &Path,
    path: &Path,
    branch: &str,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
) -> Result<Created, Error> {
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
        repo,
        repo,
        &["rev-parse", "--verify", "--quiet", &head],
    )?;
    if out.status.success() {
        return Err(Error::Exists { path: head.into() });
    }
    // The base is read once, and the branch is made exactly at it: an
    // unborn HEAD fails here, before anything is created.
    let out = run(
        program,
        repo,
        repo,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )?;
    if !out.status.success() {
        return Err(git_failed(repo, "rev-parse", &out));
    }
    let base = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    // The one command that runs a hook (`post-checkout`, which Git LFS
    // needs): its output is piped, never the caller's. Every other git
    // command in this crate runs hook-free through
    // `support::git::command`.
    let mut command = support::git::hooked_command(program);
    command
        .arg("-C")
        .arg(repo)
        .args(["worktree", "add", "-b", branch])
        .arg(path)
        .arg(&base)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = match add(&mut command) {
        Ok(out) => out,
        Err(source) => {
            rollback(program, repo, path, branch);
            return Err(Error::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if !out.status.success() {
        rollback(program, repo, path, branch);
        return Err(git_failed(path, "worktree add", &out));
    }
    // The checkout is made but unreadable, such as a hook that deleted
    // it: roll back like any other post-add failure, so no branch is
    // left behind.
    let canonical = match std::fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(source) => {
            rollback(program, repo, path, branch);
            return Err(Error::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
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
    match support::git::command(program)
        .arg("-C")
        .arg(launch)
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .output()
    {
        Ok(_) | Err(_) => {}
    }
    match support::git::command(program)
        .arg("-C")
        .arg(launch)
        .args(["branch", "-D", branch])
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
