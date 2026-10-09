//! The window of history (`docs/tui.md`, "History and paging"): the cards
//! folded from the pages in the window, over the page index. No event is
//! kept: each is folded into its page's cards and dropped, and a page
//! outside the window keeps only its seq range and its counts. Each turn's
//! totals, which its ▣ line draws, are kept for the whole session.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::{Range, RangeInclusive};

use contract::events::{TurnCompleted, TurnOutcome, UsageRecorded};
use contract::{Envelope, Seq};
use jiff::tz::TimeZone;
use ratatui::text::Line;

use crate::app::{Target, read};
use crate::format::{self, Spend};
use crate::mouse::TargetId;
use crate::pages::{Cut, Index};
use crate::rows::{RowText, Rows};
use crate::turn::{Fold, Row, Turn};

mod pins;

/// Where folding a page begins: its turn and step, the live fold's scalar
/// continuation state, and descriptions only of earlier pages' jobs that
/// complete orphaned on it. Rendered asides stay in resident pages.
#[derive(Debug, Clone, Default)]
struct Seed {
    /// The summary of the page's first card.
    first: usize,
    /// The step its turn had reached, when the page begins inside a running
    /// turn.
    step: Option<u64>,
    /// The next page begins inside the same turn, with the text that ends
    /// this page's last group.
    cut: bool,
    /// The next target id.
    next: usize,
    /// Whether a new group starts with its ledger open.
    ledgers: bool,
    /// The context size an automatic handoff runs at.
    trigger_at: Option<u64>,
    /// The last `fiber_exited` carried `suspended_on`.
    suspended: bool,
    /// Descriptions of jobs that complete orphaned on this page but started
    /// on an earlier one, set once the page closes.
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
        }
    }
}

/// One turn's totals for its ▣ line, kept for the whole session.
#[derive(Debug, Default)]
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

type TurnRanges = Vec<(usize, Range<usize>)>;
type DrawData = (Vec<Row>, Vec<RowText>, FocusItems, TurnRanges);

/// The lines shown from a row on: the first shown page's first row, each
/// line with its rows and click target, and the turn ranges in those lines.
/// A page not loaded is one blank line of its rows.
#[derive(Debug)]
pub(crate) struct Shown {
    pub(crate) first: usize,
    pub(crate) lines: Vec<(Line<'static>, usize, Option<Target>)>,
    pub(crate) turns: TurnRanges,
}

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
    /// Each closed page's cards, `None` while dropped.
    closed: Vec<Option<Part>>,
    /// The open page's cards, always held.
    open: Part,
    pending: Option<Pending>,
    summaries: Vec<Summary>,
    /// The ledger default, the last Ctrl+O.
    fold: Fold,
    /// Shell output, placed after the number of turns in the page where it ran.
    shells: Vec<(usize, usize, String)>,
    /// Focus stops by page, with row offsets within each page.
    focus: Vec<FocusItems>,
    /// What the person opened or closed since, by target.
    overrides: BTreeMap<Target, bool>,
    /// Dropped pages whose row counts wait for a reload.
    stale: BTreeSet<usize>,
    /// Pages whose load failed since the width last changed.
    failed: BTreeSet<usize>,
    /// Dropped pages a pending copy asked for, kept until it runs.
    pins: pins::Pins,
    width: u16,
    /// The zone the time of day under a prompt bubble shows
    /// (`docs/tui.md`, "Turns").
    pub(crate) zone: TimeZone,
    /// The rows of the open page's lines too wide for one row, by text, so
    /// counting it again after each line wraps only what changed.
    wrapped: HashMap<String, usize>,
    /// How many pages were counted again (tests only: whether a line
    /// re-counted its page).
    #[cfg(test)]
    pub(crate) recounts: usize,
}

impl Pages {
    /// An empty conversation at `width`.
    pub(crate) fn new(width: u16) -> Self {
        let seed = Seed::default();
        Self {
            index: Index::default(),
            seeds: vec![seed.clone()],
            closed: Vec::new(),
            open: Part::seeded(seed),
            pending: None,
            summaries: Vec::new(),
            fold: Fold::default(),
            shells: Vec::new(),
            focus: vec![Vec::new()],
            overrides: BTreeMap::new(),
            stale: BTreeSet::new(),
            failed: BTreeSet::new(),
            pins: pins::Pins::default(),
            width,
            zone: TimeZone::UTC,
            wrapped: HashMap::new(),
            #[cfg(test)]
            recounts: 0,
        }
    }

    /// Empties the conversation; the ledger default and the zone stay.
    pub(crate) fn clear(&mut self) {
        let ledgers = self.fold.ledgers;
        let zone = self.zone.clone();
        *self = Self::new(self.width);
        self.fold.ledgers = ledgers;
        self.open.fold.ledgers = ledgers;
        self.zone = zone;
    }

