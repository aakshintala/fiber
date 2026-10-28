//! The git repository guards (`docs/tools.md`, "Shell", "Effects").
//!
//! A git read is `executes` in a bare repository, or where `.git` is a
//! file whose target Fiber cannot verify: the new directory can hold hooks
//! or configuration the person never saw, and neither case is proven safe.

use std::io::Read;
use std::path::Path;

/// The most bytes read from a `.git` file: a longer file is `executes`.
pub(super) const MAX_GITFILE_BYTES: usize = 512;

/// Whether `dir` is a safe place to run a git read: every ancestor up to
/// the filesystem root holds no bare repository, and the nearest `.git`
/// is either a directory or a file naming a directory under a `.git`.
/// No repository found, or a missing path, counts as safe: the call still
/// declares `reads`, and the run fails on its own when there is nothing.
pub(super) fn repository_ok(dir: &Path) -> bool {
    // A missing starting path is not bare: the bare-ancestor walk below
    // must not fire when the workdir itself is missing.
    match std::fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    }
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
                    return gitfile_ok(&current);
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
/// A marker that exists as a symlink is unsafe, so it counts exactly as
/// a real file or directory does.
pub(super) fn is_bare(dir: &Path) -> bool {
    marker_file(&dir.join("HEAD"))
        && marker_dir(&dir.join("objects"))
        && marker_dir(&dir.join("refs"))
}

/// Whether a bare marker exists as a file. A symlink counts exactly as
/// a real file does.
fn marker_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() || meta.is_symlink())
}

/// Whether a bare marker exists as a directory. A symlink counts exactly
/// as a real directory does; a regular file does not count.
fn marker_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir() || meta.is_symlink())
}

/// Whether `dir`'s `.git` file names a verifiable target: one line
/// `gitdir: <path>` with at most one trailing newline, resolving relative
/// to `dir` to a directory under a `.git`, where worktrees
/// and submodules live. Anything else is `executes`.
fn gitfile_ok(dir: &Path) -> bool {
    let Ok(file) = std::fs::File::open(dir.join(".git")) else {
        return false;
    };
    let mut bytes = Vec::new();
    let Ok(_) = file
        .take(MAX_GITFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
    else {
        return false;
    };
    if bytes.len() > MAX_GITFILE_BYTES {
        return false;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
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
    let joined = dir.join(target);
    let Ok(canonical) = joined.canonicalize() else {
        return false;
    };
    if !canonical.is_dir() {
        return false;
    }
    canonical
        .components()
        .any(|component| component.as_os_str() == ".git")
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
