//! Fiber's per-path lock (`docs/architecture.md`, "Tool calls in a step"),
//! offered to extensions: the seam, and the one combination every holder
//! shares.

use std::path::{Path, PathBuf};

/// The per-path lock the file tools take, offered to an extension's tool.
/// Waiting is not cancelled: the holder's write is bounded.
pub trait PathLock: Send + Sync {
    /// Runs `run` while holding the lock on `path`, blocking first while
    /// another holder has it. `path` is absolute; the implementation
    /// derives its key as the file tools do.
    fn hold(&self, path: &Path, run: &mut dyn FnMut());

    /// Runs `run` while holding the lock on every path in `paths`. The
    /// default sorts the keys and drops duplicates, then nests one
    /// [`hold`](Self::hold) per key, so two renames never deadlock; an
    /// implementation that derives keys resolves each path first, then
    /// sorts and deduplicates.
    fn hold_all(&self, paths: &[PathBuf], run: &mut dyn FnMut()) {
        let mut keys = paths.to_vec();
        keys.sort();
        keys.dedup();
        nest(self, &keys, run);
    }
}

/// Runs `run` while holding the lock on each of `keys`: the first key is
/// held, then the rest inside it, so two renames never deadlock.
fn nest(lock: &(impl PathLock + ?Sized), keys: &[PathBuf], run: &mut dyn FnMut()) {
    match keys.split_first() {
        None => run(),
        Some((first, rest)) => {
            let mut next = || nest(lock, rest, run);
            lock.hold(first, &mut next);
        }
    }
}
