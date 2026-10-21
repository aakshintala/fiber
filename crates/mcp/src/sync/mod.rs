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

/// One sync's startup inputs, as at session start: the private stage and
/// commit helpers thread this instead of seven arguments each. The public
/// stage and commit entries keep their flat shapes, which `main` drives.
pub(super) struct SyncInputs<'a> {
    /// The session workspace the servers run in.
    pub(super) workspace: &'a Path,
    /// The artifacts directory binary content declares through.
    pub(super) artifacts: &'a Path,
    /// The image child shadow starts load through.
    pub(super) images: &'a Arc<dyn Images>,
    /// The cache directory session starts write through.
    pub(super) cache: &'a Path,
    /// The clock every bound reads.
    pub(super) clock: &'a Arc<dyn Clock>,
    /// The session version shadow slots carry.
    pub(super) version: &'a str,
}
