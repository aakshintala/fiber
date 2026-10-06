//! Fiber's per-path lock (`docs/architecture.md`, "Tool calls in a step"),
//! offered to extensions. Seam definitions only: no behaviour.

use std::path::Path;

/// The per-path lock the file tools take, offered to an extension's tool.
/// Waiting is not cancelled: the holder's write is bounded.
pub trait PathLock: Send + Sync {
    /// Runs `run` while holding the lock on `path`, blocking first while
    /// another holder has it. `path` is absolute; the implementation
    /// derives its key as the file tools do.
    fn hold(&self, path: &Path, run: &mut dyn FnMut());
}
