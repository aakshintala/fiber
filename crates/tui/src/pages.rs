//! The conversation as pages (`docs/tui.md`, "History and paging"): the
//! page index that cuts the session's durable lines into pages, counts
//! their rows and finds the window of pages kept on screen.

use std::collections::BTreeMap;
use std::ops::Range;

use contract::{ActionId, Seq};

/// About how many durable lines a page holds before it may be cut.
pub(crate) const PAGE_LINES: usize = 64;

/// One page: a contiguous run of durable lines by `seq`, and the rows they
/// draw at the current width. A page with no lines yet has no seq range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Page {
    /// The first line's `seq`.
    pub(crate) first_seq: Seq,
    /// The last line's `seq`.
    pub(crate) last_seq: Seq,
    /// How many durable lines it holds.
    pub(crate) lines: usize,
    /// How many rows it draws.
    pub(crate) rows: usize,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            first_seq: Seq(0),
            last_seq: Seq(0),
            lines: 0,
            rows: 0,
        }
    }
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
    seq: Seq,
    held: usize,
}

/// An action whose completion folds back into the item it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Flight {
    /// `tool_call_requested` until `tool_call_completed`.
    Call,
    /// `reasoning_started` until `reasoning_completed`.
    Reasoning,
    /// `permission_requested` until `permission_resolved`.
    Permission,
}

/// The page index. Pages are contiguous in `seq` and never overlap; every
/// durable line pushed belongs to exactly one, and the last is open.
#[derive(Debug)]
pub(crate) struct Index {
    pages: Vec<Page>,
    /// Actions in flight, with the turn each began in.
    in_flight: BTreeMap<(Flight, ActionId), u64>,
    /// How many turns have started.
    turns: u64,
    /// Whether a tool group is open: a call or thinking shown since the
    /// last text.
    group: bool,
    candidate: Option<Candidate>,
}

impl Default for Index {
    fn default() -> Self {
        Self {
            pages: vec![Page::default()],
            in_flight: BTreeMap::new(),
            turns: 0,
            group: false,
            candidate: None,
        }
    }
}

impl Index {
    /// Adds the durable line `seq` of `kind`, `shown` when it changed a
    /// card, cutting a page where the rule allows while nothing is in
    /// flight: at a `step_started` once the open page holds [`PAGE_LINES`]
    /// lines, confirmed when the step's first content is text shown
    /// (`text_completed`), and discarded when it is a tool call or thinking
    /// that joins an open tool group; and at a `turn_started` once the open
    /// page holds [`PAGE_LINES`] lines. `assistant_message_started` opens
    /// every model call, a tool call's too, so it is not content.
    ///
    /// In flight is every action whose completion folds back into an
    /// earlier item, kept across turn ends because completions may follow
    /// them, so every line that changes an item lies on the item's page.
    /// One begun two turns back is dropped: a cancelled or crashed call
    /// never completes.
    pub(crate) fn push(
        &mut self,
        seq: Seq,
        kind: &str,
        action: Option<&ActionId>,
        shown: bool,
    ) -> Cut {
        let held = self.held();
        let mut cut = Cut::None;
        let begin = |flight: Flight, index: &mut Self| {
            if let Some(action) = action {
                index.in_flight.insert((flight, action.clone()), index.turns);
            }
        };
        match kind {
            "turn_started" => {
                self.turns = self.turns.saturating_add(1);
                let keep = self.turns.saturating_sub(1);
                self.in_flight.retain(|_, turn| *turn >= keep);
                self.candidate = None;
                self.group = false;
                if held >= PAGE_LINES && self.in_flight.is_empty() {
                    self.pages.push(Page::default());
                    cut = Cut::Here;
                }
            }
            "turn_completed" => self.candidate = None,
            "step_started" => {
                self.candidate = None;
                if held >= PAGE_LINES && self.in_flight.is_empty() {
                    self.candidate = Some(Candidate { seq, held });
                    cut = Cut::Candidate;
                }
            }
            "tool_call_requested" => {
                self.candidate = None;
                self.group |= shown;
                begin(Flight::Call, self);
            }
            "reasoning_started" => {
                if self.group {
                    self.candidate = None;
                }
                self.group |= shown;
                begin(Flight::Reasoning, self);
            }
            "permission_requested" => begin(Flight::Permission, self),
            "tool_call_completed" | "reasoning_completed" | "permission_resolved" => {
                let flight = match kind {
                    "tool_call_completed" => Flight::Call,
                    "reasoning_completed" => Flight::Reasoning,
                    _ => Flight::Permission,
                };
                if let Some(action) = action {
                    self.in_flight.remove(&(flight, action.clone()));
                }
            }
            "text_completed" if shown => {
                self.group = false;
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
        open.last_seq = Seq(candidate.seq.0.saturating_sub(1));
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
    pub(crate) fn page_of(&self, seq: Seq) -> Option<usize> {
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
