//! Old diagnostic logs and crash files (`docs/state.md`, "Bounds"): the
//! files `fiber sessions prune` deletes, as the hub does when it starts.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Files in `logs/` and `crashes/` older than this are deleted.
const OLD_AFTER: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);

/// Besides the old files, each directory keeps this many of its newest.
const KEEP: usize = 100;

/// A diagnostic file to delete: its path and its length.
pub(crate) struct OldFile {
    /// The file's path.
    pub(crate) path: PathBuf,
    /// The file's length.
    pub(crate) bytes: u64,
}

/// Every file in `dir` prune deletes: the old ones, then all but the
/// newest 100 by mtime, sorted by path. A missing directory holds nothing
/// to delete, and a symlink is never selected.
pub(crate) fn old_diagnostics(dir: &Path, now: SystemTime) -> Vec<OldFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut old = Vec::new();
    let mut kept: Vec<(SystemTime, PathBuf, u64)> = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let (len, at, is_old) = match entry.metadata() {
            Ok(meta) => {
                let old = meta.modified().is_ok_and(|at| at + OLD_AFTER < now);
                let at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                (meta.len(), at, old)
            }
            Err(_) => (0, SystemTime::UNIX_EPOCH, false),
        };
        if is_old {
            old.push(OldFile { path, bytes: len });
        } else {
            kept.push((at, path, len));
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let drop = kept.len().saturating_sub(KEEP);
    for (_, path, bytes) in kept.into_iter().take(drop) {
        old.push(OldFile { path, bytes });
    }
    old.sort_by(|a, b| a.path.cmp(&b.path));
    old
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
