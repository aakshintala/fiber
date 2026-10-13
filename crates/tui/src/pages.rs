//! The conversation as pages (`docs/tui.md`, "History and paging"): the
//! page index that cuts the session's durable lines into pages, counts
//! their rows and finds the window of pages kept on screen.

use std::cell::OnceCell;
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
    /// One revision per page, pushed with it: a page's revision only
    /// grows, so a search rescans a page whose text moved (`docs/tui.md`,
    /// "History and paging"). Geometry alone never moves it: only an
    /// explicit [`Index::invalidate`] does, so wrapping at a new width
    /// keeps the revision of every page whose text the width did not
    /// change.
    revisions: Vec<u64>,
    /// How many times any page's text revision moved: the search
    /// remembers the clock it last verified and re-examines pages only
    /// when it moved, instead of walking every page per answer.
    clock: u64,
    /// The saturated row sums before each page, rebuilt lazily after
    /// page or row changes: `prefix[at]` is the rows of the pages
    /// before `at`, so row queries never walk the pages. Empty after a
    /// change until the next query rebuilds it; the rebuild visits
    /// every page once and counts those visits, while queries read the
    /// cache and visit no page.
    prefix: OnceCell<Vec<usize>>,
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
            revisions: vec![0],
            clock: 0,
            prefix: OnceCell::from(vec![0, 0]),
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
                index
                    .in_flight
                    .insert((flight, action.clone()), index.turns);
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
                    self.revisions.push(0);
                    self.prefix = OnceCell::new();
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
        self.revisions.push(0);
        self.prefix = OnceCell::new();
    }

    /// The pages, the open one last.
    pub(crate) fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// Sets page `at`'s row count, for geometry alone: wrapping at a new
    /// width, reloading, or an edge coming off moves no revision, so the
    /// search keeps what it scanned. A page's text changing calls
    /// [`Index::invalidate`] instead. A missing page stays harmless.
    pub(crate) fn set_rows(&mut self, at: usize, rows: usize) {
        let changed = self.pages.get(at).is_some_and(|page| page.rows != rows);
        if let Some(page) = self.pages.get_mut(at) {
            page.rows = rows;
        }
        if changed {
            self.prefix = OnceCell::new();
        }
    }

    /// Page `at`'s text changed for the search: its revision grows, so
    /// the search scans it again. Callers name the change: a folded
    /// line, a moved ▣ line, a closed page's rearranged cards, or a
    /// table page's new width. A missing page stays harmless.
    pub(crate) fn invalidate(&mut self, at: usize) {
        if let Some(revision) = self.revisions.get_mut(at) {
            *revision = revision.saturating_add(1);
            self.clock = self.clock.saturating_add(1);
        }
    }

    /// How many times any page's text revision moved.
    pub(crate) fn clock(&self) -> u64 {
        self.clock
    }

    /// The cached saturated row sums before each page, rebuilt once
    /// after accumulated page or row changes: `prefix[at]` is the rows
    /// of the pages before `at`. The rebuild visits every page once and
    /// counts those visits; queries read the cache and visit no page.
    fn prefixes(&self) -> &Vec<usize> {
        self.prefix.get_or_init(|| {
            let mut prefix = Vec::with_capacity(self.pages.len().saturating_add(1));
            let mut sum = 0usize;
            prefix.push(0);
            for page in &self.pages {
                // Saturation stays in the running sum: subtracting from
                // an already saturated total would undercount every page
                // after the overflow.
                sum = sum.saturating_add(page.rows);
                prefix.push(sum);
            }
            crate::work::add(|work| work.index_pages += self.pages.len());
            prefix
        })
    }

    /// Page `at`'s revision, 0 for a page never counted.
    pub(crate) fn revision(&self, at: usize) -> u64 {
        self.revisions.get(at).copied().unwrap_or(0)
    }

    /// The page holding `seq`, if any does.
    pub(crate) fn page_of(&self, seq: Seq) -> Option<usize> {
        let at = self.pages.partition_point(|page| page.last_seq < seq);
        self.pages
            .get(at)
            .filter(|page| page.lines > 0 && page.first_seq <= seq && seq <= page.last_seq)
            .map(|_| at)
    }

    /// Every row the conversation draws, from the cached prefixes.
    pub(crate) fn total(&self) -> usize {
        let prefixes = self.prefixes();
        prefixes.last().copied().unwrap_or(0)
    }

    /// Page `at`'s first row: the rows of the pages before it, from the
    /// cached prefixes.
    pub(crate) fn start(&self, at: usize) -> usize {
        let prefixes = self.prefixes();
        prefixes
            .get(at.min(prefixes.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    }

    /// The page drawing conversation row `row`, and the row's offset in it;
    /// `None` past the last row. A page drawing no row holds none.
    pub(crate) fn locate(&self, row: usize) -> Option<(usize, usize)> {
        let prefixes = self.prefixes();
        let pages = prefixes.len().saturating_sub(1);
        // The first prefix past `row`, less one: the page whose rows
        // hold it, skipping pages that draw no row.
        let at = prefixes
            .partition_point(|sum| *sum <= row)
            .saturating_sub(1);
        if at >= pages {
            return None;
        }
        // `at` holds `row`: `partition_point` put the first prefix
        // past it one further on, so its end is past `row` and no
        // guard remains to mutate.
        let start = prefixes.get(at).copied().unwrap_or(0);
        Some((at, row.saturating_sub(start)))
    }

    /// Each page's first row.
    #[cfg(test)]
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
    #[cfg(test)]
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
        let prefixes = self.prefixes();
        let from = top.saturating_sub(height);
        let to = top
            .saturating_add(height.saturating_mul(2))
            .min(prefixes.last().copied().unwrap_or(0));
        // The first page ending past `from`, and the first page starting
        // at or past `to`: binary searches over the cached saturated
        // prefixes, so a query reads the cache and visits no page.
        // `prefixes[0]` is 0, never past `from`, so the first search
        // always lands past it and the page holds the row after.
        let first = prefixes
            .partition_point(|end| *end <= from)
            .saturating_sub(1);
        let last = prefixes.partition_point(|start| *start < to);
        first..last.max(first)
    }

    /// How many lines the open page holds. The index always has one.
    fn held(&self) -> usize {
        self.pages.last().map_or(0, |page| page.lines)
    }

    /// Whether a candidate cut waits for its step's first content.
    pub(crate) fn pending(&self) -> bool {
        self.candidate.is_some()
    }
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod tests;
