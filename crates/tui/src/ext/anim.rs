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
/// width out and a new frame evicts every older entry at every width. A
/// notice key also carries the notice instance, never its text: the same
/// code and text raised again is a new item with its own counter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AnimKey {
    page: PageKey,
    slot: String,
    input: u64,
    extension: String,
    half: Option<String>,
    notice_id: Option<usize>,
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
            notice_id: None,
        }
    }

    /// The animated notice `id`: the same page, slot, input, extension
    /// and half as an item key, qualified by the notice instance, so two
    /// notices with the same code and text advance independently.
    pub(crate) fn for_notice(
        page: PageKey,
        slot: &str,
        input: u64,
        extension: &str,
        half: Option<String>,
        id: usize,
    ) -> Self {
        Self {
            page,
            slot: slot.to_owned(),
            input,
            extension: extension.to_owned(),
            half,
            notice_id: Some(id),
        }
    }

    /// The page the item draws on: page drops clear its counter.
    pub(crate) fn page(&self) -> &PageKey {
        &self.page
    }

    /// The notice instance the key animates, if any: items carry none.
    pub(crate) fn notice_id(&self) -> Option<usize> {
        self.notice_id
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