    /// Folds one live line into the open page and the turn's totals,
    /// cutting a page where the index says.
    pub(crate) fn apply(&mut self, envelope: &Envelope) -> Applied {
        let kind = envelope.kind.as_str();
        let action = envelope.action_id.as_ref();
        // A turn or step cut is decided before its line is folded; the
        // others once the fold says whether the line showed.
        let early = matches!(kind, "turn_started" | "step_started");
        let mut cut = Cut::None;
        if early && let Some(seq) = envelope.seq {
            cut = self.index.push(seq, kind, action, true);
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
            cut = self.index.push(seq, kind, action, changed);
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
        // A usage line counts the page holding its turn's ▣ line itself.
        if (changed && kind != "usage_recorded") || cut != Cut::None {
            self.count(self.closed.len());
        }
        Applied { changed, busy }
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
        let at = self.closed.len();
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
    /// one, keeps it. A late line moves no row on a dropped page but its ▣
    /// line's.
    fn usage(&mut self, envelope: &Envelope) -> Folded {
        let Some(line) = read!(envelope, UsageRecorded) else {
            return Folded::Nothing;
        };
        let known = self
            .summaries
            .iter()
            .rposition(|summary| summary.spend.holds(&line.generation_id));
        let Some(summary) = known
            .or_else(|| self.running())
            .and_then(|at| self.summaries.get_mut(at))
        else {
            return Folded::Nothing;
        };
        let width = self.width;
        let rows = |summary: &Summary| summary.closing().map(|line| crate::view::rows(line, width));
        let before = rows(summary);
        summary.spend.record(&line);
        let after = rows(summary);
        let page = summary.closed_on;
        if let (Some(before), Some(after)) = (before, after) {
            if self.part(page).is_some() {
                self.count(page);
            } else if let Some(held) = self.index.pages().get(page).map(|page| page.rows) {
                let rows = held.saturating_sub(before).saturating_add(after);
                self.index.set_rows(page, rows);
            }
        }
        Folded::Changed
    }

    /// Closes the open page at a `turn_started`.
    fn close(&mut self) {
        let seed = Seed::live(self.summaries.len(), None, &self.open);
        self.pending = None;
        let at = self.closed.len();
        let mut part = std::mem::replace(&mut self.open, Part::seeded(seed.clone()));
        self.open.fold.carry_from(&mut part.fold);
        let spans = group_spans(&part);
        if let Some(closing) = self.seeds.get_mut(at) {
            closing.spans = spans;
            closing.carried = part.fold.take_carried();
        }
        self.closed.push(Some(part));
        self.seeds.push(seed);
        self.focus.push(Vec::new());
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
            next,
            seed,
        }) = self.pending.take()
        else {
            return;
        };
        if let Some(card) = before.turns.last_mut() {
            card.end_group();
        }
        let at = self.closed.len();
        let spans = group_spans(&before);
        if let Some(closing) = self.seeds.get_mut(at) {
            closing.cut = true;
            closing.spans = spans;
            closing.carried = before.fold.take_carried();
        }
        // The next page took its copy of the open jobs at the candidate:
        // the closed page keeps none of their descriptions.
        Fold::default().carry_from(&mut before.fold);
        self.closed.push(Some(before));
        self.seeds.push(seed);
        self.focus.push(Vec::new());
        self.open = next;
        self.fold.ledgers = self.open.fold.ledgers;
        self.count(at);
    }

    /// Folds fetched durable lines into the closed pages that hold them,
    /// from each page's seed, and counts their rows. The open page is
    /// never folded again: it holds the live fold. Session state is the
    /// app's, so nothing here touches it.
    pub(crate) fn load(&mut self, lines: &[Envelope]) {
        let mut folding: Option<(usize, Part)> = None;
        for line in lines {
            let Some(at) = line.seq.and_then(|seq| self.index.page_of(seq)) else {
                continue;
            };
            if at >= self.closed.len() {
                continue;
            }
            if folding.as_ref().is_none_or(|(page, _)| *page != at) {
                if let Some((page, part)) = folding.take() {
                    self.keep(page, part);
                }
                folding = self.begin(at).map(|part| (at, part));
            }
            if let Some((_, part)) = &mut folding {
                fold(part, line);
            }
        }
        if let Some((page, part)) = folding {
            self.keep(page, part);
        }
    }

    /// The cards page `at` starts folding from: its seed's, or `None` for
    /// the open page, which holds the live fold, and for an unknown one.
    fn begin(&self, at: usize) -> Option<Part> {
        if at >= self.closed.len() {
            return None;
        }
        self.seeds.get(at).cloned().map(Part::seeded)
    }

