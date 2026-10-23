//! How much work folding and drawing did, counted on the calling thread:
//! reply renders, row builds and page counts, with the index's page
//! walks, the usage lines' turn lookups, the prompt recall lists built
//! and the workspace paths lowercased for file search. The `paging` jig
//! prints the paging counts (not the recall or path counts) beside its
//! fold time, and tests assert every count: a test of how much work
//! something does counts the work (`docs/testing.md`). Each
//! count is one thread-local add, nothing beside the work it counts, and
//! every count has a test asserting its exact number, so no count
//! survives that no test can tell apart.

use std::cell::Cell;

/// The work counted since the last [`take`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Work {
    /// Reply renders asked for, each a render or a cached one.
    pub reply_renders: usize,
    /// Turn cards drawn into rows: `Turn::rows` calls.
    pub turn_rows: usize,
    /// Pages whose rows were counted: `Pages::count` calls.
    pub page_counts: usize,
    /// Pages visited by the index's walks over every page: `total`,
    /// `start` and `window`.
    pub index_pages: usize,
    /// Turn summaries looked up to place a `usage_recorded` line.
    pub usage_summaries: usize,
    /// Drawn pieces measured for their width in the text helpers:
    /// one per grapheme or control character `cut` or `wrap_joined`
    /// sized, including a cut's first rejected piece.
    pub grapheme_widths: usize,
    /// Pages whose text the search scanned: resident scans and scratch
    /// folds of dropped pages, one per page scan.
    pub search_pages: usize,
    /// Page entries the search's order and progress bookkeeping
    /// examined: order extensions, scan-progress checks and completion
    /// walks, one per entry examined.
    pub search_order_pages: usize,
    /// Shell candidates examined while placing shell output for a draw.
    pub shell_visits: usize,
    /// Renderer calls: one per cache miss that called the renderer.
    pub renderer_calls: usize,
    /// Prompt recall lists built: one per rebuild of the deduplicated list
    /// and its lowercase keys.
    pub recall_builds: usize,
    /// Workspace listing paths lowercased for file search: one per path,
    /// when the listing arrives.
    pub path_lowercases: usize,
}

thread_local! {
    static WORK: Cell<Work> = Cell::new(Work::default());
}

/// Adds to this thread's counts.
pub(crate) fn add(count: impl FnOnce(&mut Work)) {
    WORK.with(|work| {
        let mut now = work.get();
        count(&mut now);
        work.set(now);
    });
}

/// This thread's counts since the last call, which start again from zero.
pub fn take() -> Work {
    WORK.with(|work| work.replace(Work::default()))
}

#[cfg(test)]
#[path = "work_tests.rs"]
mod tests;
