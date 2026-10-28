//! The window of history (`docs/tui.md`, "History and paging"): the cards
//! folded from the pages in the window, over the page index. No event is
//! kept: each is folded into its page's cards and dropped, and a page
//! outside the window keeps only its seq range and its counts. Each turn's
//! totals, which its ▣ line draws, are kept for the whole session.

use std::collections::{BTreeMap, HashMap};
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

use contract::events::{TurnCompleted, TurnOutcome, UsageRecorded};
use contract::{Envelope, GenerationId, Seq, SessionId};
use jiff::tz::TimeZone;
use ratatui::text::Line;

use crate::app::{Target, read};
use crate::format::{self, Spend};
use crate::mouse::TargetId;
use crate::pages::{Cut, Index};
use crate::rows::{RowText, Rows};
use crate::surface::Edges;
use crate::turn::{DrawSummary, Entry, Fold, Row, Turn, target_id};

mod images;
mod live;
mod pins;
mod residency;

/// Where folding a page begins: its turn and step, the live fold's
/// continuation state, and descriptions only of earlier pages' jobs that
/// complete orphaned on it. Rendered asides stay in resident pages.
#[derive(Debug, Clone, Default)]
pub(crate) struct Seed {
    /// The summary of the page's first card.
    first: usize,
    /// The step its turn had reached, when the page begins inside a running
    /// turn.
    step: Option<u64>,
    /// The next page begins inside the same turn, with the text that ends
    /// this page's last group.
    cut: bool,
    /// The page's last card piece goes on as the next page's first: it
    /// draws no bottom edge (`docs/tui.md`, "History and paging").
    joins_next: bool,
    /// The page's first card piece goes on from the page before: it draws
    /// no top edge (`docs/tui.md`, "History and paging").
    joins_previous: bool,
    /// The next target id.
    next: usize,
    /// Whether a new group starts with its ledger open.
    ledgers: bool,
    /// The context size an automatic handoff runs at.
    trigger_at: Option<u64>,
    /// The last `fiber_exited` carried `suspended_on`.
    suspended: bool,
    /// Descriptions of jobs that ended stopped or orphaned on this
    /// page, set once the page closes.
    carried: HashMap<String, String>,
    /// The live durations of the page's groups, in order: a reload folds
    /// the page's durable lines only, without the ephemeral lines that
    /// timed them.
    spans: Vec<(u64, u64)>,
}

impl Seed {
    /// The seed a page opening on `open`'s cards starts from: its turn and
    /// step, and the fold's continuation state.
    fn live(first: usize, step: Option<u64>, open: &Part) -> Self {
        let (next, ledgers, trigger_at, suspended) = open.fold.seed_continuation();
        Self {
            first,
            step,
            cut: false,
            joins_next: false,
            joins_previous: false,
            next,
            ledgers,
            trigger_at,
            suspended,
            carried: HashMap::new(),
            spans: Vec::new(),
        }
    }
}

/// One page's cards: the turns from summary `first` on.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    first: usize,
    turns: Vec<Turn>,
    fold: Fold,
    aside_start: usize,
    /// The page's last card piece goes on as the next page's first: it
    /// draws no bottom edge (`docs/tui.md`, "History and paging").
    joins_next: bool,
    /// The page's first card piece goes on from the page before: it draws
    /// no top edge (`docs/tui.md`, "History and paging").
    joins_previous: bool,
}

impl Part {
    /// The cards a page seeded with `seed` begins with.
    fn seeded(seed: Seed) -> Self {
        Self {
            first: seed.first,
            turns: seed.step.map(Turn::part).into_iter().collect(),
            fold: Fold::seeded(
                seed.next,
                seed.ledgers,
                seed.trigger_at,
                seed.suspended,
                seed.carried,
            ),
            aside_start: 0,
            joins_next: false,
            joins_previous: seed.joins_previous,
        }
    }
}

fn call_paths(part: &Part) -> CallPaths {
    part.turns
        .iter()
        .flat_map(Turn::groups)
        .flat_map(|group| group.calls())
        .filter(|call| matches!(call.name.as_str(), "read" | "edit" | "write"))
        .filter_map(|call| {
            let path = call.arguments.get("path")?.as_str()?;
            Some((Target::Call(call.id), path.to_owned()))
        })
        .collect()
}

/// One turn's totals for its ▣ line, kept for the whole session.
#[derive(Debug, Clone, Default)]
struct Summary {
    started: u64,
    step: u64,
    calls: u64,
    /// Its usage, late lines and delegates' copies included.
    spend: Spend,
    ended: Option<(TurnCompleted, u64)>,
    /// A resume cut the turn short, at the resume's time.
    cut: Option<u64>,
    /// The page holding its `turn_completed`.
    closed_on: usize,
}

impl Summary {
    /// The ▣ line, once the turn has ended.
    fn closing(&self) -> Option<Line<'static>> {
        self.ended.as_ref().map(|(done, ts)| {
            let head = match done.outcome {
                TurnOutcome::Completed => "▣ completed",
                TurnOutcome::Interrupted => "▣ interrupted",
                TurnOutcome::Failed => "▣ failed",
            };
            let ms = ts.saturating_sub(self.started);
            Line::raw(format::closing(head, ms, self.calls, &self.spend.usage()))
        })
    }
}

/// A candidate cut's two outcomes: the open page as it was before the
/// step, and the new page's cards from the step on.
#[derive(Debug)]
struct Pending {
    before: Part,
    next: Part,
    seed: Seed,
}

/// What folding one line into a page's cards did.
#[derive(Debug)]
pub(crate) enum Folded {
    /// Changed no card.
    Nothing,
    /// Changed a card.
    Changed,
    /// `turn_started`: a new card.
    Started,
    /// `step_started` in the running turn.
    Stepped,
    /// `tool_call_requested` joined the running turn.
    Called,
    /// `turn_completed`, and whether it closed the running card.
    Ended(TurnCompleted, bool),
    /// `fiber_started` resumed closed the running card as cut short.
    CutShort,
}

impl Folded {
    /// Whether a card changed.
    fn changed(&self) -> bool {
        match self {
            Self::Changed | Self::Started | Self::Called | Self::CutShort => true,
            Self::Ended(_, closed) => *closed,
            Self::Nothing | Self::Stepped => false,
        }
    }
}

/// What one live line did: whether a card changed, and whether a turn now
/// runs when that changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Applied {
    pub(crate) changed: bool,
    pub(crate) busy: Option<bool>,
}

pub(crate) type TurnRanges = Vec<(usize, Range<usize>)>;
/// File paths of the calls in a page's own fold, keyed by their section target.
pub(crate) type CallPaths = Vec<(Target, String)>;
type DrawData = (Vec<Row>, Vec<RowText>, FocusItems, TurnRanges);

/// The row pinned to the conversation's top row: the header's line as
/// the shown pass drew it, its target and its first wrapped row in the
/// whole conversation (`docs/tui.md`, "Tool groups and the ledger").
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pin {
    pub(crate) line: Line<'static>,
    pub(crate) target: Target,
    pub(crate) row: usize,
}

/// The lines shown from a row on: the first shown page's first row, each
/// line with its rows and click target, and the turn ranges in those lines.
/// A page not loaded is one blank line of its rows.
#[derive(Debug)]
pub(crate) struct Shown {
    pub(crate) first: usize,
    pub(crate) lines: Vec<(Line<'static>, usize, Option<Target>)>,
    pub(crate) turns: TurnRanges,
    /// The spinning marks drawn, each a line's index and its cell's
    /// column (`docs/tui.md`, "The working line").
    pub(crate) spins: Vec<SpinMark>,
    /// The pinned header, when the top row sits inside opened content.
    pub(crate) pin: Option<Pin>,
}

/// A spinning mark: a line's index in [`Shown::lines`] and its cell's
/// column in that line.
pub(crate) type SpinMark = (usize, u16);

/// What a page draws: as shown, or with every section open for the
/// search (`docs/tui.md`, "Search": every match is counted at once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Draw {
    /// As shown: a closed section draws its own line only.
    Shown,
    /// Every collapsible body draws, open or not.
    AllOpen,
}

/// A focus stop's row, height and stable id across paging.
pub(crate) type FocusItems = Vec<(usize, usize, TargetId)>;

