//! The window of history (`docs/tui.md`, "History and paging"): the cards
//! folded from the pages in the window, over the page index. No event is
//! kept: each is folded into its page's cards and dropped, and a page
//! outside the window keeps only its seq range and its counts. Each turn's
//! totals, which its ▣ line draws, are kept for the whole session.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;

use contract::events::{InputItem, SteeringApplied, TurnCompleted, TurnStarted, UsageRecorded};
use contract::shapes::ContentPart;
use contract::{Envelope, Seq};
use ratatui::text::Line;

use crate::app::{Target, read};
use crate::format::{self, Spend};
use crate::pages::{Cut, Index};
use crate::turn::{Fold, Row, Turn};

/// Where folding a page begins.
#[derive(Debug, Clone, Copy)]
struct Seed {
    /// The summary of the page's first card.
    first: usize,
    /// The step its turn had reached, when the page begins inside a running
    /// turn.
    step: Option<u64>,
    /// The next page begins inside the same turn, with the text that ends
    /// this page's last group.
    cut: bool,
}

/// One page's cards: the turns from summary `first` on.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    first: usize,
    turns: Vec<Turn>,
}

impl Part {
    /// The cards a page seeded with `seed` begins with.
    fn seeded(seed: Seed) -> Self {
        Self {
            first: seed.first,
            turns: seed.step.map(Turn::part).into_iter().collect(),
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
    /// The page holding its `turn_completed`.
    closed_on: usize,
}

impl Summary {
    /// The ▣ line, once the turn has ended.
    fn closing(&self) -> Option<Line<'static>> {
        self.ended.as_ref().map(|(done, ts)| {
            let ms = ts.saturating_sub(self.started);
            format::dim(format::closing(done, ms, self.calls, &self.spend.usage()))
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
}

impl Folded {
    /// Whether a card changed.
    fn changed(&self) -> bool {
        match self {
            Self::Changed | Self::Started | Self::Called => true,
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

/// The lines shown from a row on: the first shown page's first row, and
/// each line with its rows. A page not loaded is one blank line of its
/// rows.
pub(crate) type Shown = (usize, Vec<(Line<'static>, usize)>);

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
    /// What the person opened or closed since, by target.
    overrides: BTreeMap<Target, bool>,
    /// Dropped pages whose row counts wait for a reload.
    stale: BTreeSet<usize>,
    /// Pages whose load failed since the width last changed.
    failed: BTreeSet<usize>,
    width: u16,
}

impl Pages {
    /// An empty conversation at `width`.
    pub(crate) fn new(width: u16) -> Self {
        let seed = Seed {
            first: 0,
            step: None,
            cut: false,
        };
        Self {
            index: Index::default(),
            seeds: vec![seed],
            closed: Vec::new(),
            open: Part::seeded(seed),
            pending: None,
            summaries: Vec::new(),
            fold: Fold::default(),
            overrides: BTreeMap::new(),
            stale: BTreeSet::new(),
            failed: BTreeSet::new(),
            width,
        }
    }

    /// Empties the conversation; the ledger default stays.
    pub(crate) fn clear(&mut self) {
        let ledgers = self.fold.ledgers;
        *self = Self::new(self.width);
        self.fold.ledgers = ledgers;
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
        let folded = if kind == "usage_recorded" {
            self.usage(envelope)
        } else {
            if let Some(pending) = &mut self.pending {
                fold(&mut pending.next, &self.fold, envelope);
            }
            fold(&mut self.open, &self.fold, envelope)
        };
        let changed = folded.changed();
        if !early && let Some(seq) = envelope.seq {
            cut = self.index.push(seq, kind, action, changed);
        }
        if cut == Cut::AtCandidate {
            self.confirm();
        } else if !self.index.pending() {
            self.pending = None;
        }
        let busy = match folded {
            Folded::Started => Some(true),
            Folded::Ended(..) => Some(false),
            Folded::Nothing | Folded::Changed | Folded::Stepped | Folded::Called => None,
        };
        self.summarise(folded, envelope.ts);
        if changed || cut != Cut::None {
            self.count(self.closed.len());
        }
        Applied { changed, busy }
    }

    /// The running turn's summary, if a turn runs.
    fn running(&self) -> Option<usize> {
        let last = self.summaries.len().checked_sub(1)?;
        self.summaries
            .get(last)
            .filter(|summary| summary.ended.is_none())
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
            (
                Folded::Nothing
                | Folded::Changed
                | Folded::Stepped
                | Folded::Called
                | Folded::Ended(..),
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
        let seed = Seed {
            first: self.summaries.len(),
            step: None,
            cut: false,
        };
        self.pending = None;
        let at = self.closed.len();
        let part = std::mem::replace(&mut self.open, Part::seeded(seed));
        self.closed.push(Some(part));
        self.seeds.push(seed);
        self.count(at);
    }

    /// A candidate cut at a `step_started` not yet folded.
    fn candidate(&self) -> Pending {
        let seed = match self.running() {
            Some(at) => Seed {
                first: at,
                step: self.summaries.get(at).map(|summary| summary.step),
                cut: false,
            },
            None => Seed {
                first: self.summaries.len(),
                step: None,
                cut: false,
            },
        };
        Pending {
            before: self.open.clone(),
            next: Part::seeded(seed),
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
        if let Some(closing) = self.seeds.get_mut(at) {
            closing.cut = true;
        }
        self.closed.push(Some(before));
        self.seeds.push(seed);
        self.open = next;
        self.count(at);
    }

    /// Folds fetched durable lines into the closed pages that hold them,
    /// from each page's seed, and counts their rows. The open page is
    /// never folded again: it holds the live fold. Session state is the
    /// app's, so nothing here touches it.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the loop loads pages from the next commit")
    )]
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
                let seed = self.seeds.get(at).copied();
                folding = seed.map(|seed| (at, Part::seeded(seed)));
            }
            if let Some((_, part)) = &mut folding {
                fold(part, &self.fold, line);
            }
        }
        if let Some((page, part)) = folding {
            self.keep(page, part);
        }
    }

    /// Keeps page `at`'s folded cards, closes the group the next page ends,
    /// applies what the person opened, and counts its rows.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the loop loads pages from the next commit")
    )]
    fn keep(&mut self, at: usize, mut part: Part) {
        if self.seeds.get(at).is_some_and(|seed| seed.cut)
            && let Some(card) = part.turns.last_mut()
        {
            card.end_group();
        }
        for (target, open) in &self.overrides {
            set(&mut part, target, *open);
        }
        if let Some(slot) = self.closed.get_mut(at) {
            *slot = Some(part);
        }
        self.stale.remove(&at);
        self.failed.remove(&at);
        self.count(at);
    }

    /// The page holding `seq` could not be loaded: it is not asked for
    /// again until the width changes.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the loop loads pages from the next commit")
    )]
    pub(crate) fn fail(&mut self, seq: Seq) {
        if let Some(at) = self.index.page_of(seq) {
            self.failed.insert(at);
        }
    }

    /// The seq ranges to load, in order: the window's pages not resident,
    /// then the pages whose row counts are stale. A page may be listed
    /// twice; once loaded, it is no longer needed.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the loop loads pages from the next commit")
    )]
    pub(crate) fn needs(&self, top: usize, height: usize) -> Vec<RangeInclusive<Seq>> {
        let wanted = |at: &usize| {
            !self.failed.contains(at) && self.closed.get(*at).is_some_and(Option::is_none)
        };
        let mut pages: Vec<usize> = self.index.window(top, height).filter(wanted).collect();
        pages.extend(self.stale.iter().copied().filter(wanted));
        pages
            .into_iter()
            .filter_map(|at| self.index.pages().get(at))
            .map(|page| page.first_seq..=page.last_seq)
            .collect()
    }

    /// Drops the cards of every closed page outside the window.
    pub(crate) fn trim(&mut self, top: usize, height: usize) {
        let window = self.index.window(top, height);
        for (at, part) in self.closed.iter_mut().enumerate() {
            if !window.contains(&at) {
                *part = None;
            }
        }
    }

    /// Re-counts every page at `width`: a resident page in place, a dropped
    /// one once it is loaded again.
    pub(crate) fn set_width(&mut self, width: u16) {
        if width == self.width {
            return;
        }
        self.width = width;
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
            if let Some(flag) = part.turns.iter_mut().find_map(|card| card.flag(target)) {
                *flag = !*flag;
                found = Some((at, *flag));
                break;
            }
        }
        let Some((at, open)) = found else {
            return false;
        };
        self.overrides.insert(target.clone(), open);
        if let Some(pending) = &mut self.pending {
            set(&mut pending.before, target, open);
            set(&mut pending.next, target, open);
        }
        self.count(at);
        true
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
                .filter(|group| group.has_calls())
                .peekable();
            if ledgers.peek().is_none() {
                !self.fold.ledgers
            } else {
                !ledgers.all(|group| group.open)
            }
        };
        self.fold.ledgers = open;
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
            for group in part.turns.iter_mut().flat_map(Turn::groups_mut) {
                if group.has_calls() {
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
                        let mut rows = Vec::new();
                        self.draw(part, &mut rows);
                        lines.extend(rows.into_iter().map(|(line, _)| {
                            let count = crate::view::rows(line.clone(), self.width);
                            (line, count)
                        }));
                    }
                    None => lines.push((Line::default(), page.rows)),
                }
            }
            start = next;
        }
        (first.unwrap_or(start), lines)
    }

    /// The resident pages' lines, in order.
    pub(crate) fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        for part in self
            .closed
            .iter()
            .flatten()
            .chain(std::iter::once(&self.open))
        {
            self.draw(part, &mut out);
        }
        out
    }

    /// The page index.
    pub(crate) fn index(&self) -> &Index {
        &self.index
    }

    /// How many pages hold cards, the open one included.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the loop loads pages from the next commit")
    )]
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
    fn draw(&self, part: &Part, out: &mut Vec<Row>) {
        for (at, card) in part.turns.iter().enumerate() {
            card.rows(self.width, out);
            let summary = self.summaries.get(part.first.saturating_add(at));
            if !card.is_open()
                && let Some(line) = summary.and_then(Summary::closing)
            {
                out.push((line, None));
            }
        }
    }

    /// Counts page `at`'s rows from its cards, while it holds them.
    fn count(&mut self, at: usize) {
        let Some(part) = self.part(at) else {
            return;
        };
        let mut lines = Vec::new();
        self.draw(part, &mut lines);
        let rows = lines.into_iter().fold(0usize, |sum, (line, _)| {
            sum.saturating_add(crate::view::rows(line, self.width))
        });
        self.index.set_rows(at, rows);
    }
}