    /// Finishes page `at`'s folded cards without keeping them: closes the
    /// group the next page ends, applies what the person opened, and
    /// restores the live durations, as [`Pages::keep`] does before it
    /// counts. The search folds a dropped page through it and draws the
    /// result without keeping it.
    fn finish(&self, at: usize, part: &mut Part) {
        if self.seeds.get(at).is_some_and(|seed| seed.cut)
            && let Some(card) = part.turns.last_mut()
        {
            card.end_group();
        }
        for (target, open) in &self.overrides {
            set(part, target, *open);
        }
        if let Some(seed) = self.seeds.get(at) {
            restore_spans(part, &seed.spans);
        }
    }

    /// Keeps page `at`'s folded cards, closes the group the next page ends,
    /// applies what the person opened, and counts its rows.
    fn keep(&mut self, at: usize, mut part: Part) {
        self.finish(at, &mut part);
        if let Some(slot) = self.closed.get_mut(at) {
            *slot = Some(part);
        }
        self.stale.remove(&at);
        self.failed.remove(&at);
        self.count(at);
    }

    /// Closed page `at` folded from its seed and drawn with every section
    /// open, kept nowhere: what the search scans on a dropped page
    /// (`docs/tui.md`, "History and paging": search renders each page
    /// to text and keeps only its matches). `None` for the open page,
    /// which holds the live fold, and for an unknown one.
    pub(crate) fn fold_text(
        &self,
        at: usize,
        lines: &[Envelope],
    ) -> Option<(Vec<Row>, Vec<RowText>)> {
        let mut part = self.begin(at)?;
        for line in lines {
            fold(&mut part, line);
        }
        self.finish(at, &mut part);
        let (rows, texts, _, _) = self.draw_data(at, &part, Draw::AllOpen);
        Some((rows, texts))
    }

    /// The page holding `seq` could not be loaded: it is not asked for
    /// again until the width changes.
    pub(crate) fn fail(&mut self, seq: Seq) {
        if let Some(at) = self.index.page_of(seq) {
            self.failed.insert(at);
        }
    }

    /// The seq ranges to load, in order: the window's pages not resident,
    /// then the pages whose row counts are stale, then the pages a
    /// pending copy asked for. A page may be listed twice; once loaded,
    /// it is no longer needed.
    pub(crate) fn needs(&self, top: usize, height: usize) -> Vec<RangeInclusive<Seq>> {
        let wanted = |at: &usize| {
            !self.failed.contains(at) && self.closed.get(*at).is_some_and(Option::is_none)
        };
        let mut pages: Vec<usize> = self.index.window(top, height).filter(wanted).collect();
        pages.extend(self.stale.iter().copied().filter(wanted));
        pages.extend(self.pins.pages().filter(wanted));
        pages
            .into_iter()
            .filter_map(|at| self.index.pages().get(at))
            .map(|page| page.first_seq..=page.last_seq)
            .collect()
    }