/// The conversation: the page index, the cards of the resident pages, each
/// turn's totals and what the person opened.
#[derive(Debug)]
pub(crate) struct Pages {
    index: Index,
    /// One per page.
    seeds: Vec<Seed>,
    /// The closed pages' cards and what the dropped ones wait on.
    residency: residency::Residency,
    /// The open page's cards, always held.
    open: Part,
    pending: Option<Pending>,
    summaries: Vec<Summary>,
    /// The summary keeping each generation's usage, by its id, so a
    /// `usage_recorded` line finds its turn without walking every turn.
    generations: BTreeMap<GenerationId, usize>,
    /// The ledger default, the last Ctrl+O.
    fold: Fold,
    /// Shell output, placed after the number of turns in the page where it ran.
    shells: Vec<(usize, usize, String)>,
    /// Where each page and turn-after keeps its shells, in insertion
    /// order: drawing a turn slot looks up its own bucket instead of
    /// walking every shell the session ever ran.
    shell_index: HashMap<(usize, usize), Vec<usize>>,
    /// Whether each page's replies hold a Markdown table, `None` until
    /// the page is first counted: tables lay out at the render width, so
    /// their logical lines move with it and the search rescans them
    /// after a width change, while any other page keeps its matches.
    /// The answer survives the page's drop and reload, since dropping
    /// keeps it and counting sets it again.
    table: Vec<Option<bool>>,
    /// Focus stops by page, with row offsets within each page.
    focus: Vec<FocusItems>,
    /// Where each page's pieces start, by page: each row offset at
    /// which a piece drawn from another line's `seq` begins, with that
    /// `seq`. A row with no source, an edge or a gap, belongs to the
    /// piece above, and the first piece starts at the page's first row.
    /// Counting sets it with the rows, so it keeps their width.
    row_seqs: Vec<Vec<(usize, Seq)>>,
    /// A held landing's top, its segment's session with its `seq`
    /// and offset as [`Pages::seq_row`] takes them, kept while a
    /// dropped page waits to be counted at the width: its rows and
    /// pieces keep the old width until it loads, so each load finds
    /// the top again.
    anchor: Option<(SessionId, Seq, usize)>,
    /// What the person opened or closed since, by target.
    overrides: BTreeMap<Target, bool>,
    /// The session's images, one id per path (`docs/tui.md`, "Images").
    images: crate::image::Layout,
    width: u16,
    /// The zone the time of day under a prompt bubble shows
    /// (`docs/tui.md`, "Turns").
    pub(crate) zone: TimeZone,
    /// The rows of the open page's lines too wide for one row, by text, so
    /// counting it again after each line wraps only what changed.
    wrapped: HashMap<String, usize>,
    /// Whether the open page's recount waits for the batch's end, and
    /// whether one waits: a batch folds many lines, and its end counts
    /// the page once, not once per changed line.
    held: bool,
    dirty: bool,
    /// How many pages were counted again (tests only: whether a line
    /// re-counted its page).
    #[cfg(test)]
    pub(crate) recounts: usize,
    /// The renderer seam's shared state: the installed renderer, the
    /// pages' caches, and whether the screen reader drops every style.
    pub(crate) ext: crate::ext::Ext,
    halves: BTreeMap<SessionId, BTreeMap<String, String>>,
}

impl Pages {
    /// An empty conversation at `width`.
    pub(crate) fn new(width: u16) -> Self {
        let seed = Seed::default();
        Self {
            index: Index::default(),
            seeds: vec![seed.clone()],
            residency: residency::Residency::default(),
            open: Part::seeded(seed),
            pending: None,
            summaries: Vec::new(),
            generations: BTreeMap::new(),
            fold: Fold::default(),
            shells: Vec::new(),
            shell_index: HashMap::new(),
            table: vec![None],
            focus: vec![Vec::new()],
            row_seqs: Vec::new(),
            anchor: None,
            overrides: BTreeMap::new(),
            images: crate::image::Layout::default(),
            width,
            zone: TimeZone::UTC,
            wrapped: HashMap::new(),
            held: false,
            dirty: false,
            #[cfg(test)]
            recounts: 0,
            ext: crate::ext::Ext::default(),
            halves: BTreeMap::new(),
        }
    }

    /// Empties the conversation; the ledger default and the zone stay.
    /// The image sizing stays too, while its ids go with the session.
    pub(crate) fn clear(&mut self) {
        let ledgers = self.fold.ledgers;
        let zone = self.zone.clone();
        let sizing = self.images.sizing;
        let handle = self.ext.handle;
        let flat = self.ext.flat;
        *self = Self::new(self.width);
        self.fold.ledgers = ledgers;
        self.open.fold.ledgers = ledgers;
        self.zone = zone;
        self.images.sizing = sizing;
        // The install slot and the flag survive a clear; the cache
        // starts empty with the new pages.
        self.ext.handle = handle;
        self.ext.flat = flat;
    }

    /// Folds one live line into the open page and the turn's totals,
    /// cutting a page where the index says.
    pub(crate) fn apply(&mut self, envelope: &Envelope) -> Applied {
        self.note_images(envelope);
        let kind = envelope.kind.as_str();
        let action = envelope.action_id.as_ref();
        // A turn, step or session cut is decided before its line is
        // folded; the others once the fold says whether the line
        // showed. The session cut must come first, so a new segment's
        // first line never folds into the closed ancestor part.
        let early = matches!(kind, "turn_started" | "step_started" | "session_started");
        let mut cut = Cut::None;
        if early && let Some(seq) = envelope.seq {
            cut = self
                .index
                .push(&envelope.session_id, seq, kind, action, true);
            match cut {
                Cut::Here => self.close(),
                Cut::Candidate => self.pending = Some(self.candidate()),
                Cut::None | Cut::AtCandidate => {}
            }
        }
        let mut pending_folded = None;
        if let Some(pending) = &mut self.pending {
            pending_folded = Some(fold(&mut pending.next, envelope));
        }
        let folded = fold(&mut self.open, envelope);
        if kind == "usage_recorded" {
            self.usage(envelope);
        }
        let folded = pending_folded
            .filter(|_| matches!(folded, Folded::Nothing))
            .unwrap_or(folded);
        let changed = folded.changed();
        if !early && let Some(seq) = envelope.seq {
            cut = self
                .index
                .push(&envelope.session_id, seq, kind, action, changed);
        }
        if cut == Cut::AtCandidate {
            self.confirm();
        } else if !self.index.pending() {
            self.pending = None;
        }
        self.summarise(folded, envelope.ts);
        // Whether a turn runs is what the open card says: a resume closes
        // the card as cut short without a `turn_completed`, which the
        // totals below only learn through `CutShort`.
        let busy = Some(self.open.turns.last().is_some_and(Turn::is_open));
        // A folded line changes the open page's search text, whatever
        // follows: closing moves its cards, but the line stays on the
        // new open page. Counting may wait for the batch's end, but the
        // revision moves now, so a search never trusts a stale scan.
        if changed && kind != "usage_recorded" {
            self.index.invalidate(self.residency.len());
        }
        // A usage line counts the page holding its turn's ▣ line itself.
        if (changed && kind != "usage_recorded") || cut != Cut::None {
            if self.held {
                // A batch folds many lines before it counts: the page is
                // marked, and the batch's end counts it once.
                self.dirty = true;
            } else {
                self.count(self.residency.len());
            }
        }
        self.rejoin();
        Applied { changed, busy }
    }

    /// Holds the open page's recount while a batch of hub lines folds.
    /// Closed pages still count at once: only the open page is
    /// recounted per line.
    pub(crate) fn hold(&mut self) {
        self.held = true;
    }

    /// Counts the open page once for the batch that folded, if one of
    /// its lines marked it, and releases the hold.
    pub(crate) fn flush(&mut self) {
        self.held = false;
        if self.dirty {
            self.dirty = false;
            self.count(self.residency.len());
        }
    }

    /// Whether the open page's recount waits for the batch's end.
    pub(crate) fn holding(&self) -> bool {
        self.held
    }

    /// The running turn's summary, if a turn runs: the last one while it
    /// has neither completed nor been cut short by a crash.
    fn running(&self) -> Option<usize> {
        let last = self.summaries.len().checked_sub(1)?;
        self.summaries
            .get(last)
            .filter(|summary| summary.ended.is_none() && summary.cut.is_none())
            .map(|_| last)
    }

    /// Keeps a live line's totals.
    fn summarise(&mut self, folded: Folded, ts: u64) {
        let at = self.residency.len();
        let running = self.running().and_then(|at| self.summaries.get_mut(at));
        match (folded, running) {
            (Folded::Started, _) => self.summaries.push(Summary {
                started: ts,
                ..Summary::default()
            }),
            (Folded::Stepped, Some(summary)) => summary.step = summary.step.saturating_add(1),
            (Folded::Called, Some(summary)) => summary.calls = summary.calls.saturating_add(1),
            (Folded::Ended(done, true), Some(summary)) => {
                summary.ended = Some((done, ts));
                summary.closed_on = at;
            }
            (Folded::CutShort, Some(summary)) => {
                summary.cut = Some(ts);
                summary.closed_on = at;
            }
            (
                Folded::Nothing
                | Folded::Changed
                | Folded::Stepped
                | Folded::Called
                | Folded::Ended(..)
                | Folded::CutShort,
                _,
            ) => {}
        }
    }

