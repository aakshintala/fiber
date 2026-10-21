//! The reload's server sync (`docs/mcp.md`, "Reload"): classify each new
//! spec against the live slots, stage (re-list keeps, shadow-start
//! replacements and starts), then commit (stop and remove old slots,
//! install the new ones, write caches, declare). `main` prepare drives
//! this through the crate root and never touches slots directly.

mod commit;
mod rows;
mod stage;

use std::path::Path;
use std::sync::Arc;

use contract::clock::Clock;
use contract::images::Images;

pub use commit::{Synced, commit_sync};
pub use rows::{SyncRow, classify_sync};
pub use stage::{StagedSync, stage_sync};

/// One sync's startup inputs, as at session start: the public stage and
/// commit entries thread this instead of one argument each, and `main`
/// builds it at the call site.
pub struct SyncInputs<'a> {
    /// The session workspace the servers run in.
    pub workspace: &'a Path,
    /// The artifacts directory binary content declares through.
    pub artifacts: &'a Path,
    /// The image child shadow starts load through.
    pub images: &'a Arc<dyn Images>,
    /// The cache directory session starts write through.
    pub cache: &'a Path,
    /// The clock every bound reads.
    pub clock: &'a Arc<dyn Clock>,
    /// The session version shadow slots carry.
    pub version: &'a str,
}
