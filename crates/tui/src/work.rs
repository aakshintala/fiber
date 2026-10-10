//! How much work folding and drawing did, counted on the calling thread:
//! markdown renders, row builds, page counts and each pass `on_line`
//! runs, with the items the passes walk. The `paging` jig prints them
//! beside its fold time, and tests assert them: a test of how much work
//! something does counts the work (`docs/testing.md`). Each count is one
//! thread-local add, nothing beside the work it counts.

use std::cell::Cell;

/// The work counted since the last [`take`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Work {
    /// Lines folded by `App::on_line`.
    pub lines: usize,
    /// Markdown renders: `markdown::render` calls.
    pub markdown_renders: usize,
    /// Reply renders asked for, each a render or a cached one.
    pub reply_renders: usize,
    /// Turn cards drawn into rows: `Turn::rows` calls.
    pub turn_rows: usize,
    /// Pages whose lines were built: `Pages::draw_data` calls.
    pub page_builds: usize,
    /// Pages whose rows were counted: `Pages::count` calls.
    pub page_counts: usize,
    /// Lines measured for their wrapped rows: `view::rows` calls.
    pub line_measures: usize,
    /// `home_outgoing` passes.
    pub home_outgoing: usize,
    /// `sessions_outgoing` passes.
    pub sessions_outgoing: usize,
    /// `find_outgoing` passes.
    pub find_outgoing: usize,
    /// `reconcile_attention` passes.
    pub reconcile_attention: usize,
    /// `App::settle` passes.
    pub settle: usize,
    /// Closed pages visited by `Pages::trim`.
    pub trim_pages: usize,
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

impl std::ops::AddAssign for Work {
    fn add_assign(&mut self, other: Self) {
        self.lines += other.lines;
        self.markdown_renders += other.markdown_renders;
        self.reply_renders += other.reply_renders;
        self.turn_rows += other.turn_rows;
        self.page_builds += other.page_builds;
        self.page_counts += other.page_counts;
        self.line_measures += other.line_measures;
        self.home_outgoing += other.home_outgoing;
        self.sessions_outgoing += other.sessions_outgoing;
        self.find_outgoing += other.find_outgoing;
        self.reconcile_attention += other.reconcile_attention;
        self.settle += other.settle;
        self.trim_pages += other.trim_pages;
        self.index_pages += other.index_pages;
        self.usage_summaries += other.usage_summaries;
    }
}

impl Work {
    /// Each count as a name and its number, in field order.
    pub fn named(&self) -> [(&'static str, usize); 15] {
        [
            ("lines", self.lines),
            ("markdown_renders", self.markdown_renders),
            ("reply_renders", self.reply_renders),
            ("turn_rows", self.turn_rows),
            ("page_builds", self.page_builds),
            ("page_counts", self.page_counts),
            ("line_measures", self.line_measures),
            ("home_outgoing", self.home_outgoing),
            ("sessions_outgoing", self.sessions_outgoing),
            ("find_outgoing", self.find_outgoing),
            ("reconcile_attention", self.reconcile_attention),
            ("settle", self.settle),
            ("trim_pages", self.trim_pages),
            ("index_pages", self.index_pages),
            ("usage_summaries", self.usage_summaries),
        ]
    }
}
