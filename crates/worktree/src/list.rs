//! Lists the worktrees of a repository through `git worktree list`.

use std::path::{Path, PathBuf};

use super::{Error, git_failed, run};

/// One entry of `git worktree list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The worktree's directory, as git printed it.
    pub path: PathBuf,
    /// Whether git marked the entry `bare`: it has no files.
    pub bare: bool,
    /// Whether git marked the entry `locked`.
    pub locked: bool,
}

/// Every worktree of the repository `dir` is in, main first.
pub fn list(dir: &Path) -> Result<Vec<Listed>, Error> {
    list_with("git", dir)
}

/// Lists with `program` as `git`, so tests can point at a missing one.
fn list_with(program: &str, dir: &Path) -> Result<Vec<Listed>, Error> {
    let out = run(
        program,
        dir,
        dir,
        &["worktree", "list", "--porcelain", "-z"],
    )?;
    if !out.status.success() {
        return Err(git_failed(dir, "worktree list", &out));
    }
    parse(&out.stdout, dir)
}

/// Reads one `git worktree list --porcelain -z` output: each field ends
/// in NUL, and an empty field ends the entry. Unknown fields are
/// ignored, so a newer git never shifts an entry.
fn parse(stdout: &[u8], dir: &Path) -> Result<Vec<Listed>, Error> {
    let mut entries = Vec::new();
    let mut current: Option<Listed> = None;
    for record in stdout.split(|byte| *byte == 0) {
        if record.is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }
        if let Some(rest) = record.strip_prefix(b"worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            #[cfg(unix)]
            let path = {
                use std::os::unix::ffi::OsStrExt;
                PathBuf::from(std::ffi::OsStr::from_bytes(rest))
            };
            #[cfg(not(unix))]
            let path = PathBuf::from(String::from_utf8_lossy(rest).into_owned());
            current = Some(Listed {
                path,
                bare: false,
                locked: false,
            });
        } else if record == b"bare"
            && let Some(entry) = current.as_mut()
        {
            entry.bare = true;
        } else if (record == b"locked" || record.starts_with(b"locked "))
            && let Some(entry) = current.as_mut()
        {
            entry.locked = true;
        }
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(Error::Git {
            path: dir.to_owned(),
            command: "worktree list".to_owned(),
            stderr: "no worktrees listed".to_owned(),
        });
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
