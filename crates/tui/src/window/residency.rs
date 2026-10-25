//! The closed pages' residency: their cards while held, and what a
//! dropped page waits on (`docs/tui.md`, "History and paging"). A page
//! outside the window keeps only its seq range and its counts; loading
//! folds its durable lines again from its seed, and a width change
//! clears what failed since the last one.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;

use contract::{Envelope, Seq, SessionId};

use super::pins::Pins;
use super::{Part, Seed, fold, restore_spans, set};
use crate::app::Target;
use crate::pages::Index;

/// The closed pages' cards and what the dropped ones wait on: every
/// kept page's cards, the set holding them, the dropped pages owed a
/// count or a load, the loads that failed since the width last changed,
/// and the pages a pending copy pins past the window.
#[derive(Debug, Default)]
pub(crate) struct Residency {
    /// Each closed page's cards, `None` while dropped.
    closed: Vec<Option<Part>>,
    /// The closed pages holding cards: `trim` drops from this set
    /// instead of scanning every page, so settling costs residents,
    /// not the session. The set over-approximates: a dropped page may
    /// linger in it, and every site that keeps cards inserts, so no
    /// kept page is ever missing.
    resident_set: BTreeSet<usize>,
    /// Dropped pages whose row counts wait for a reload.
    stale: BTreeSet<usize>,
    /// Pages whose load failed since the width last changed.
    failed: BTreeSet<usize>,
    /// Dropped pages a pending copy asked for, kept until it runs.
    pins: Pins,
}

impl Residency {
    /// Appends `part` as a new closed page and marks it resident,
    /// returning its index: what closing a page keeps.
    pub(crate) fn push(&mut self, part: Part) -> usize {
        let at = self.closed.len();
        self.closed.push(Some(part));
        self.resident_set.insert(at);
        at
    }

    /// How many closed pages the conversation holds: the open page's
    /// index.
    pub(crate) fn len(&self) -> usize {
        self.closed.len()
    }

    /// Closed page `at`'s cards, while it holds them.
    pub(crate) fn part(&self, at: usize) -> Option<&Part> {
        self.closed.get(at).and_then(Option::as_ref)
    }

    /// Closed page `at`'s cards to change, while it holds them: `None`
    /// for the open page, which the caller holds itself, and for a
    /// dropped or unknown one.
    pub(crate) fn slot_mut(&mut self, at: usize) -> Option<&mut Part> {
        self.closed.get_mut(at)?.as_mut()
    }

