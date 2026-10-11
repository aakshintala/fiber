//! How much work folding and drawing did, counted on the calling thread:
//! reply renders, row builds and page counts, with the index's page
//! walks and the usage lines' turn lookups. The `paging` jig prints
//! them beside its fold time, and tests assert them: a test of how much
//! work something does counts the work (`docs/testing.md`). Each count
//! is one thread-local add, nothing beside the work it counts, and every
//! count has a test asserting its exact number, so no count survives
//! that no test can tell apart.

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