/// Sets what `target` names in `part` to `open`, when it holds it.
fn set(part: &mut Part, target: &Target, open: bool) {
    if let Some(flag) = part.turns.iter_mut().find_map(|card| card.flag(target)) {
        *flag = open;
    }
}

/// The running card, if any.
fn running(turns: &mut [Turn]) -> Option<&mut Turn> {
    turns.last_mut().filter(|turn| turn.is_open())
}

/// Folds one line into a page's cards. Only the cards: a turn's totals and
/// session state are kept by their owners, so folding a fetched page again
/// touches neither.
///
/// debt: a completion arriving more than one turn after its call lands on
/// a later page and finds no card, so the call keeps its running glyph;
/// upgrade on a log showing completions two turns late.
pub(crate) fn fold(part: &mut Part, fold: &Fold, envelope: &Envelope) -> Folded {
    match envelope.kind.as_str() {
        "turn_started" => read!(envelope, TurnStarted).map_or(Folded::Nothing, |started| {
            let prompts = started
                .input
                .iter()
                .filter_map(|input| {
                    if let InputItem::Message { content, .. } = input {
                        Some(text_of(content))
                    } else {
                        None
                    }
                })
                .collect();
            part.turns.push(Turn::new(prompts));
            Folded::Started
        }),
        "turn_completed" => read!(envelope, TurnCompleted).map_or(Folded::Nothing, |done| {
            let closed = running(&mut part.turns).map(Turn::complete).is_some();
            Folded::Ended(done, closed)
        }),
        "steering_applied" => read!(envelope, SteeringApplied)
            .and_then(|applied| {
                running(&mut part.turns).map(|turn| turn.steer(text_of(&applied.content)))
            })
            .map_or(Folded::Nothing, |()| Folded::Changed),
        "step_started" => running(&mut part.turns)
            .map(Turn::step_started)
            .map_or(Folded::Nothing, |()| Folded::Stepped),
        kind => {
            let changed = envelope.action_id.as_ref().is_some_and(|action| {
                crate::turn::fold_action(&mut part.turns, fold, envelope, &action.0)
            });
            match (changed, kind) {
                (false, _) => Folded::Nothing,
                (true, "tool_call_requested") => Folded::Called,
                (true, _) => Folded::Changed,
            }
        }
    }
}

/// The text parts of a message, joined.
fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .collect()
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;