    /// `usage_recorded`: the turn holding its generation, else the running
    /// one, keeps it. A late line moves no row but its ▣ line's. The open
    /// page is counted again, at the batch's end while one folds; a closed
    /// page holding its cards only when that line's rows change, and
    /// otherwise, as a dropped page, it adds the difference.
    fn usage(&mut self, envelope: &Envelope) -> Folded {
        let Some(line) = read!(envelope, UsageRecorded) else {
            return Folded::Nothing;
        };
        crate::work::add(|work| work.usage_summaries += 1);
        let Some(at) = self
            .generations
            .get(&line.generation_id)
            .copied()
            .or_else(|| self.running())
        else {
            return Folded::Nothing;
        };
        let Some(summary) = self.summaries.get_mut(at) else {
            return Folded::Nothing;
        };
        let width = self.width;
        // The ▣ line draws at the card's inner width, split by the same
        // rule, so its count and its rows cannot drift apart
        // (`docs/tui.md`, "Turns"). The lines themselves compare, not
        // only their counts: a late line with equal rows still moves the
        // search revision when its text changed.
        let before = summary.closing();
        let generation = line.generation_id.clone();
        summary.spend.record(None, line);
        let after = summary.closing();
        let rows = |line: Option<&Line<'static>>| {
            line.map(|line| crate::rows::fitted(line, crate::surface::inset(width)))
        };
        let page = summary.closed_on;
        self.generations.insert(generation, at);
        if before != after {
            self.index.invalidate(page);
        }
        if let (Some(before), Some(after)) = (before, after) {
            let before_rows = rows(Some(&before));
            let after_rows = rows(Some(&after));
            let open = page == self.residency.len();
            if open && self.held {
                self.dirty = true;
            } else if (open || before_rows != after_rows) && self.part(page).is_some() {
                self.count(page);
            } else if let Some(held) = self.index.pages().get(page).map(|page| page.rows) {
                let rows = held
                    .saturating_sub(before_rows.unwrap_or(0))
                    .saturating_add(after_rows.unwrap_or(0));
                self.index.set_rows(page, rows);
            }
        }
        Folded::Changed
    }

    /// Closes the open page at a `turn_started`.
    fn close(&mut self) {
        let seed = Seed::live(self.summaries.len(), None, &self.open);
        self.pending = None;
        let at = self.residency.len();
        let mut part = std::mem::replace(&mut self.open, Part::seeded(seed.clone()));
        self.open.fold.carry_from(&mut part.fold);
        let spans = group_spans(&part);
        if let Some(closing) = self.seeds.get_mut(at) {
            closing.spans = spans;
            closing.carried = part.fold.take_carried();
        }
        self.residency.push(part);
        self.seeds.push(seed);
        self.focus.push(Vec::new());
        self.table.push(None);
        // Closing moves the carried descriptions off the page, so its
        // text may differ from what the search scanned: it scans again.
        self.index.invalidate(at);
        self.count(at);
    }

    /// A candidate cut at a `step_started` not yet folded.
    fn candidate(&self) -> Pending {
        let seed = match self.running() {
            Some(at) => Seed::live(
                at,
                self.summaries.get(at).map(|summary| summary.step),
                &self.open,
            ),
            None => Seed::live(self.summaries.len(), None, &self.open),
        };
        let mut next = Part::seeded(seed.clone());
        next.fold.carry_copy(&self.open.fold);
        Pending {
            before: self.open.clone(),
            next,
            seed,
        }
    }

    /// The candidate's step opened with text: the open page as it was
    /// before the step closes, and the step's cards open the next.
    fn confirm(&mut self) {
        let Some(Pending {
            mut before,
            mut next,
            mut seed,
        }) = self.pending.take()
        else {
            return;
        };
        if let Some(card) = before.turns.last_mut() {
            card.end_group();
        }
        let at = self.residency.len();
        let spans = group_spans(&before);
        // Whether the cut card's pieces drew, from the rows the pieces
        // draw, never from entries: an entry can draw nothing
        // (`docs/tui.md`, "History and paging"). The page before the cut
        // is closed and never folds another line, so whether its last
        // piece drew is final here.
        let before_drew = before.turns.last().map(|card| {
            let mut scratch = Rows::default();
            card.rows(
                self.wrap_width(),
                &self.zone,
                Edges::BOTH,
                &self.images,
                &mut scratch,
            )
            .last
        });
        let next_drew = next.turns.first().map(|card| {
            let mut scratch = Rows::default();
            card.rows(
                self.wrap_width(),
                &self.zone,
                Edges::BOTH,
                &self.images,
                &mut scratch,
            )
            .first
        });
        let joins = before_drew.unwrap_or(false) && next_drew.unwrap_or(false);
        next.joins_previous = before_drew.unwrap_or(false);
        seed.joins_previous = before_drew.unwrap_or(false);
        before.joins_next = joins;
        if let Some(closing) = self.seeds.get_mut(at) {
            closing.cut = true;
            closing.spans = spans;
            closing.carried = before.fold.take_carried();
            closing.joins_next = joins;
        }
        // The next page took its copy of the open jobs at the candidate:
        // the closed page keeps none of their descriptions.
        Fold::default().carry_from(&mut before.fold);
        self.residency.push(before);
        self.seeds.push(seed);
        self.focus.push(Vec::new());
        self.table.push(None);
        self.open = next;
        self.fold.ledgers = self.open.fold.ledgers;
        // Ending the cut group, joining the edges and moving the carried
        // descriptions may all change the closed page's text: it scans
        // again, while the new open page is fresh and unscanned.
        self.index.invalidate(at);
        self.count(at);
    }

    /// While the open page goes on from the page before and that page
    /// still draws its bottom edge, draws the open page's first turn
    /// again: once its first piece draws, the edge comes off, so the
    /// pages joined draw what one fold draws (`docs/tui.md`, "History
    /// and paging"). A piece never goes from rows back to none, so the
    /// flag only ever turns true once.
    fn rejoin(&mut self) {
        let open = self.residency.len();
        if !self.open.joins_previous {
            return;
        }
        let Some(before) = open.checked_sub(1) else {
            return;
        };
        if self.seeds.get(before).is_some_and(|seed| seed.joins_next) {
            return;
        }
        let drew = self.open.turns.first().map(|card| {
            let mut scratch = Rows::default();
            card.rows(
                self.wrap_width(),
                &self.zone,
                Edges::BOTH,
                &self.images,
                &mut scratch,
            )
            .first
        });
        if !drew.unwrap_or(false) {
            return;
        }
        if let Some(seed) = self.seeds.get_mut(before) {
            seed.joins_next = true;
        }
        if let Some(part) = self.residency.slot_mut(before) {
            part.joins_next = true;
            self.count(before);
        } else if let Some(rows) = self.index.pages().get(before).map(|page| page.rows) {
            self.index.set_rows(before, rows.saturating_sub(1));
        }
    }

    /// Folds fetched durable lines into the closed pages that hold them,
    /// each window's lines into its own segment's pages, from each page's
    /// seed, and counts their rows: every line in a page shares its
    /// session, and no page spans a continuation point. The open page is
    /// never folded again: it holds the live fold. Session state is the
    /// app's, so nothing here touches it.
    pub(crate) fn load(&mut self, session: &SessionId, lines: &[Envelope]) {
        let mut folding: Option<(usize, Vec<&Envelope>)> = None;
        for line in lines {
            self.note_images(line);
            let Some(at) = line.seq.and_then(|seq| self.index.page_of(session, seq)) else {
                continue;
            };
            if at >= self.residency.len() {
                continue;
            }
            if folding.as_ref().is_none_or(|(page, _)| *page != at) {
                if let Some((page, group)) = folding.take()
                    && self
                        .residency
                        .load(page, &group, &self.seeds, &self.overrides)
                {
                    self.count(page);
                }
                folding = Some((at, Vec::new()));
            }
            if let Some((_, group)) = &mut folding {
                group.push(line);
            }
        }
        if let Some((page, group)) = folding.take()
            && self
                .residency
                .load(page, &group, &self.seeds, &self.overrides)
        {
            self.count(page);
        }
    }

    /// Closed page `at` folded from its seed and drawn with every section
    /// open, kept nowhere: what the search scans on a dropped page
    /// (`docs/tui.md`, "History and paging": search renders each page
    /// to text and keeps only its matches). `None` for the open page,
    /// which holds the live fold, and for an unknown one.
    #[cfg(test)]
    pub(crate) fn fold_text(
        &self,
        at: usize,
        lines: &[Envelope],
    ) -> Option<(Vec<Row>, Vec<RowText>)> {
        self.fold_text_turns(at, lines)
            .map(|(rows, texts, _, _)| (rows, texts))
    }

    /// A scratch draw and its turn ranges from the same draw.
    pub(crate) fn fold_text_turns(
        &self,
        at: usize,
        lines: &[Envelope],
    ) -> Option<(Vec<Row>, Vec<RowText>, TurnRanges, CallPaths)> {
        let part = self
            .residency
            .refold(at, lines, &self.seeds, &self.overrides)?;
        let (rows, texts, _, turns) = self.draw_data(at, &part, Draw::AllOpen);
        Some((rows, texts, turns, call_paths(&part)))
    }

    /// The page holding segment `session`'s `seq` could not be loaded:
    /// it is not asked for again until the width changes.
    pub(crate) fn fail(&mut self, session: &SessionId, seq: Seq) {
        self.residency.fail(session, seq, &self.index);
    }

    /// The seq ranges to load, in order, each with its segment's session:
    /// the window's pages not resident, then the pages whose row counts
    /// are stale, then the pages a pending copy asked for. A page may be
    /// listed twice; once loaded, it is no longer needed.
    pub(crate) fn needs(&self, top: usize, height: usize) -> Vec<(SessionId, RangeInclusive<Seq>)> {
        self.residency.needs(top, height, &self.index)
    }

    /// Drops the cards of every closed page outside the window, keeping
    /// the pages a pending copy asked for until it runs.
    pub(crate) fn trim(&mut self, top: usize, height: usize) {
        let evicted = self.residency.trim(top, height, &self.index);
        for (at, session) in evicted {
            self.ext
                .cache
                .drop_page(crate::ext::cache::PageKey { session, index: at });
        }
    }

    /// How many pages the conversation holds, the open one included.
    pub(crate) fn page_count(&self) -> usize {
        self.seeds.len()
    }

    /// The first turn page `at` holds, if it holds one.
    pub(crate) fn page_first(&self, at: usize) -> Option<usize> {
        self.seeds.get(at).map(|seed| seed.first)
    }

    /// Whether the next page begins inside the same turn.
    pub(crate) fn page_cut(&self, at: usize) -> bool {
        self.seeds.get(at).is_some_and(|seed| seed.cut)
    }

    /// Keeps page `at` resident for one more pending copy.
    pub(crate) fn want(&mut self, at: usize) {
        self.residency.want(at);
    }

    /// One pending copy lets page `at` go; it drops with the window once
    /// no copy keeps it.
    pub(crate) fn unwant(&mut self, at: usize) {
        self.residency.unwant(at);
    }

    /// How many pins pending copies hold (tests only: what an abandoned
    /// copy must return to).
    #[cfg(test)]
    pub(crate) fn pinned(&self) -> usize {
        self.residency.pinned()
    }

    /// The width rows wrap at.
    pub(crate) fn wrap_width(&self) -> u16 {
        self.width
    }

    /// Whether page `at`'s load failed since the width last changed.
    /// Only a closed page can fail: the open page holds the live fold
    /// and never loads, and an unknown page holds nothing.
    pub(crate) fn page_failed(&self, at: usize) -> bool {
        if at >= self.residency.len() {
            return false;
        }
        self.residency.page_failed(at)
    }

    /// A resident page's rows and their texts as drawn now; `None` while
    /// it is dropped.
    pub(crate) fn page_text(&self, at: usize) -> Option<(Vec<Row>, Vec<RowText>)> {
        let part = self.part(at)?;
        let (rows, texts, _, _) = self.draw_data(at, part, Draw::Shown);
        Some((rows, texts))
    }

    /// A resident page's rows and texts drawn with every section open;
    /// `None` while it is dropped.
    #[cfg(test)]
    pub(crate) fn page_text_open(&self, at: usize) -> Option<(Vec<Row>, Vec<RowText>)> {
        self.page_text_open_turns(at)
            .map(|(rows, texts, _, _)| (rows, texts))
    }

    /// A resident all-open draw and its turn ranges from the same draw.
    pub(crate) fn page_text_open_turns(
        &self,
        at: usize,
    ) -> Option<(Vec<Row>, Vec<RowText>, TurnRanges, CallPaths)> {
        let part = self.part(at)?;
        let (rows, texts, _, turns) = self.draw_data(at, part, Draw::AllOpen);
        Some((rows, texts, turns, call_paths(part)))
    }

    /// Re-counts every page at `width`: a resident page in place, a dropped
    /// one once it is loaded again. A page whose replies hold a table, or
    /// whose sensitivity is still unknown, scans again: tables lay out at
    /// the render width, so their logical lines move with it. Any other
    /// page keeps the matches the search already holds.
    pub(crate) fn set_width(&mut self, width: u16) {
        if width == self.width {
            return;
        }
        // Every entry was validated at the old width: a changed page
        // width changes every derived row width with it.
        self.ext.cache.drop_entries();
        self.width = width;
        self.wrapped.clear();
        self.residency.on_width_change();
        for at in 0..=self.residency.len() {
            if self.table.get(at).copied().flatten().unwrap_or(true) {
                self.index.invalidate(at);
            }
        }
        self.recount();
    }

    /// Counts every resident page again, and marks every dropped one stale.
    pub(crate) fn recount(&mut self) {
        for at in 0..=self.residency.len() {
            if self.part(at).is_some() {
                self.count(at);
            } else {
                self.residency.mark_stale(at);
            }
        }
    }

    /// The session-half map the frame's notice seam reads, if loaded.
    pub(crate) fn halves_for(&self, session: &SessionId) -> Option<BTreeMap<String, String>> {
        self.halves.get(session).cloned()
    }

    /// Retains the latest complete half map and updates only its session's pages.
    pub(crate) fn record_halves(&mut self, session: &SessionId, halves: BTreeMap<String, String>) {
        self.halves.insert(session.clone(), halves);
        let matching: Vec<_> = self
            .index
            .pages()
            .iter()
            .enumerate()
            .filter_map(|(at, page)| (&page.session == session).then_some(at))
            .collect();
        for at in matching {
            if self.part(at).is_some() {
                self.count(at);
            } else {
                self.residency.mark_stale(at);
            }
        }
    }

    /// Records the event without interpreting eligibility in the terminal.
    pub(crate) fn record_extensions(&mut self, envelope: &Envelope) {
        if envelope.kind == "extensions_loaded"
            && let Some(loaded) = envelope
                .payload
                .get("extensions")
                .and_then(serde_json::Value::as_array)
        {
            let halves = loaded
                .iter()
                .filter_map(|half| {
                    Some((
                        half.get("name")?.as_str()?.to_owned(),
                        half.get("version")?.as_str()?.to_owned(),
                    ))
                })
                .collect();
            self.record_halves(&envelope.session_id, halves);
        }
    }

    /// Whether the screen reader drops every style from the returned
    /// spans. No production caller yet: the screen-reader ticket wires
    /// it.
    #[allow(
        dead_code,
        reason = "no production caller until the screen-reader ticket wires it"
    )]
    pub(crate) fn set_flat(&mut self, flat: bool) {
        self.ext.flat = flat;
    }

    /// Opens or closes what `target` names on a resident page; false when
    /// none holds it. It stays so when its page is dropped and loaded.
    pub(crate) fn open(&mut self, target: &Target) -> bool {
        let at = self.residency.len();
        let resident = self
            .residency
            .resident_mut()
            .chain(std::iter::once((at, &mut self.open)));
        let mut found = None;
        for (at, part) in resident {
            let state = part
                .turns
                .iter_mut()
                .find_map(|card| card.toggle(*target))
                .or_else(|| {
                    part.fold
                        .asides
                        .iter_mut()
                        .map(|(_, aside)| aside)
                        .find_map(|aside| aside.toggle(*target))
                });
            if let Some(open) = state {
                found = Some((at, open));
                break;
            }
        }
        let Some((at, open)) = found else {
            return false;
        };
        self.record(target, open, at);
        true
    }

    /// Opens what `target` names on resident pages and the pending parts,
    /// as [`Pages::open`] does but never closing: a search expanding the
    /// sections around its current match (`docs/tui.md`, "Search"). The
    /// override is recorded so a dropped page loads it open, and whether
    /// anything changed.
    pub(crate) fn force_open(&mut self, target: &Target) -> bool {
        let last = self.residency.len();
        let mut changed = false;
        for at in 0..=last {
            let found = if at == last {
                set(&mut self.open, target, true)
            } else {
                self.residency
                    .slot_mut(at)
                    .is_some_and(|part| set(part, target, true))
            };
            if found {
                self.record(target, true, at);
                changed = true;
            }
        }
        if !changed {
            // Nothing resident holds it, but a dropped page may: the
            // override still loads it open.
            self.overrides.insert(*target, true);
            if let Some(pending) = &mut self.pending {
                set(&mut pending.before, target, true);
                set(&mut pending.next, target, true);
            }
        }
        changed
    }

    /// Records what `target` names as `open`, sets it on the pending
    /// parts, and counts page `at`: what [`Pages::open`] and
    /// [`Pages::force_open`] share.
    fn record(&mut self, target: &Target, open: bool, at: usize) {
        self.overrides.insert(*target, open);
        if let Some(pending) = &mut self.pending {
            set(&mut pending.before, target, open);
            set(&mut pending.next, target, open);
        }
        self.count(at);
    }

    /// An empty conversation at this one's wrap width, zone, ledger
    /// default and image sizing; nothing else: no overrides, shells or
    /// summaries, and the open page's recount not held.
    pub(crate) fn fresh(&self) -> Pages {
        let mut fresh = Pages::new(self.width);
        fresh.fold.ledgers = self.fold.ledgers;
        fresh.open.fold.ledgers = self.fold.ledgers;
        fresh.zone = self.zone.clone();
        fresh.images.sizing = self.images.sizing;
        // The copy draws through the same install, with an empty cache
        // and the same flag.
        fresh.ext.handle = self.ext.handle;
        fresh.ext.flat = self.ext.flat;
        fresh.ext.cache = Arc::new(self.ext.cache.scratch_sharing_pending());
        fresh.halves = self.halves.clone();
        fresh
    }

    /// What `target` names on the fold, by the identity that survives a
    /// refold: calls key on the call id, groups and thoughts on the
    /// action that made them, copy targets on their reply's ordinal
    /// with the block kept, images on their path, and `Login` as is. A
    /// target the fold lacks is dropped: notes and orphaned-job lines
    /// carry no identity the fold can match, so they never carry.
    pub(crate) fn same_target(&self, target: Target, fold: &Pages) -> Option<Target> {
        match target {
            Target::Call(id) => self
                .call_action(id)
                .and_then(|action| fold.find_call(&action))
                .map(Target::Call),
            Target::CallView(id) => self
                .call_action(id)
                .and_then(|action| fold.find_call(&action))
                .map(Target::CallView),
            Target::Group(id) => self
                .group_key(id)
                .and_then(|key| fold.find_group(&key))
                .map(Target::Group),
            Target::Thought(id) => self
                .thought_action(id)
                .and_then(|action| fold.find_thought(&action))
                .map(Target::Thought),
            Target::Copy { reply, block } => self
                .reply_ordinal(reply)
                .and_then(|at| fold.reply_at(at))
                .map(|id| Target::Copy { reply: id, block }),
            Target::Image(id) => self
                .image(id)
                .and_then(|part| {
                    fold.images
                        .ids
                        .get(&(part.session.clone(), part.path.clone()))
                        .copied()
                })
                .map(Target::Image),
            Target::Login => Some(Target::Login),
            Target::Note(_) | Target::Orphans(_) => None,
        }
    }

    /// Carries the preview's closed ancestor-segment runs into the fold
    /// front beside `carry_opens`: the index runs, their seeds and their
    /// resident cards move, and every position-keyed set shifts by the
    /// carried count. A dropped ancestor page loads again under its own
    /// session; the fold's own runs keep their keys and keep folding
    /// own-log lines after land. No page spans a continuation point. A
    /// preview with no ancestor runs carries nothing.
    pub(crate) fn carry_chain(&self, fold: &mut Pages) {
        let Some(attached) = self.index.pages().last().map(|page| page.session.clone()) else {
            return;
        };
        let len = self
            .index
            .pages()
            .iter()
            .take(self.residency.len())
            .take_while(|page| page.session != attached)
            .count();
        if len == 0 {
            return;
        }
        // The summaries the carried pages draw: every summary a carried
        // seed or card names. Summaries are pushed in fold order, so the
        // carried ones are the prefix ending here.
        let mut summaries = 0;
        for at in 0..len {
            if let Some(seed) = self.seeds.get(at) {
                summaries = summaries.max(seed.first.saturating_add(1));
            }
            if let Some(part) = self.residency.part(at) {
                summaries = summaries.max(part.first.saturating_add(part.turns.len().max(1)));
            }
        }
        let summaries = summaries.min(self.summaries.len());
        fold.index.prepend_from(&self.index, len);
        fold.seeds.splice(..0, self.seeds.iter().take(len).cloned());
        for (_, part) in fold.residency.resident_mut() {
            part.first = part.first.saturating_add(summaries);
        }
        self.residency.carry_prefix_into(&mut fold.residency, len);
        fold.table.splice(..0, self.table.iter().take(len).cloned());
        fold.focus.splice(..0, self.focus.iter().take(len).cloned());
        fold.row_seqs
            .splice(..0, self.row_seqs.iter().take(len).cloned());
        for seed in fold.seeds.iter_mut().skip(len) {
            seed.first = seed.first.saturating_add(summaries);
        }
        fold.open.first = fold.open.first.saturating_add(summaries);
        if let Some(pending) = fold.pending.as_mut() {
            pending.before.first = pending.before.first.saturating_add(summaries);
            pending.next.first = pending.next.first.saturating_add(summaries);
        }
        fold.summaries
            .splice(..0, self.summaries.iter().take(summaries).cloned());
        for summary in fold.summaries.iter_mut().skip(summaries) {
            summary.closed_on = summary.closed_on.saturating_add(len);
        }
        for id in fold.generations.values_mut() {
            *id = id.saturating_add(summaries);
        }
        for (generation, id) in &self.generations {
            if *id < summaries {
                fold.generations.entry(generation.clone()).or_insert(*id);
            }
        }
        let mut shells: Vec<(usize, usize, String)> = self
            .shells
            .iter()
            .filter(|(page, _, _)| *page < len)
            .cloned()
            .collect();
        shells.extend(fold.shells.iter().map(|(page, after, text)| {
            (
                page.saturating_add(len),
                after.saturating_add(summaries),
                text.clone(),
            )
        }));
        fold.shell_index.clear();
        for (slot, (page, after, _)) in shells.iter().enumerate() {
            fold.shell_index
                .entry((*page, *after))
                .or_default()
                .push(slot);
        }
        fold.shells = shells;
        // Asides queue behind their turn, so their turn-after moves with
        // the summaries: the open and pending folds only, since a reload
        // seeds a closed page's fold without any.
        for (turn, _) in fold.open.fold.asides.iter_mut() {
            *turn = turn.saturating_add(summaries);
        }
        if let Some(pending) = fold.pending.as_mut() {
            for (turn, _) in pending.before.fold.asides.iter_mut() {
                *turn = turn.saturating_add(summaries);
            }
            for (turn, _) in pending.next.fold.asides.iter_mut() {
                *turn = turn.saturating_add(summaries);
            }
        }
        // The chain keeps one id per `(session, path)`: each carried
        // part re-notes under its own segment, so the same path in two
        // segments keeps two ids.
        for part in self.images.parts.values() {
            fold.images.note(part);
        }
    }

    /// Carries what the person opened into the fold before the swap: the
    /// ledger default, then each override the fold resolves. A target
    /// the fold lacks is dropped, and the fold counts once at the end.
    pub(crate) fn carry_opens(&self, fold: &mut Pages) {
        if self.fold.ledgers != fold.fold.ledgers {
            fold.set_ledgers(self.fold.ledgers);
        }
        for (target, open) in &self.overrides {
            let Some(resolved) = self.same_target(*target, fold) else {
                continue;
            };
            for part in fold.resident_mut() {
                set(part, &resolved, *open);
            }
            fold.overrides.insert(resolved, *open);
        }
        fold.recount();
    }

    /// Every resident part and the pending pair, for opening what
    /// `carry_opens` resolved.
    fn resident_mut(&mut self) -> impl Iterator<Item = &mut Part> {
        let pending = self
            .pending
            .iter_mut()
            .flat_map(|pending| [&mut pending.before, &mut pending.next]);
        self.residency
            .resident_mut()
            .map(|(_, part)| part)
            .chain(std::iter::once(&mut self.open))
            .chain(pending)
    }

    /// The call id of the call `id` names, anywhere resident.
    fn call_action(&self, id: usize) -> Option<String> {
        self.calls()
            .find(|(at, _)| *at == id)
            .map(|(_, action)| action)
    }

    /// The id the fold gives the call `action` names, if it folds it.
    fn find_call(&self, action: &str) -> Option<usize> {
        self.calls()
            .find(|(_, name)| name == action)
            .map(|(id, _)| id)
    }

    /// Every resident call's id and call id.
    fn calls(&self) -> impl Iterator<Item = (usize, String)> {
        self.turns().flat_map(|turn| {
            turn.groups().flat_map(|group| {
                group.sections.iter().flat_map(|section| {
                    section
                        .calls
                        .iter()
                        .map(|call| (call.id, call.action.clone()))
                })
            })
        })
    }

    /// The key of the group `id` names, anywhere resident.
    fn group_key(&self, id: usize) -> Option<String> {
        self.turns()
            .flat_map(Turn::groups)
            .filter_map(|group| group.key.clone())
            .find(|key| target_id(key) == id)
    }

    /// The id the fold gives the group `key` names, if it folds it.
    fn find_group(&self, key: &str) -> Option<usize> {
        self.turns()
            .flat_map(Turn::groups)
            .filter_map(|group| group.key.as_deref())
            .find(|name| *name == key)
            .map(target_id)
    }

    /// Every resident thought's id and action, in fold order.
    fn thoughts(&self) -> impl Iterator<Item = (usize, String)> {
        self.turns()
            .flat_map(Turn::groups)
            .flat_map(|group| {
                group
                    .sections
                    .iter()
                    .flat_map(|section| section.thoughts.iter())
            })
            .map(|thought| (thought.id, thought.action.clone()))
    }

    /// The action of the thought `id` names, anywhere resident.
    fn thought_action(&self, id: usize) -> Option<String> {
        self.thoughts()
            .find(|(at, _)| *at == id)
            .map(|(_, action)| action)
    }

    /// The id the fold gives the thought `action` names, if it folds it.
    fn find_thought(&self, action: &str) -> Option<usize> {
        self.thoughts()
            .find(|(_, name)| name == action)
            .map(|(id, _)| id)
    }

    /// The ordinal of the reply `id` names among the resident replies.
    fn reply_ordinal(&self, id: usize) -> Option<usize> {
        self.replies().position(|at| at == id)
    }

    /// The id of the `at`th resident reply, if one folds that far.
    fn reply_at(&self, at: usize) -> Option<usize> {
        self.replies().nth(at)
    }

    /// Every resident reply's copy-target id, in fold order.
    fn replies(&self) -> impl Iterator<Item = usize> {
        self.turns().flat_map(|turn| {
            turn.entries.iter().filter_map(|entry| match entry {
                Entry::Reply { reply, .. } => Some(reply.id()),
                Entry::Steer(_)
                | Entry::Group(_)
                | Entry::Aside(_)
                | Entry::Band(_)
                | Entry::Answers(_) => None,
            })
        })
    }

    /// The path of the image `id` names, if it drew.
    /// Every resident turn, closed pages first.
    fn turns(&self) -> impl Iterator<Item = &Turn> {
        let pending = self
            .pending
            .as_ref()
            .into_iter()
            .flat_map(|pending| [&pending.before, &pending.next]);
        self.residency
            .resident()
            .map(|(_, part)| part)
            .chain(std::iter::once(&self.open))
            .chain(pending)
            .flat_map(|part| part.turns.iter())
    }

    /// Ctrl+O: closes every ledger when all on the resident pages are open,
    /// else opens them all, and sets the default the dropped pages and
    /// groups made later take. The dropped pages are counted again.
    pub(crate) fn toggle_ledgers(&mut self) {
        let open = {
            let mut ledgers = self
                .residency
                .resident()
                .map(|(_, part)| part)
                .chain(std::iter::once(&self.open))
                .flat_map(|part| part.turns.iter())
                .flat_map(Turn::groups)
                .filter(|group| group.has_ledger())
                .peekable();
            if ledgers.peek().is_none() {
                !self.fold.ledgers
            } else {
                !ledgers.all(|group| group.open)
            }
        };
        self.set_ledgers(open);
    }

    /// Sets the ledger default, the dropped seeds and every resident
    /// group, dropping group overrides; the dropped pages take it on
    /// load. Counts every resident page again.
    fn set_ledgers(&mut self, open: bool) {
        self.fold.ledgers = open;
        self.open.fold.ledgers = open;
        for seed in &mut self.seeds {
            seed.ledgers = open;
        }
        if let Some(pending) = &mut self.pending {
            pending.seed.ledgers = open;
        }
        self.overrides
            .retain(|target, _| !matches!(target, Target::Group(_)));
        let pending = self
            .pending
            .iter_mut()
            .flat_map(|pending| [&mut pending.before, &mut pending.next]);
        for part in self
            .residency
            .resident_mut()
            .map(|(_, part)| part)
            .chain(std::iter::once(&mut self.open))
            .chain(pending)
        {
            part.fold.ledgers = open;
            for group in part.turns.iter_mut().flat_map(Turn::groups_mut) {
                if group.has_ledger() {
                    group.open = open;
                }
            }
        }
        self.recount();
    }

    /// The lines drawing rows `[top, top + height)`, from the first page
    /// that draws one.
    pub(crate) fn shown(&self, top: usize, height: usize) -> Shown {
        let end = top.saturating_add(height);
        let mut first = None;
        let mut lines = Vec::new();
        let mut turns = Vec::new();
        let mut spins = Vec::new();
        let mut pin: Option<Pin> = None;
        let mut pin_done = false;
        let mut start = 0usize;
        for (at, page) in self.index.pages().iter().enumerate() {
            if start >= end {
                break;
            }
            let next = start.saturating_add(page.rows);
            if next > top {
                first.get_or_insert(start);
                // The first page drawing a visible row holds the top
                // row: every page before it ends at or before `top`,
                // and every later one starts past it. The pin is read
                // once, from that page alone.
                let top_page = !pin_done;
                pin_done = true;
                match self.part(at) {
                    Some(part) => {
                        let (rows, texts, _, page_turns) = self.draw_data(at, part, Draw::Shown);
                        if top_page {
                            pin = self.pin_on_page(part, &rows, &texts, start, top);
                        }
                        let base = lines.len();
                        for (at, text) in texts.iter().enumerate() {
                            if let Some(col) = text.spin {
                                spins.push((base.saturating_add(at), col));
                            }
                        }
                        lines.extend(rows.into_iter().map(|(line, target)| {
                            let count = crate::view::rows(line.clone(), self.width);
                            (line, count, target)
                        }));
                        turns.extend(
                            page_turns
                                .into_iter()
                                .map(|(turn, range)| (turn, base + range.start..base + range.end)),
                        );
                    }
                    None => lines.push((Line::default(), page.rows, None)),
                }
            }
            start = next;
        }
        Shown {
            first: first.unwrap_or(start),
            lines,
            turns,
            spins,
            pin,
        }
    }

    /// The pin for the top row on page `part`: the innermost `Group` or
    /// `Call` scope of the top row's own text. A `Group` pins only while
    /// its ledger is open; a `Call` pins exactly while its rows exist.
    /// The header is the first line on the page with the pin's target;
    /// none when the page is dropped or the header is not on it. A found
    /// header always sits above the top row: a section's header draws
    /// before its body ever opens its scope.
    fn pin_on_page(
        &self,
        part: &Part,
        rows: &[Row],
        texts: &[RowText],
        page_start: usize,
        top: usize,
    ) -> Option<Pin> {
        let mut offset = top.saturating_sub(page_start);
        let mut line_at = None;
        for (at, (line, _)) in rows.iter().enumerate() {
            let count = crate::view::rows(line.clone(), self.width);
            if offset < count {
                line_at = Some(at);
                break;
            }
            offset = offset.saturating_sub(count);
        }
        let at = line_at?;
        let scopes = texts.get(at)?.scopes.as_slice();
        let target = scopes
            .iter()
            .rev()
            .find(|target| {
                matches!(
                    target,
                    crate::app::Target::Group(_) | crate::app::Target::Call(_)
                )
            })
            .copied()?;
        if let crate::app::Target::Group(id) = target {
            let open = part
                .turns
                .iter()
                .flat_map(crate::turn::Turn::groups)
                .filter_map(|group| group.key.as_deref().map(|key| (key, group.open)))
                .any(|(key, open)| crate::turn::target_id(key) == id && open);
            if !open {
                return None;
            }
        }
        let mut header = None;
        for (at, (line, line_target)) in rows.iter().enumerate() {
            if *line_target == Some(target) {
                header = Some((at, line.clone()));
                break;
            }
        }
        let (header_at, line) = header?;
        let mut row = page_start;
        for (line, _) in rows.iter().take(header_at) {
            row = row.saturating_add(crate::view::rows(line.clone(), self.width));
        }
        Some(Pin { line, target, row })
    }

    /// Focus stops across the indexed conversation, even on dropped pages.
    pub(crate) fn focus_items(&self) -> FocusItems {
        let mut out = Vec::new();
        let mut start = 0usize;
        for (at, page) in self.index.pages().iter().enumerate() {
            if let Some(items) = self.focus.get(at) {
                out.extend(
                    items
                        .iter()
                        .map(|(row, height, id)| (start.saturating_add(*row), *height, *id)),
                );
            }
            start = start.saturating_add(page.rows);
        }
        out
    }

    /// The text of a turn's un-targeted rows on resident pages.
    pub(crate) fn turn_text(&self, turn: usize) -> Option<String> {
        let mut text = Vec::new();
        for (at, part) in self
            .residency
            .resident()
            .chain(std::iter::once((self.residency.len(), &self.open)))
        {
            let (rows, texts, _, turns) = self.draw_data(at, part, Draw::Shown);
            for (_, range) in turns.into_iter().filter(|(id, _)| *id == turn) {
                if let (Some(turn_rows), Some(turn_texts)) =
                    (rows.get(range.clone()), texts.get(range))
                {
                    // The rows with no target of their own, in logical
                    // text: decoration adds nothing, and a wrapped row
                    // joins its line (`docs/tui.md`, "Selection and
                    // copy"). A row with a target copies through its own
                    // stop.
                    let (kept, kept_texts): (Vec<Row>, Vec<RowText>) = turn_rows
                        .iter()
                        .zip(turn_texts)
                        .filter(|((_, target), _)| target.is_none())
                        .map(|(row, text)| (row.clone(), text.clone()))
                        .unzip();
                    text.extend(
                        crate::logical::logical(&kept, &kept_texts)
                            .into_iter()
                            .map(|logical| logical.text),
                    );
                }
            }
        }
        (!text.is_empty()).then(|| text.join("\n"))
    }

    /// The resident pages' lines, in order.
    pub(crate) fn rows(&self) -> Vec<Row> {
        let mut out = Rows::default();
        for (at, part) in self
            .residency
            .resident()
            .chain(std::iter::once((self.residency.len(), &self.open)))
        {
            self.draw(at, part, &mut out);
        }
        out.into_parts().0
    }

    /// The segment session and `seq` of the line whose piece draws
    /// conversation row `row`, and the row's offset in that piece: the
    /// piece starting at or above it on its page, else the page's first
    /// `seq` at offset 0. `None` past the last row or on a page with no
    /// line.
    pub(crate) fn row_seq(&self, row: usize) -> Option<(SessionId, Seq, usize)> {
        let (at, offset) = self.index.locate(row)?;
        let page = self.index.pages().get(at).filter(|page| page.lines > 0)?;
        let runs = self.row_seqs.get(at).map_or(&[][..], Vec::as_slice);
        let before = runs.partition_point(|(start, _)| *start <= offset);
        let (seq, off) = before
            .checked_sub(1)
            .and_then(|run| runs.get(run))
            .map_or((page.first_seq, 0), |(start, seq)| {
                (*seq, offset.saturating_sub(*start))
            });
        Some((page.session.clone(), seq, off))
    }

    /// The conversation row `offset` rows into the piece drawn from
    /// segment `session`'s `seq`'s line, kept within the piece; else the
    /// first row of the first piece after it on its page, else the
    /// page's first row. `None` when no page of the segment holds
    /// `seq`: ancestor and descendant logs both start at `Seq(0)`, so
    /// the row names its segment.
    /// No test pins `>` against `>=`
    /// below: the exact arm takes every `seq` a run starts at, so where
    /// the search runs `>=` matches `>` on every input.
    #[cfg_attr(false, mutants::skip)]
    pub(crate) fn seq_row(&self, session: &SessionId, seq: Seq, offset: usize) -> Option<usize> {
        let at = self.index.page_of(session, seq)?;
        let runs = self.row_seqs.get(at).map_or(&[][..], Vec::as_slice);
        let rows = self.index.pages().get(at).map_or(0, |page| page.rows);
        let exact = runs.iter().position(|(_, from)| *from == seq);
        let within = match exact {
            Some(run) => runs.get(run).map_or(0, |(start, _)| {
                let end = runs
                    .get(run.saturating_add(1))
                    .map_or(rows, |(end, _)| *end);
                start.saturating_add(offset.min(end.saturating_sub(start.saturating_add(1))))
            }),
            None => runs
                .iter()
                .find(|(_, from)| *from > seq)
                .map_or(0, |(start, _)| *start),
        };
        Some(self.index.start(at).saturating_add(within))
    }

    /// Holds a landing's top at its segment's session with its `seq`
    /// and `offset` until no dropped page waits to be counted at the
    /// width: [`Pages::anchored_row`] finds it again after each load.
    pub(crate) fn hold_anchor(&mut self, session: &SessionId, seq: Seq, offset: usize) {
        self.anchor = Some((session.clone(), seq, offset));
    }

    /// Releases the held pass's landing position.
    pub(crate) fn clear_anchor(&mut self) {
        self.anchor = None;
    }

    /// Whether a dropped page still waits to be counted at the width:
    /// the pass stays live while one is owed.
    pub(crate) fn stale_owed(&self) -> bool {
        self.residency.stale_owed()
    }

    /// The held anchor's row as the pages count now, as
    /// [`Pages::seq_row`] finds it under its segment's session; `None`
    /// with no anchor held. The anchor lets go once every stale page is
    /// counted again or failed, since then its row no longer moves.
    pub(crate) fn anchored_row(&mut self) -> Option<usize> {
        let (session, seq, offset) = self.anchor.clone()?;
        if !self.stale_owed() {
            self.anchor = None;
        }
        self.seq_row(&session, seq, offset)
    }

    /// The page index.
    pub(crate) fn index(&self) -> &Index {
        &self.index
    }

    /// The code block target in a resident page, when it is still held:
    /// the block rendered at the width the pages wrap at.
    pub(crate) fn copy_target(&self, target: Target) -> Option<crate::markdown::CopyTarget> {
        self.residency
            .resident()
            .map(|(_, part)| part)
            .chain(std::iter::once(&self.open))
            .find_map(|part| crate::app::copy::copy_target(&part.turns, target, self.wrap_width()))
    }

    /// Adds shell output to the page where it ran.
    pub(crate) fn add_shell(&mut self, item: Option<String>) -> bool {
        let Some(item) = item else {
            return false;
        };
        let page = self.residency.len();
        let after = self.summaries.len();
        self.shell_index
            .entry((page, after))
            .or_default()
            .push(self.shells.len());
        self.shells.push((page, after, item));
        // Shell output draws on the page, so its search text changed.
        self.index.invalidate(page);
        self.count(page);
        true
    }

    /// Each page's turns, the open page first and then back through
    /// the closed ones; `None` for a page the window dropped.
    pub(crate) fn newest_first(&self) -> impl Iterator<Item = Option<&[Turn]>> {
        std::iter::once(Some(self.open.turns.as_slice())).chain(
            (0..self.residency.len())
                .rev()
                .map(|at| self.residency.part(at).map(|part| part.turns.as_slice())),
        )
    }

    /// How many pages hold cards, the open one included.
    pub(crate) fn resident(&self) -> usize {
        self.residency.resident().count().saturating_add(1)
    }

    /// Page `at`'s cards, while it holds them.
    pub(crate) fn part(&self, at: usize) -> Option<&Part> {
        if at == self.residency.len() {
            return Some(&self.open);
        }
        self.residency.part(at)
    }

    /// Whether page `at`'s replies hold a Markdown table: unknown
    /// pages answer sensitively, so the search never trusts an old
    /// table layout for them.
    pub(crate) fn page_table(&self, at: usize) -> bool {
        self.table.get(at).copied().flatten().unwrap_or(true)
    }

    /// A page's lines: each card's, and the ▣ line of each card that ends
    /// on it, drawn from its turn's totals.
    fn draw(&self, page: usize, part: &Part, out: &mut Rows) {
        let (rows, texts, _, _) = self.draw_data(page, part, Draw::Shown);
        for (row, text) in rows.into_iter().zip(texts) {
            out.push_text(row, text);
        }
    }

    /// A page's lines, their turn ranges, and stable focus stops.
    fn draw_data(&self, page: usize, part: &Part, mode: Draw) -> DrawData {
        self.draw_sourced(page, part, mode).0
    }

    /// [`Pages::draw_data`], and each line's source `seq`.
    fn draw_sourced(&self, page: usize, part: &Part, mode: Draw) -> (DrawData, Vec<Option<Seq>>) {
        // A scratch draw of a dropped page must not store entries under
        // that page's index: the window only drops resident pages, so
        // those entries would leak. It binds a throwaway cache instead,
        // which serves repeats inside the draw and is dropped with it.
        let session = self
            .index
            .pages()
            .get(page)
            .map(|page| page.session.clone())
            .unwrap_or_else(|| SessionId(String::new()));
        let ctx = crate::ext::SessionCtx {
            session_id: Some(session.clone()),
            halves: self.halves.get(&session).cloned(),
        };
        let key = crate::ext::cache::PageKey {
            session,
            index: page,
        };
        let seam = if self.part(page).is_none() {
            crate::ext::Ext {
                handle: self.ext.handle,
                cache: Arc::new(self.ext.cache.scratch_sharing_pending()),
                flat: self.ext.flat,
            }
            .bind(key, ctx)
        } else {
            self.ext.bind(key, ctx)
        };
        let mut out = match mode {
            Draw::Shown => Rows::default(),
            Draw::AllOpen => Rows::all_open(),
        }
        .with_seam(seam);
        let mut turns = Vec::new();
        let mut asides = part
            .fold
            .asides
            .iter()
            .enumerate()
            .skip(part.aside_start)
            .peekable();
        for at in 0..=part.turns.len() {
            let after = part.first.saturating_add(at);
            while let Some((_, (_, aside))) = asides.next_if(|(_, (turns, _))| *turns <= after) {
                aside.rows(crate::surface::inset(self.wrap_width()), &mut out);
            }
            // Shell output lives in the page and turn-after bucket it ran
            // in: only those candidates are examined, in the order they
            // ran, never every shell the session ever ran.
            if let Some(bucket) = self.shell_index.get(&(page, after)) {
                crate::work::add(|work| work.shell_visits += bucket.len());
                for slot in bucket {
                    if let Some((_, _, text)) = self.shells.get(*slot) {
                        out.extend(
                            text.split('\n')
                                .map(|line| (Line::raw(line.to_owned()), None)),
                        );
                    }
                }
            }
            let Some(card) = part.turns.get(at) else {
                continue;
            };
            let summary = self
                .summaries
                .get(after)
                .filter(|summary| summary.closed_on == page);
            let first = out.len();
            let edges = Edges {
                top: !(at == 0 && part.joins_previous),
                bottom: !(at + 1 == part.turns.len() && part.joins_next),
            };
            card.rows_with_summary(
                self.wrap_width(),
                &self.zone,
                edges,
                &self.images,
                summary.map(|summary| DrawSummary {
                    started: summary.started,
                    calls: summary.calls,
                    spend: &summary.spend,
                    ended: summary.ended.as_ref(),
                }),
                &mut out,
            );
            if first < out.len() {
                turns.push((after, first..out.len()));
            }
        }
        let mut starts = Vec::with_capacity(out.len());
        let mut row = 0usize;
        for (line, _) in out.iter() {
            starts.push(row);
            row = row.saturating_add(crate::view::rows(line.clone(), self.width));
        }
        let mut focus: FocusItems = turns
            .iter()
            .filter_map(|(turn, range)| {
                let line = range.start;
                Some((
                    *starts.get(line)?,
                    crate::view::rows(out.get(line)?.0.clone(), self.width),
                    TargetId::Turn(*turn),
                ))
            })
            .collect();
        focus.extend(out.iter().enumerate().filter_map(|(line, (_, target))| {
            let target = (*target)?;
            Some((
                *starts.get(line)?,
                crate::view::rows(out.get(line)?.0.clone(), self.width),
                TargetId::Line(target),
            ))
        }));
        let (rows, texts, sources) = out.into_sourced_parts();
        ((rows, texts, focus, turns), sources)
    }

    /// Counts page `at`'s rows and focus stops from its cards, while it holds them.
    fn count(&mut self, at: usize) {
        crate::work::add(|work| work.page_counts += 1);
        let Some(part) = self.part(at) else {
            return;
        };
        // Counting sees the page's current replies, so it refreshes the
        // table answer too: a reply that gained or lost a table flips
        // the page with it, and a dropped page keeps its answer until
        // its reload counts it again. Every reply source visited counts,
        // so no session-size-dependent walk hides here.
        let (page_table, sources) = classify(part);
        crate::work::add(|work| work.search_order_pages += sources);
        let ((lines, _, focus, _), sources) = self.draw_sourced(at, part, Draw::Shown);
        let width = self.width;
        let open = at == self.residency.len();
        let mut wrapped = HashMap::new();
        let mut rows = 0usize;
        let mut runs: Vec<(usize, Seq)> = Vec::new();
        for ((line, _), source) in lines.iter().zip(sources) {
            if let Some(seq) = source
                && runs.last().is_none_or(|(_, last)| *last != seq)
            {
                // Rows above the page's first piece, edges and gaps,
                // belong to it.
                runs.push((if runs.is_empty() { 0 } else { rows }, seq));
            }
            let count = if !open || line.width() <= usize::from(width) {
                crate::view::rows(line.clone(), width)
            } else {
                let text = line.to_string();
                let count = self
                    .wrapped
                    .get(&text)
                    .copied()
                    .unwrap_or_else(|| crate::view::rows(line.clone(), width));
                wrapped.insert(text, count);
                count
            };
            rows = rows.saturating_add(count);
        }
        if open {
            self.wrapped = wrapped;
        }
        #[cfg(test)]
        {
            self.recounts = self.recounts.saturating_add(1);
        }
        self.index.set_rows(at, rows);
        // The table answer grows with the pages, never shrinks: the
        // maximum holds the capacity without a guard left to mutate.
        self.table
            .resize(self.table.len().max(at.saturating_add(1)), None);
        if let Some(slot) = self.table.get_mut(at) {
            *slot = Some(page_table);
        }
        if at >= self.row_seqs.len() {
            self.row_seqs.resize_with(at.saturating_add(1), Vec::new);
        }
        if let Some(slot) = self.row_seqs.get_mut(at) {
            *slot = runs;
        }
        if at >= self.focus.len() {
            self.focus.resize_with(at, Vec::new);
            self.focus.push(focus);
        } else if let Some(items) = self.focus.get_mut(at) {
            *items = focus;
        }
    }
}

