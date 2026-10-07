//! The window of history (`docs/tui.md`, "History and paging"): the cards
//! folded from the pages in the window, over the page index. No event is
//! kept: each is folded into its page's cards and dropped, and a page
//! outside the window keeps only its seq range and its counts. Each turn's
//! totals, which its ▣ line draws, are kept for the whole session.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::RangeInclusive;

use contract::events::{TurnCompleted, TurnOutcome, UsageRecorded};
use contract::{Envelope, Seq};
use ratatui::text::Line;

use crate::app::{Target, read};
use crate::format::{self, Spend};
use crate::pages::{Cut, Index};
use crate::turn::{Fold, Row, Turn};

/// Where folding a page begins.
#[derive(Debug, Clone)]
struct Seed {
    /// The summary of the page's first card.
    first: usize,
    /// The step its turn had reached, when the page begins inside a running
    /// turn.
    step: Option<u64>,
    /// The next page begins inside the same turn, with the text that ends
    /// this page's last group.
    cut: bool,
    /// The turn fold's state at the start of this page.
    fold: Fold,
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
            fold: seed.fold.clone(),
            aside_start: seed.fold.asides.len(),
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

/// The lines shown from a row on: the first shown page's first row, and
/// each line with its rows and click target. A page not loaded is one blank
/// line of its rows.
pub(crate) type Shown = (usize, Vec<(Line<'static>, usize, Option<Target>)>);

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
    /// What the person opened or closed since, by target.
    overrides: BTreeMap<Target, bool>,
    /// Dropped pages whose row counts wait for a reload.
    stale: BTreeSet<usize>,
    /// Pages whose load failed since the width last changed.
    failed: BTreeSet<usize>,
    width: u16,
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
        let fold = Fold::default();
        let seed = Seed {
            first: 0,
            step: None,
            cut: false,
            fold: fold.clone(),
        };
        Self {
            index: Index::default(),
            seeds: vec![seed.clone()],
            closed: Vec::new(),
            open: Part::seeded(seed),
            pending: None,
            summaries: Vec::new(),
            fold,
            shells: Vec::new(),
            overrides: BTreeMap::new(),
            stale: BTreeSet::new(),
            failed: BTreeSet::new(),
            width,
            wrapped: HashMap::new(),
            #[cfg(test)]
            recounts: 0,
        }
    }

    /// Empties the conversation; the ledger default stays.
    pub(crate) fn clear(&mut self) {
        let ledgers = self.fold.ledgers;
        *self = Self::new(self.width);
        self.fold.ledgers = ledgers;
        self.open.fold.ledgers = ledgers;
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
        let seed = Seed {
            first: self.summaries.len(),
            step: None,
            cut: false,
            fold: self.open.fold.clone(),
        };
        self.pending = None;
        let at = self.closed.len();
        let part = std::mem::replace(&mut self.open, Part::seeded(seed.clone()));
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
                fold: self.open.fold.clone(),
            },
            None => Seed {
                first: self.summaries.len(),
                step: None,
                cut: false,
                fold: self.open.fold.clone(),
            },
        };
        Pending {
            before: self.open.clone(),
            next: Part::seeded(seed.clone()),
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
                let seed = self.seeds.get(at).cloned();
                folding = seed.map(|seed| (at, Part::seeded(seed)));
            }
            if let Some((_, part)) = &mut folding {
                fold(part, line);
            }
        }
        if let Some((page, part)) = folding {
            self.keep(page, part);
        }
    }

    /// Keeps page `at`'s folded cards, closes the group the next page ends,
    /// applies what the person opened, and counts its rows.
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
    pub(crate) fn fail(&mut self, seq: Seq) {
        if let Some(at) = self.index.page_of(seq) {
            self.failed.insert(at);
        }
    }

    /// The seq ranges to load, in order: the window's pages not resident,
    /// then the pages whose row counts are stale. A page may be listed
    /// twice; once loaded, it is no longer needed.
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
        self.overrides.insert(*target, open);
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
            seed.fold.ledgers = open;
        }
        if let Some(pending) = &mut self.pending {
            pending.seed.fold.ledgers = open;
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
                        self.draw(at, part, &mut rows);
                        lines.extend(rows.into_iter().map(|(line, target)| {
                            let count = crate::view::rows(line.clone(), self.width);
                            (line, count, target)
                        }));
                    }
                    None => lines.push((Line::default(), page.rows, None)),
                }
            }
            start = next;
        }
        (first.unwrap_or(start), lines)
    }

    /// The resident pages' lines, in order.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        for (at, part) in self
            .closed
            .iter()
            .enumerate()
            .filter_map(|(at, part)| part.as_ref().map(|part| (at, part)))
            .chain(std::iter::once((self.closed.len(), &self.open)))
        {
            self.draw(at, part, &mut out);
        }
        out
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
    fn draw(&self, page: usize, part: &Part, out: &mut Vec<Row>) {
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
                aside.rows(out);
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
            card.rows(self.width, out);
        }
    }

    /// Counts page `at`'s rows from its cards, while it holds them.
    fn count(&mut self, at: usize) {
        let Some(part) = self.part(at) else {
            return;
        };
        let mut lines = Vec::new();
        self.draw(at, part, &mut lines);
        let width = self.width;
        let open = at == self.closed.len();
        let mut wrapped = HashMap::new();
        let mut rows = 0usize;
        for (line, _) in lines {
            let count = if !open || line.width() <= usize::from(width) {
                crate::view::rows(line, width)
            } else {
                let text = line.to_string();
                let count = self
                    .wrapped
                    .get(&text)
                    .copied()
                    .unwrap_or_else(|| crate::view::rows(line, width));
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
    }
}

/// Sets what `target` names in `part` to `open`, when it holds it.
fn set(part: &mut Part, target: &Target, open: bool) {
    if !part
        .turns
        .iter_mut()
        .any(|card| card.set_open(target, open))
    {
        part.fold
            .asides
            .iter_mut()
            .map(|(_, aside)| aside)
            .any(|aside| aside.set_open(target, open));
    }
}

/// Folds one line into a page's cards and fold state using the turn's
/// current rendering path.
pub(crate) fn fold(part: &mut Part, envelope: &Envelope) -> Folded {
    let kind = envelope.kind.as_str();
    let was_open = part.turns.last().is_some_and(Turn::is_open);
    let changed = crate::turn::fold_line(&mut part.turns, &mut part.fold, envelope);
    match kind {
        "turn_started" if changed => Folded::Started,
        // A resume that folded something closed the card it found open:
        // `changed` alone says the rule below fired, closing nothing when
        // the process is fresh or suspended the turn instead.
        "fiber_started" if changed && was_open && !part.turns.last().is_some_and(Turn::is_open) => {
            Folded::CutShort
        }
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
