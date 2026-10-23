//! The git repository guards (`docs/tools.md`, "Shell", "Effects").
//!
//! A git read is `executes` in a bare repository, or where `.git` is a
//! file whose target Fiber cannot verify: the new directory can hold hooks
//! or configuration the person never saw, and neither case is proven safe.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// The most bytes read from a `.git` file: a longer file is `executes`.
pub(super) const MAX_GITFILE_BYTES: usize = 512;

/// Whether `dir` is a safe place to run a git read: every ancestor up to
/// the filesystem root holds no bare repository, and the nearest `.git`
/// is either a directory or a file naming a directory under a `.git`.
/// No repository found, or a missing path, counts as safe: the call still
/// declares `reads`, and the run fails on its own when there is nothing.
pub(super) fn repository_ok(dir: &Path) -> bool {
    let mut current = dir.to_path_buf();
    loop {
        match std::fs::symlink_metadata(current.join(".git")) {
            Ok(meta) => {
                if meta.is_symlink() {
                    // A symlinked `.git` can point anywhere, so the call
                    // is reviewed.
                    return false;
                }
                if meta.is_dir() {
                    // A normal repository ends the walk.
                    return true;
                }
                if meta.is_file() {
                    return gitfile_ok(&current.join(".git"), &current);
                }
                return false;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            // Any I/O error other than not-found leaves the repository
            // unverified, so the call is reviewed.
            Err(_) => return false,
        }
        if is_bare(&current) {
            return false;
        }
        if !current.pop() {
            return true;
        }
    }
}

/// Whether `dir` is a bare repository: it holds `HEAD` as a file with
/// `objects/` and `refs/` beside it, and no `.git` (the caller checked).
fn is_bare(dir: &Path) -> bool {
    let head = std::fs::symlink_metadata(dir.join("HEAD")).is_ok_and(|meta| meta.is_file());
    let objects = std::fs::symlink_metadata(dir.join("objects")).is_ok_and(|meta| meta.is_dir());
    let refs = std::fs::symlink_metadata(dir.join("refs")).is_ok_and(|meta| meta.is_dir());
    head && objects && refs
}

/// Whether a `.git` file names a verifiable target: one line
/// `gitdir: <path>` with at most one trailing newline, resolving relative
/// to the file's directory to a directory under a `.git`, where worktrees
/// and submodules live. Anything else is `executes`.
fn gitfile_ok(gitfile: &Path, dir: &Path) -> bool {
    let file = match std::fs::File::open(gitfile) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_GITFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return false;
    }
    if bytes.len() > MAX_GITFILE_BYTES {
        return false;
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => return false,
    };
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains('\n') {
        return false;
    }
    let Some(target) = line.strip_prefix("gitdir: ") else {
        return false;
    };
    if target.is_empty() {
        return false;
    }
    let joined = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        dir.join(target)
    };
    let canonical = match joined.canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => return false,
    };
    if !canonical.is_dir() {
        return false;
    }
    canonical
        .components()
        .any(|component| matches!(component, Component::Normal(name) if name == OsStr::new(".git")))
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
