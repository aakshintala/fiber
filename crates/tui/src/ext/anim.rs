//! One animated item's identity and the shared tick floor (`docs/tui.md`,
//! "Animation").
//!
//! A renderer declares its interval once at registration; the adapter
//! normalizes it through [`effective_interval`], and the seam uses the
//! same helper at its due checks and wake asks, so a raw value never
//! reaches the schedule.

use std::time::Duration;

use super::cache::PageKey;
use crate::tick::TICK;

/// One animated item: its page, slot, input, extension and half, but no
/// width. A width change keeps the frame counter, so the key leaves the
/// width out and a new frame evicts every older entry at every width.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AnimKey {
    page: PageKey,
    slot: String,
    input: u64,
    extension: String,
    half: Option<String>,
}

impl AnimKey {
    /// The animated item `extension` draws in `slot` on `page` for the
    /// input keyed `input` under `half`.
    pub(crate) fn new(
        page: PageKey,
        slot: &str,
        input: u64,
        extension: &str,
        half: Option<String>,
    ) -> Self {
        Self {
            page,
            slot: slot.to_owned(),
            input,
            extension: extension.to_owned(),
            half,
        }
    }

    /// The page the item draws on: page drops clear its counter.
    pub(crate) fn page(&self) -> &PageKey {
        &self.page
    }
}

/// The interval the schedule uses for a declared `raw` one: a value below
/// [`TICK`] fires at the tick rate instead (`docs/tui.md`, "Animation").
/// Idempotent, so registration, due checks and wake asks share it.
pub fn effective_interval(raw: Duration) -> Duration {
    raw.max(TICK)
}

#[cfg(test)]
#[path = "anim_tests.rs"]
mod tests;