/// Whether `part`'s replies hold a Markdown table, and how many reply
/// sources were visited to tell: only replies render through the
/// table layout, so only they can make a page table-sensitive. The
/// reply caches its own answer per text, shared by its clones, so a
/// recount revisits sources without reparsing them.
fn classify(part: &Part) -> (bool, usize) {
    let mut sources = 0usize;
    for turn in &part.turns {
        for entry in &turn.entries {
            if let Entry::Reply { reply, .. } = entry {
                sources = sources.saturating_add(1);
                if reply.has_table() {
                    return (true, sources);
                }
            }
        }
    }
    (false, sources)
}

/// The live durations of `part`'s groups, in order.
fn group_spans(part: &Part) -> Vec<(u64, u64)> {
    part.turns
        .iter()
        .flat_map(Turn::groups)
        .map(|group| (group.first, group.last))
        .collect()
}

/// Restores `spans` onto a refolded page's groups, pair by pair: the same
/// durable lines fold the same groups in the same order, so the reload
/// draws the live durations without the ephemeral lines that timed them.
fn restore_spans(part: &mut Part, spans: &[(u64, u64)]) {
    let mut spans = spans.iter();
    for card in &mut part.turns {
        for group in card.groups_mut() {
            if let Some(&(first, last)) = spans.next() {
                group.first = first;
                group.last = last;
            }
        }
    }
}