    /// Drops the cards of every closed page outside the window, keeping
    /// the pages a pending copy asked for until it runs.
    pub(crate) fn trim(&mut self, top: usize, height: usize) {
        let window = self.index.window(top, height);
        for (at, part) in self.closed.iter_mut().enumerate() {
            if !window.contains(&at) && !self.pins.contains(at) {
                *part = None;
            }
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
        self.pins.pin(at);
    }

    /// One pending copy lets page `at` go; it drops with the window once
    /// no copy keeps it.
    pub(crate) fn unwant(&mut self, at: usize) {
        self.pins.unpin(at);
    }

    /// How many pins pending copies hold (tests only: what an abandoned
    /// copy must return to).
    #[cfg(test)]
    pub(crate) fn pinned(&self) -> usize {
        self.pins.total()
    }

    /// The width rows wrap at.
    pub(crate) fn wrap_width(&self) -> u16 {
        self.width
    }

    /// Whether page `at`'s load failed since the width last changed.
    pub(crate) fn page_failed(&self, at: usize) -> bool {
        self.failed.contains(&at)
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
    pub(crate) fn page_text_open(&self, at: usize) -> Option<(Vec<Row>, Vec<RowText>)> {
        let part = self.part(at)?;
        let (rows, texts, _, _) = self.draw_data(at, part, Draw::AllOpen);
        Some((rows, texts))
    }

    /// Re-counts every page at `width`: a resident page in place, a dropped
    /// one once it is loaded again.
    pub(crate) fn set_width(&mut self, width: u16) {
        if width == self.width {
            return;
        }
        self.width = width;
        self.wrapped.clear();
        self.failed.clear();
        self.recount();
    }

    /// Counts every resident page again, and marks every dropped one stale.
    fn recount(&mut self) {
        for at in 0..=self.closed.len() {
            if self.part(at).is_some() {
                self.count(at);
            } else {
                self.stale.insert(at);
            }
        }
    }

    /// Opens or closes what `target` names on a resident page; false when
    /// none holds it. It stays so when its page is dropped and loaded.
    pub(crate) fn open(&mut self, target: &Target) -> bool {
        let at = self.closed.len();
        let resident = self
            .closed
            .iter_mut()
            .enumerate()
            .filter_map(|(at, part)| part.as_mut().map(|part| (at, part)))
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
        let last = self.closed.len();
        let mut changed = false;
        for at in 0..=last {
            let found = if at == last {
                set(&mut self.open, target, true)
            } else {
                self.closed
                    .get_mut(at)
                    .is_some_and(|part| part.as_mut().is_some_and(|part| set(part, target, true)))
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

    /// Ctrl+O: closes every ledger when all on the resident pages are open,
    /// else opens them all, and sets the default the dropped pages and
    /// groups made later take. The dropped pages are counted again.
    pub(crate) fn toggle_ledgers(&mut self) {
        let open = {
            let mut ledgers = self
                .closed
                .iter()
                .flatten()
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
            .closed
            .iter_mut()
            .flatten()
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
        let mut start = 0usize;
        for (at, page) in self.index.pages().iter().enumerate() {
            if start >= end {
                break;
            }
            let next = start.saturating_add(page.rows);
            if next > top {
                first.get_or_insert(start);
                match self.part(at) {
                    Some(part) => {
                        let (rows, _, _, page_turns) = self.draw_data(at, part, Draw::Shown);
                        let base = lines.len();
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
        }
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
            .closed
            .iter()
            .enumerate()
            .filter_map(|(at, part)| part.as_ref().map(|part| (at, part)))
            .chain(std::iter::once((self.closed.len(), &self.open)))
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
            .closed
            .iter()
            .enumerate()
            .filter_map(|(at, part)| part.as_ref().map(|part| (at, part)))
            .chain(std::iter::once((self.closed.len(), &self.open)))
        {
            self.draw(at, part, &mut out);
        }
        out.into_parts().0
    }

    /// The page index.
    pub(crate) fn index(&self) -> &Index {
        &self.index
    }

    /// The code block target in a resident page, when it is still held.
    pub(crate) fn copy_target(
        &self,
        target: Target,
        width: u16,
    ) -> Option<crate::markdown::CopyTarget> {
        self.closed
            .iter()
            .flatten()
            .chain(std::iter::once(&self.open))
            .find_map(|part| crate::app::copy::copy_target(&part.turns, target, width))
    }

    /// Adds shell output to the page where it ran.
    pub(crate) fn add_shell(&mut self, item: Option<String>) -> bool {
        let Some(item) = item else {
            return false;
        };
        self.shells
            .push((self.closed.len(), self.summaries.len(), item));
        self.count(self.closed.len());
        true
    }

    /// How many pages hold cards, the open one included.
    pub(crate) fn resident(&self) -> usize {
        self.closed.iter().flatten().count().saturating_add(1)
    }

    /// Page `at`'s cards, while it holds them.
    pub(crate) fn part(&self, at: usize) -> Option<&Part> {
        if at == self.closed.len() {
            return Some(&self.open);
        }
        self.closed.get(at).and_then(Option::as_ref)
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
        let mut out = match mode {
            Draw::Shown => Rows::default(),
            Draw::AllOpen => Rows::all_open(),
        };
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
                aside.rows(&mut out);
            }
            for (on_page, shell_after, text) in &self.shells {
                if *on_page == page && *shell_after == after {
                    out.extend(
                        text.split('\n')
                            .map(|line| (Line::raw(line.to_owned()), None)),
                    );
                }
            }
            let Some(card) = part.turns.get(at) else {
                continue;
            };
            let summary = self.summaries.get(after);
            let mut card = card.clone();
            if let Some(summary) = summary.filter(|summary| summary.closed_on == page) {
                card.summary(
                    summary.started,
                    summary.calls,
                    &summary.spend,
                    summary.ended.as_ref(),
                );
            }
            let first = out.len();
            card.rows(self.width, &self.zone, &mut out);
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
        let (rows, texts) = out.into_parts();
        (rows, texts, focus, turns)
    }

    /// Counts page `at`'s rows and focus stops from its cards, while it holds them.
    fn count(&mut self, at: usize) {
        let Some(part) = self.part(at) else {
            return;
        };
        let (lines, _, focus, _) = self.draw_data(at, part, Draw::Shown);
        let width = self.width;
        let open = at == self.closed.len();
        let mut wrapped = HashMap::new();
        let mut rows = 0usize;
        for (line, _) in &lines {
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
        if at >= self.focus.len() {
            self.focus.resize_with(at, Vec::new);
            self.focus.push(focus);
        } else if let Some(items) = self.focus.get_mut(at) {
            *items = focus;
        }
    }
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
