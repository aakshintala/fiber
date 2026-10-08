//! The pages kept resident past the window, counted per page
//! (`docs/tui.md`, "History and paging"): a whole-turn copy and a
//! selection's copy each pin the dropped pages they wait on, and neither
//! can drop the other's. A page drops with the window only once no copy
//! pins it.

use std::collections::BTreeMap;

/// How many pending copies pin each page. No entry holds a count of 0.
#[derive(Debug, Default)]
pub(crate) struct Pins {
    counts: BTreeMap<usize, usize>,
}

impl Pins {
    /// One more copy keeps page `at` resident.
    pub(crate) fn pin(&mut self, at: usize) {
        let count = self.counts.entry(at).or_default();
        *count = count.saturating_add(1);
    }

    /// One copy lets page `at` go; a page no copy pinned stays unpinned.
    pub(crate) fn unpin(&mut self, at: usize) {
        if let Some(count) = self.counts.get_mut(&at) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.counts.remove(&at);
            }
        }
    }

    /// Whether any copy pins page `at`.
    pub(crate) fn contains(&self, at: usize) -> bool {
        self.counts.contains_key(&at)
    }

    /// The pinned pages, in order.
    pub(crate) fn pages(&self) -> impl Iterator<Item = usize> + '_ {
        self.counts.keys().copied()
    }

    /// Every pin held, summed over the pages.
    #[cfg(test)]
    pub(crate) fn total(&self) -> usize {
        self.counts.values().sum()
    }
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
