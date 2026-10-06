//! Fiber's per-path lock (`docs/architecture.md`, "Tool calls in a step"),
//! offered to extensions: the seam every holder shares.

use std::path::{Path, PathBuf};

/// The per-path lock the file tools take, offered to an extension's tool.
/// Waiting is not cancelled: the holder's write is bounded.
pub trait PathLock: Send + Sync {
    /// Runs `run` while holding the lock on `path`, blocking first while
    /// another holder has it. `path` is absolute; the implementation
    /// derives its key as the file tools do.
    fn hold(&self, path: &Path, run: &mut dyn FnMut());

    /// Runs `run` while holding the lock on every path in `paths`: the
    /// keys sorted with duplicates dropped, each held inside the last,
    /// so two renames never deadlock. An implementation that derives
    /// keys resolves each path first, then sorts and deduplicates.
    fn hold_all(&self, paths: &[PathBuf], run: &mut dyn FnMut());
}
