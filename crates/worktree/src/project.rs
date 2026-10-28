//! A workspace's project: git's shared directory inside a repository,
//! otherwise the launch directory itself.

use std::path::{Path, PathBuf};
use std::process::Stdio;

/// A workspace's project and whether git found a repository there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// The project's identity path (`docs/state.md`, "Projects"): git's
    /// shared directory inside a repository, otherwise the launch directory
    /// itself, symlinks resolved.
    pub path: PathBuf,
    /// Whether `git rev-parse` found a repository at the launch directory,
    /// a bare repository and a `.git` directory included.
    pub in_repository: bool,
}

/// The project's identity path (`docs/state.md`, "Projects"), from
/// [`resolve_project`].
pub fn project(launch: &Path) -> PathBuf {
    resolve_project(launch).path
}

/// Resolves the project of `launch` with one `git rev-parse`: git's shared
/// directory when `launch` is inside a repository, otherwise `launch`
/// itself, symlinks resolved. The invocation carries the scrubbed
/// environment (`support::git::command`): `core.fsmonitor` off,
/// `GIT_OPTIONAL_LOCKS` and `GIT_TERMINAL_PROMPT` pinned to `0`, and the
/// `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR` and `GIT_INDEX_FILE`
/// variables removed, so the caller's environment cannot redirect the
/// discovery.
pub fn resolve_project(launch: &Path) -> Project {
    let common = support::git::command("git")
        .arg("-C")
        .arg(launch)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stderr(Stdio::null())
        .output();
    let (identity, in_repository) = match common {
        Ok(out) if out.status.success() => (
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n')),
            true,
        ),
        // Not a repository, or no git: the launch directory is the project.
        Ok(_) | Err(_) => (launch.to_path_buf(), false),
    };
    Project {
        path: identity.canonicalize().unwrap_or(identity),
        in_repository,
    }
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