/// Sets what `target` names in `part` to `open`; whether it holds it.
fn set(part: &mut Part, target: &Target, open: bool) -> bool {
    part.turns
        .iter_mut()
        .any(|card| card.set_open(target, open))
        || part
            .fold
            .asides
            .iter_mut()
            .map(|(_, aside)| aside)
            .any(|aside| aside.set_open(target, open))
}

/// Folds one line into a page's cards and fold state using the turn's
/// current rendering path.
pub(crate) fn fold(part: &mut Part, envelope: &Envelope) -> Folded {
    let kind = envelope.kind.as_str();
    let was_open = part.turns.last().is_some_and(Turn::is_open);
    let changed = crate::turn::fold_line(&mut part.turns, &mut part.fold, envelope);
    if let Some(turn) = part.turns.last_mut() {
        turn.note_seq(envelope.seq);
    }
    match kind {
        "turn_started" if changed => Folded::Started,
        // Only a resume that cuts an open turn changes the card.
        "fiber_started" if changed => Folded::CutShort,
        // `changed` is redundant here: a completed line that folded
        // nothing re-reads as nothing below, so openness alone decides.
        "turn_completed" if was_open => {
            read!(envelope, TurnCompleted).map_or(Folded::Nothing, |done| Folded::Ended(done, true))
        }
        "step_started" if was_open => Folded::Stepped,
        "tool_call_requested" if changed => Folded::Called,
        _ if changed => Folded::Changed,
        _ => Folded::Nothing,
    }
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "window/usage_tests.rs"]
mod usage_tests;
