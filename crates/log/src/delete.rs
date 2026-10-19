//! Removes a session directory (`docs/invocation.md`, "Deleting and
//! pruning"). Only `log` removes the session directory, holds its lock,
//! or mints `seq` (`docs/architecture.md`).

use std::path::Path;

/// Whether `dir` is a plain directory: `symlink_metadata` succeeds and is
/// a directory. A link to a directory is not plain, so the hub's lookup
/// refuses it before taking a lock.
pub fn is_plain_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir())
}

#[cfg(test)]
#[path = "delete_tests.rs"]
mod tests;