    /// Every resident closed page with its index, in page order.
    pub(crate) fn resident(&self) -> impl Iterator<Item = (usize, &Part)> + '_ {
        self.closed
            .iter()
            .enumerate()
            .filter_map(|(at, part)| part.as_ref().map(|part| (at, part)))
    }

    /// Every resident closed page with its index to change, in page
    /// order.
    pub(crate) fn resident_mut(&mut self) -> impl Iterator<Item = (usize, &mut Part)> + '_ {
        self.closed
            .iter_mut()
            .enumerate()
            .filter_map(|(at, part)| part.as_mut().map(|part| (at, part)))
    }

    /// The cards page `at` starts folding from: its seed's, or `None`
    /// for the open page, which holds the live fold, and for an unknown
    /// one.
    fn begin(&self, at: usize, seeds: &[Seed]) -> Option<Part> {
        if at >= self.closed.len() {
            return None;
        }
        seeds.get(at).cloned().map(Part::seeded)
    }

    /// Finishes page `at`'s folded cards without keeping them: closes
    /// the group the next page ends, applies what the person opened,
    /// and restores the live durations.
    fn finish(
        &self,
        at: usize,
        part: &mut Part,
        seeds: &[Seed],
        overrides: &BTreeMap<Target, bool>,
    ) {
        if seeds.get(at).is_some_and(|seed| seed.cut)
            && let Some(card) = part.turns.last_mut()
        {
            card.end_group();
        }
        if let Some(seed) = seeds.get(at) {
            part.joins_next = seed.joins_next;
        }
        for (target, open) in overrides {
            set(part, target, *open);
        }
        if let Some(seed) = seeds.get(at) {
            restore_spans(part, &seed.spans);
        }
    }

    /// Keeps page `at`'s folded cards, closes the group the next page
    /// ends, applies what the person opened, and clears what the page
    /// waited on; the caller counts it again.
    fn keep(
        &mut self,
        at: usize,
        mut part: Part,
        seeds: &[Seed],
        overrides: &BTreeMap<Target, bool>,
    ) {
        self.finish(at, &mut part, seeds, overrides);
        if let Some(slot) = self.closed.get_mut(at) {
            *slot = Some(part);
            self.resident_set.insert(at);
        }
        self.stale.remove(&at);
        self.failed.remove(&at);
    }

    /// Folds one already-grouped closed page from its borrowed lines and
    /// keeps it: true when a page was kept. The caller notes each
    /// line's images, resolves its page and groups its lines first, and
    /// counts the kept page after.
    pub(crate) fn load(
        &mut self,
        at: usize,
        lines: &[&Envelope],
        seeds: &[Seed],
        overrides: &BTreeMap<Target, bool>,
    ) -> bool {
        let Some(mut part) = self.begin(at, seeds) else {
            return false;
        };
        for line in lines {
            fold(&mut part, line);
        }
        self.keep(at, part, seeds, overrides);
        true
    }

    /// Closed page `at` folded from its seed, kept nowhere: what the
    /// search scans on a dropped page. `None` for the open page, which
    /// holds the live fold, and for an unknown one.
    pub(crate) fn refold(
        &self,
        at: usize,
        lines: &[Envelope],
        seeds: &[Seed],
        overrides: &BTreeMap<Target, bool>,
    ) -> Option<Part> {
        let mut part = self.begin(at, seeds)?;
        for line in lines {
            fold(&mut part, line);
        }
        self.finish(at, &mut part, seeds, overrides);
        Some(part)
    }

    /// The page holding segment `session`'s `seq` could not be loaded:
    /// it is not asked for again until the width changes. An unknown
    /// `seq` changes nothing.
    pub(crate) fn fail(&mut self, session: &SessionId, seq: Seq, index: &Index) {
        if let Some(at) = index.page_of(session, seq) {
            self.failed.insert(at);
        }
    }

    /// The seq ranges to load, in order, each with its segment's
    /// session: the window's pages not resident, then the pages whose
    /// row counts are stale, then the pages a pending copy asked for. A
    /// page may be listed twice; once loaded, it is no longer needed.
    pub(crate) fn needs(
        &self,
        top: usize,
        height: usize,
        index: &Index,
    ) -> Vec<(SessionId, RangeInclusive<Seq>)> {
        let wanted = |at: &usize| {
            !self.failed.contains(at) && self.closed.get(*at).is_some_and(Option::is_none)
        };
        let mut pages: Vec<usize> = index.window(top, height).filter(wanted).collect();
        pages.extend(self.stale.iter().copied().filter(wanted));
        pages.extend(self.pins.pages().filter(wanted));
        pages
            .into_iter()
            .filter_map(|at| index.pages().get(at))
            .map(|page| (page.session.clone(), page.first_seq..=page.last_seq))
            .collect()
    }

    /// Drops the cards of every kept page outside the window that no
    /// pending copy pins, and returns each dropped page's index with
    /// its segment's session, so the caller evicts its entries after
    /// this borrow ends. Only pages holding cards can drop, so the scan
    /// costs residents, never the session.
    pub(crate) fn trim(
        &mut self,
        top: usize,
        height: usize,
        index: &Index,
    ) -> Vec<(usize, SessionId)> {
        let window = index.window(top, height);
        let mut evicted = Vec::new();
        for at in self.resident_set.clone() {
            if !window.contains(&at) && !self.pins.contains(at) {
                if let Some(slot) = self.closed.get_mut(at) {
                    *slot = None;
                }
                // A dropped page's entries go with it: a reload calls
                // its renderer again.
                if let Some(page) = index.pages().get(at) {
                    evicted.push((at, page.session.clone()));
                }
                self.resident_set.remove(&at);
            }
        }
        evicted
    }

    /// Marks a dropped page stale: its rows wait to be counted again
    /// once it loads.
    pub(crate) fn mark_stale(&mut self, at: usize) {
        self.stale.insert(at);
    }

    /// Clears what failed since the last width change: a new width
    /// asks for every dropped page again. The caller recounts next.
    pub(crate) fn on_width_change(&mut self) {
        self.failed.clear();
    }

    /// Splices this residency's first `len` pages into `fold`'s front
    /// and shifts every position-keyed set by the carried count, so the
    /// fold's own entries keep their pages: the closed cards, the held
    /// set with only the carried resident pages, and the stale, failed
    /// and pinned sets.
    pub(crate) fn carry_prefix_into(&self, fold: &mut Residency, len: usize) {
        fold.closed
            .splice(..0, self.closed.iter().take(len).cloned());
        let stale: Vec<usize> = fold.stale.iter().copied().collect();
        fold.stale.clear();
        for at in stale {
            fold.stale.insert(at.saturating_add(len));
        }
        for at in &self.stale {
            if *at < len {
                fold.stale.insert(*at);
            }
        }
        let failed: Vec<usize> = fold.failed.iter().copied().collect();
        fold.failed.clear();
        for at in failed {
            fold.failed.insert(at.saturating_add(len));
        }
        for at in &self.failed {
            if *at < len {
                fold.failed.insert(*at);
            }
        }
        let resident: Vec<usize> = fold.resident_set.iter().copied().collect();
        fold.resident_set.clear();
        for at in resident {
            fold.resident_set.insert(at.saturating_add(len));
        }
        for at in &self.resident_set {
            if *at < len && self.closed.get(*at).is_some_and(Option::is_some) {
                fold.resident_set.insert(*at);
            }
        }
        let pins: Vec<usize> = fold.pins.pages().collect();
        for at in &pins {
            fold.pins.unpin(*at);
        }
        for at in self.pins.pages().filter(|at| *at < len).collect::<Vec<_>>() {
            fold.pins.pin(at);
        }
        for at in pins {
            fold.pins.pin(at.saturating_add(len));
        }
    }

    /// Keeps page `at` resident for one more pending copy.
    pub(crate) fn want(&mut self, at: usize) {
        self.pins.pin(at);
    }

    /// One pending copy lets page `at` go; it drops with the window
    /// once no copy keeps it.
    pub(crate) fn unwant(&mut self, at: usize) {
        self.pins.unpin(at);
    }

    /// How many pins pending copies hold (tests only: what an abandoned
    /// copy must return to).
    #[cfg(test)]
    pub(crate) fn pinned(&self) -> usize {
        self.pins.total()
    }

    /// Whether page `at`'s load failed since the width last changed.
    pub(crate) fn page_failed(&self, at: usize) -> bool {
        self.failed.contains(&at)
    }

    /// Whether a dropped page still waits to be counted at the width:
    /// the pass stays live while one is owed.
    pub(crate) fn stale_owed(&self) -> bool {
        self.stale.iter().any(|at| !self.failed.contains(at))
    }

    /// The dropped pages waiting on a count, to read (tests only: the
    /// pass the session gate holds).
    #[cfg(test)]
    pub(crate) fn stale(&self) -> &BTreeSet<usize> {
        &self.stale
    }

    /// The dropped pages waiting on a count, to seed (tests only: the
    /// pass a recount or a reload settles).
    #[cfg(test)]
    pub(crate) fn stale_mut(&mut self) -> &mut BTreeSet<usize> {
        &mut self.stale
    }

    /// The pages whose load failed, to read (tests only: what a width
    /// change clears).
    #[cfg(test)]
    pub(crate) fn failed(&self) -> &BTreeSet<usize> {
        &self.failed
    }

    /// The pages whose load failed, to seed (tests only: the ask a
    /// reload skips).
    #[cfg(test)]
    pub(crate) fn failed_mut(&mut self) -> &mut BTreeSet<usize> {
        &mut self.failed
    }

    /// The pages holding cards, to read (tests only: what a carried
    /// preview keeps).
    #[cfg(test)]
    pub(crate) fn resident_set(&self) -> &BTreeSet<usize> {
        &self.resident_set
    }

    /// The pages holding cards, to seed (tests only: the fold a carry
    /// shifts).
    #[cfg(test)]
    pub(crate) fn resident_set_mut(&mut self) -> &mut BTreeSet<usize> {
        &mut self.resident_set
    }

    /// The pages a pending copy keeps, to read (tests only: what a
    /// carry moves over).
    #[cfg(test)]
    pub(crate) fn pins(&self) -> &Pins {
        &self.pins
    }

    /// Each closed page's cards, to read (tests only: the slots a fresh
    /// fold draws).
    #[cfg(test)]
    pub(crate) fn slots_for_test(&self) -> &[Option<Part>] {
        &self.closed
    }

    /// Drops page `at`'s cards without touching the held set, as a
    /// direct drop always could (tests only: the fixture a reload
    /// settles).
    #[cfg(test)]
    pub(crate) fn drop_slot_for_test(&mut self, at: usize) {
        if let Some(slot) = self.closed.get_mut(at) {
            *slot = None;
        }
    }

    /// Drops every closed page's cards without touching the held set
    /// (tests only: the window a reload settles).
    #[cfg(test)]
    pub(crate) fn drop_all_for_test(&mut self) {
        self.closed.fill(None);
    }

    /// Drops the first half of the closed pages' cards without touching
    /// the held set (tests only: the share a reload settles).
    #[cfg(test)]
    pub(crate) fn drop_half_for_test(&mut self) {
        let half = self.closed.len() / 2;
        for part in self.closed.iter_mut().take(half) {
            *part = None;
        }
    }
}
