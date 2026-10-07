//! The conversation as pages (`docs/tui.md`, "History and paging"): the
//! page index that cuts the session's durable lines into pages, counts
//! their rows and finds the window of pages kept on screen.

use std::collections::HashSet;
use std::ops::Range;

/// About how many durable lines a page holds before it may be cut.
pub(crate) const PAGE_LINES: usize = 64;

/// One page: a contiguous run of durable lines by `seq`, and the rows they
/// draw at the current width. A page with no lines yet has no seq range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Page {
    /// The first line's `seq`.
    pub(crate) first_seq: u64,
    /// The last line's `seq`.
    pub(crate) last_seq: u64,
    /// How many durable lines it holds.
    pub(crate) lines: usize,
    /// How many rows it draws.
    pub(crate) rows: usize,
}

/// What one durable line did to the pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cut {
    /// Nothing: the line joins the open page.
    None,
    /// The line is a `step_started` that may begin a page; what is folded
    /// from here on moves to the new page if the step opens with text.
    Candidate,
    /// The step opened with text: a new page begins at the candidate, and
    /// this line joins it.
    AtCandidate,
    /// A new page begins at this line.
    Here,
}

/// The candidate cut: its `seq`, and how many lines the open page held
/// before it.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    seq: u64,
    held: usize,
}

/// The page index. Pages are contiguous in `seq` and never overlap; every
/// durable line pushed belongs to exactly one, and the last is open.
#[derive(Debug)]
pub(crate) struct Index {
    pages: Vec<Page>,
    /// Tool calls requested and not yet completed, by action id.
    in_flight: HashSet<String>,
    candidate: Option<Candidate>,
}

impl Default for Index {
    fn default() -> Self {
        Self {
            pages: vec![Page::default()],
            in_flight: HashSet::new(),
            candidate: None,
        }
    }
}

impl Index {
    /// Adds the durable line `seq` of `kind`, cutting a page where the rule
    /// allows: at a `step_started` once the open page holds
    /// [`PAGE_LINES`] lines and no tool call is in flight, confirmed when
    /// the step's first content is text (`text_completed`) and discarded
    /// when it is a tool call, which continues the open tool group; and at
    /// a `turn_started` once the open page holds [`PAGE_LINES`] lines.
    /// `assistant_message_started` opens every model call, a tool call's
    /// too, so it is not content.
    pub(crate) fn push(&mut self, seq: u64, kind: &str, action: Option<&str>) -> Cut {
        let held = self.held();
        let mut cut = Cut::None;
        match kind {
            "turn_started" => {
                self.in_flight.clear();
                self.candidate = None;
                if held >= PAGE_LINES {
                    self.pages.push(Page::default());
                    cut = Cut::Here;
                }
            }
            "turn_completed" => {
                self.in_flight.clear();
                self.candidate = None;
            }
            "step_started" => {
                self.candidate = None;
                if held >= PAGE_LINES && self.in_flight.is_empty() {
                    self.candidate = Some(Candidate { seq, held });
                    cut = Cut::Candidate;
                }
            }
            "tool_call_requested" => {
                self.candidate = None;
                if let Some(action) = action {
                    self.in_flight.insert(action.to_owned());
                }
            }
            "tool_call_completed" => {
                if let Some(action) = action {
                    self.in_flight.remove(action);
                }
            }
            "text_completed" => {
                if let Some(candidate) = self.candidate.take() {
                    self.split(candidate);
                    cut = Cut::AtCandidate;
                }
            }
            _ => {}
        }
        if let Some(open) = self.pages.last_mut() {
            if open.lines == 0 {
                open.first_seq = seq;
            }
            open.last_seq = seq;
            open.lines = open.lines.saturating_add(1);
        }
        cut
    }

    /// Moves the lines from `candidate` on into a new page.
    fn split(&mut self, candidate: Candidate) {
        let Some(open) = self.pages.last_mut() else {
            return;
        };
        let moved = open.lines.saturating_sub(candidate.held);
        let last_seq = open.last_seq;
        open.lines = candidate.held;
        open.last_seq = candidate.seq.saturating_sub(1);
        self.pages.push(Page {
            first_seq: candidate.seq,
            last_seq,
            lines: moved,
            rows: 0,
        });
    }

    /// The pages, the open one last.
    pub(crate) fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// Sets page `at`'s row count.
    pub(crate) fn set_rows(&mut self, at: usize, rows: usize) {
        if let Some(page) = self.pages.get_mut(at) {
            page.rows = rows;
        }
    }

    /// The page holding `seq`, if any does.
    pub(crate) fn page_of(&self, seq: u64) -> Option<usize> {
        let at = self.pages.partition_point(|page| page.last_seq < seq);
        self.pages
            .get(at)
            .filter(|page| page.lines > 0 && page.first_seq <= seq && seq <= page.last_seq)
            .map(|_| at)
    }

    /// Every row the conversation draws.
    pub(crate) fn total(&self) -> usize {
        self.pages
            .iter()
            .fold(0usize, |sum, page| sum.saturating_add(page.rows))
    }

    /// Each page's first row.
    pub(crate) fn starts(&self) -> Vec<usize> {
        let mut start = 0usize;
        self.pages
            .iter()
            .map(|page| {
                let at = start;
                start = start.saturating_add(page.rows);
                at
            })
            .collect()
    }

    /// Whether page `at` draws a row in `[from, to)`.
    pub(crate) fn intersects(&self, at: usize, from: usize, to: usize) -> bool {
        let start: usize = self
            .pages
            .iter()
            .take(at)
            .fold(0usize, |sum, page| sum.saturating_add(page.rows));
        let rows = self.pages.get(at).map_or(0, |page| page.rows);
        rows > 0 && start < to && start.saturating_add(rows) > from
    }

    /// The window's pages, as a range of page indices: those drawing a row
    /// in `[top - height, top + 2 * height)`, clamped to the conversation.
    /// A page in the range that draws no row is in no window.
    pub(crate) fn window(&self, top: usize, height: usize) -> Range<usize> {
        let from = top.saturating_sub(height);
        let to = top
            .saturating_add(height.saturating_mul(2))
            .min(self.total());
        let mut first = self.pages.len();
        let mut last = self.pages.len();
        let mut start = 0usize;
        for (at, page) in self.pages.iter().enumerate() {
            let end = start.saturating_add(page.rows);
            if first == self.pages.len() && end > from {
                first = at;
            }
            if start >= to {
                last = at;
                break;
            }
            start = end;
        }
        first..last.max(first)
    }

    /// How many lines the open page holds. The index always has one.
    fn held(&self) -> usize {
        self.pages.last().map_or(0, |page| page.lines)
    }
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod tests;
