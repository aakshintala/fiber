//! Conversation search (`docs/tui.md`, "Search"): Ctrl+F opens the bar,
//! typing fills its query, and every keystroke schedules the scan after a
//! pause (`docs/tui.md`, "History and paging": it waits for a pause in
//! typing of a quarter of a second). The scan covers the whole session
//! log: resident pages scan at once, and each dropped page is fetched with
//! `history` without waiting, at most one request on the wire, and scanned
//! on its answer. Matches are anchored to their logical line, so a new
//! width, an opened section or live output never invalidates them.

use std::collections::HashMap;
use std::time::Duration;

use super::{App, Effect, Target, history_args, mint, session_command};
use crate::keys::{Edit, Key};
use crate::link::history_answer;
use crate::logical;

mod scan;
mod state;

pub(crate) use scan::Snippet;
use scan::{Anchor, hit_chars, row_offsets};
pub(super) use state::Find;
use state::Match;

/// How long after the last keystroke the scan starts (`docs/tui.md`,
/// "History and paging").
pub(in crate::app) const FIND_PAUSE: Duration = Duration::from_millis(250);

/// One `history` answer's lines at most (`docs/invocation.md`, "Driver
/// commands").
const HISTORY_LINES: u64 = 256;

/// What a lost connection fails the scan with.
const LOST: &str = "connection lost";

/// The search-incomplete notice's identity: the text `find_lost` pushes.
/// Reconnect keeps this text standing; the bar closing or a completed
/// rescan clears it.
pub(super) fn search_incomplete_text() -> String {
    format!("Could not search all of history: {LOST}")
}

/// The search bar as drawn: its query and its match count.
#[derive(Debug)]
pub(crate) struct FindBar {
    /// What was typed into the bar.
    pub(crate) query: String,
    /// The count beside it, as drawn: `3 of 41`, `3 of 41…` while
    /// scanning, `no matches`, `10000+` past the cap, and ` · incomplete`
    /// when a page could not be read (`docs/tui.md`, "Search").
    pub(crate) count: String,
}

impl App {
    /// A key for the search bar, right after the Ctrl+R panel in
    /// [`App::route_key`], so an approval's typing and Esc, the offer's
    /// keys and the Ctrl+R panel's reach their panels first (`docs/tui.md`,
    /// "Search": search is Ctrl+F, and Cmd+F where the terminal forwards
    /// it). `None` when the bar is closed and the key is not Ctrl+F, and
    /// for the keys the bar leaves to the handlers below it.
    pub(in crate::app) fn find_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.find.is_open() {
            if *key != Key::CtrlF {
                return None;
            }
            // While an item view is open search opens nothing: it scans
            // the attached session's history.
            if self.item_open() {
                return Some(Effect::None);
            }
            // Home draws no conversation, and over the offer the offer's
            // own keys win; both leave Ctrl+F unhandled above, so the bar
            // opens only on a session's conversation.
            if self.session().is_none() || self.home_screen().is_some() {
                return None;
            }
            self.find.open();
            return Some(Effect::None);
        }
        match key {
            Key::Char(ch) => {
                let mut text = [0; 4];
                Some(self.find.type_query(ch.encode_utf8(&mut text)))
            }
            Key::Backspace if !self.find.query().is_empty() => Some(self.find.pop_query()),
            Key::Esc => {
                self.find.close();
                Some(Effect::None)
            }
            // A second Ctrl+F opens the results view; until then the
            // bar stays open. While the call's view is open the results
            // stay closed, so matches never leave the view.
            Key::CtrlF => {
                if self.call_view_open() {
                    return Some(Effect::None);
                }
                self.open_results();
                Some(Effect::None)
            }
            // The view's keys scroll on with the bar open.
            Key::PageUp | Key::PageDown => None,
            Key::Backspace
            | Key::End
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlC
            | Key::CtrlO
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::AltA
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => Some(Effect::None),
            // Between matches, wrapping at either end (`docs/tui.md`,
            // "Search").
            Key::Enter | Key::Down => {
                self.find.step(true);
                Some(Effect::None)
            }
            Key::Up => {
                self.find.step(false);
                Some(Effect::None)
            }
        }
    }

    /// An editing key for the search bar: a paste joins its lines, and
    /// Shift+Enter moves to the previous match once matches land.
    /// `Some(Effect::FindPause { .. })` for a paste that changed the
    /// query, `Some(Effect::None)` for any other edit while the bar holds
    /// the keyboard, `None` when the bar is closed or a panel above it is
    /// open, so an approval's paste reaches its feedback and the Ctrl+R
    /// panel keeps its own (`docs/tui.md`, "Search").
    pub(in crate::app) fn find_edit(&mut self, edit: &Edit) -> Option<Effect> {
        if !self.find.is_open() || self.panel().is_some() || self.search_panel().is_some() {
            return None;
        }
        if let Edit::Paste(text) = edit {
            let joined: String = text
                .chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect();
            if joined.is_empty() {
                return Some(Effect::None);
            }
            return Some(self.find.type_query(&joined));
        }
        if *edit == Edit::ShiftEnter {
            // While the call's view is open the step moves inside it.
            if self.call_view_open() {
                self.step_call_view_match(&Key::Up);
                return Some(Effect::None);
            }
            self.find.step(false);
            return Some(Effect::None);
        }
        Some(Effect::None)
    }

    /// The bar as drawn, while it is open. While the call's view is
    /// open the count names the view's matches, never the
    /// conversation's.
    pub(crate) fn find_bar(&self) -> Option<FindBar> {
        if !self.find.is_open() {
            return None;
        }
        if let Some(count) = self.call_view_count() {
            return Some(FindBar {
                query: self.find.query().to_owned(),
                count,
            });
        }
        Some(FindBar {
            query: self.find.query().to_owned(),
            count: self.find.count(self.screen.pages()),
        })
    }

    /// Whether the search bar is open: what `key_context` reads, without
    /// building the drawn count `find_bar` computes on every key.
    pub(in crate::app) fn find_open(&self) -> bool {
        self.find.is_open()
    }

    /// The match marks on screen, each with whether it is the current
    /// match: every occurrence of the query in every shown logical line
    /// as drawn, the current one brighter (`docs/tui.md`, "Search").
    /// With no query, no scan or no matches this costs one check
    /// (`docs/tui.md`, "Performance").
    pub(crate) fn find_marks(
        &self,
        area: ratatui::layout::Rect,
    ) -> Vec<(ratatui::layout::Rect, bool)> {
        // While the call's view is open its frame paints the view's own
        // scoped marks; the hidden conversation rows would map to wrong
        // cells over it, so no marks come from here.
        if self.call_view_open() {
            return Vec::new();
        }
        if !self.find.is_open()
            || !self.find.due()
            || self.find.query().is_empty()
            || self.find.total() == 0
        {
            return Vec::new();
        }
        let pages = self.screen.pages();
        let width = pages.wrap_width();
        let (top, y0, _) = self.view_rows(area);
        let height = usize::from(area.height);
        let mut out = Vec::new();
        for at in 0..pages.page_count() {
            let Some((rows, texts)) = pages.page_text(at) else {
                continue;
            };
            let start = pages.index().start(at);
            if start >= top.saturating_add(height) {
                break;
            }
            let offsets = row_offsets(&rows, width);
            let end = start.saturating_add(offsets.last().copied().unwrap_or(0));
            if end < top {
                continue;
            }
            // The placed cells of the rows holding matches, each placed
            // once however many of its chars match.
            let mut placed: HashMap<usize, Vec<crate::cells::Placed>> = HashMap::new();
            // Which of the page's matches with equal scopes each hit
            // is, in order: what locates the current match on screen.
            let mut seen: HashMap<Vec<Target>, usize> = HashMap::new();
            for line in logical::logical(&rows, &texts) {
                for hit in logical::matches(&line.text, self.find.query()) {
                    let anchor = Anchor::new(at, &line, hit.clone());
                    let nth = seen.get(&anchor.scopes).copied().unwrap_or(0);
                    seen.insert(anchor.scopes.clone(), nth.saturating_add(1));
                    let current = self
                        .find
                        .current()
                        .is_some_and(|c| c.anchor == anchor && c.nth == nth);
                    for (row, byte) in hit_chars(&line, &hit) {
                        let cells = placed.entry(row).or_insert_with(|| {
                            rows.get(row)
                                .map_or(Vec::new(), |drawn| crate::cells::place(&drawn.0, width))
                        });
                        let next = cells.partition_point(|cell| cell.bytes.end <= byte);
                        let Some(cell) = cells.get(next) else {
                            continue;
                        };
                        if cell.bytes.start > byte {
                            continue;
                        }
                        // A row above the viewport draws nothing: it
                        // converts to no screen row (`docs/tui.md`,
                        // "Search": marks follow whatever is drawn).
                        let absolute = start
                            .saturating_add(offsets.get(row).copied().unwrap_or(0))
                            .saturating_add(usize::from(cell.row));
                        if absolute < top {
                            continue;
                        }
                        let y = y0 as usize + absolute.saturating_sub(top);
                        let x = area.x.saturating_add(cell.col);
                        if y < usize::from(area.bottom()) && x < area.right() {
                            out.push((
                                ratatui::layout::Rect::new(
                                    x,
                                    u16::try_from(y).unwrap_or(u16::MAX),
                                    cell.width,
                                    1,
                                ),
                                current,
                            ));
                        }
                    }
                }
            }
        }
        out
    }

    /// The pause after `generation`'s keystroke passed: resident pages
    /// scan at once, and the first dropped page is fetched. A generation
    /// but the current one, a closed bar and an empty query start nothing.
    pub(crate) fn find_due(&mut self, generation: u64) -> Vec<String> {
        if generation != self.find.generation()
            || !self.find.is_open()
            || self.find.query().is_empty()
        {
            return Vec::new();
        }
        // While the call's view is open the scan stays inside it: no
        // conversation page reads, and the view rebuilds its matches.
        if self.call_view_open() {
            self.refresh_call_view_matches();
            return Vec::new();
        }
        if self.find.due() {
            return self.find_outgoing();
        }
        let pages = self.screen.pages();
        let (top, _) = self.scroll();
        let start = pages.index().locate(top).map_or(0, |(at, _)| at);
        let count = pages.page_count();
        let clock = pages.index().clock();
        let session = self.session().cloned();
        self.find.start(start, count, clock, session);
        // Scans resident pages, fetches the first dropped one, and
        // reveals the current match in the same step: its sections open
        // and the view scrolls to it, and a dropped page it sits on
        // loads with the frame's other pages.
        self.settle_find();
        self.find_outgoing()
    }

    /// Scans every reached-but-unscanned resident page and every
    /// dirtied one, in scan order, advancing the reach past settled
    /// positions. New pages join behind the order; changed pages rescan
    /// without starving the pages the reach still owes. Whether any page
    /// scanned.
    fn scan_resident_all(&mut self) -> bool {
        let count = self.screen.pages().page_count();
        self.find.extend_order(count);
        // Text that moved since the last verification soils its pages
        // for a rescan; with no moves this costs nothing.
        self.find.verify(self.screen.pages());
        let mut rescanned = false;
        // Dirtied positions first: they stand earlier in scan order than
        // the reach, and their rescan never starves it. A position
        // settled another way (its answer landed first) just cleans.
        for pos in self.find.dirty().clone() {
            crate::work::add(|work| work.search_order_pages += 1);
            let Some(at) = self.find.page_at(pos) else {
                self.find.clean(pos);
                continue;
            };
            let revision = self.screen.pages().index().revision(at);
            if self.find.scanned(at) == Some(revision) {
                self.find.clean(pos);
                continue;
            }
            if self.screen.pages().part(at).is_some() {
                self.scan_resident(at);
                self.find.clean(pos);
                rescanned = true;
            }
        }
        // New positions in scan order: resident ones scan, and the
        // reach stops at the first dropped one, which the pump fetches.
        // Every pass settles exactly one position past the reach, so
        // the loop variable tracks it with no comparison left to mutate.
        for pos in self.find.cursor()..count {
            let Some(at) = self.find.page_at(pos) else {
                break;
            };
            crate::work::add(|work| work.search_order_pages += 1);
            let revision = self.screen.pages().index().revision(at);
            if self.find.scanned(at) == Some(revision) {
                self.find.reach(pos.saturating_add(1));
                continue;
            }
            if self.screen.pages().part(at).is_some() {
                self.scan_resident(at);
                self.find.reach(pos.saturating_add(1));
                rescanned = true;
            } else {
                break;
            }
        }
        rescanned
    }

    /// Scans resident page `at` for the current query, recording its
    /// matches whole and picking the current match when it can become it.
    fn scan_resident(&mut self, at: usize) {
        let revision = self.screen.pages().index().revision(at);
        if self.find.scanned(at) == Some(revision) {
            return;
        }
        let query = self.find.query().to_owned();
        let Some(found) = scan::resident(self.screen.pages(), at, &query) else {
            return;
        };
        crate::work::add(|work| work.search_pages += 1);
        let kept = self.find.record(at, revision, found);
        let (top, _) = self.scroll();
        self.find.consider(at, &kept, top);
    }

    /// Scans the lines folded for dropped page `at`, recording its matches
    /// whole and picking the current match when it can become it.
    fn scan_scratch(
        &mut self,
        at: usize,
        revision: u64,
        rows: &[crate::turn::Row],
        texts: &[crate::rows::RowText],
        turns: &[(usize, std::ops::Range<usize>)],
        paths: &crate::window::CallPaths,
    ) {
        crate::work::add(|work| work.search_pages += 1);
        let query = self.find.query().to_owned();
        let found = scan::scratch(self.screen.pages(), at, rows, texts, turns, paths, &query);
        let kept = self.find.record(at, revision, found);
        let (top, _) = self.scroll();
        self.find.consider(at, &kept, top);
    }

    /// Fetches the next dropped page the scan has not read, if any, then
    /// falls back to the first match in page order when the scan found
    /// none at or after the top row anywhere after it. The next page in
    /// scan order wins: a dirtied page beats the reach's, and the
    /// reach's beats every page behind it. At most one request is ever
    /// on the wire, whatever the generation.
    fn pump_find(&mut self) {
        if !self.find.due() {
            return;
        }
        // While the call's view is open nothing fetches for the
        // conversation's scan: its matches would leak outside the view.
        if self.call_view_open() {
            return;
        }
        let count = self.screen.pages().page_count();
        self.find.extend_order(count);
        if !self.find.capped() && !self.find.fetching() {
            crate::work::add(|work| work.search_order_pages += 1);
            // Past positions already settled: the reach never looks back.
            self.find.skip_settled(self.screen.pages());
            let mut next = None;
            // The reach's page, unless a dirtied one stands earlier.
            if self.find.cursor() < count {
                let Some(at) = self.find.page_at(self.find.cursor()) else {
                    return;
                };
                let revision = self.screen.pages().index().revision(at);
                if self.find.scanned(at) != Some(revision) && self.screen.pages().part(at).is_none()
                {
                    next = Some(at);
                }
            }
            // Dirtied positions before the reach stand earlier in scan
            // order, so the first dropped one still waiting wins over
            // the reach: each entry examined once, never the whole order.
            for pos in self
                .find
                .dirty()
                .range(..self.find.cursor())
                .cloned()
                .collect::<Vec<_>>()
            {
                crate::work::add(|work| work.search_order_pages += 1);
                let Some(at) = self.find.page_at(pos) else {
                    continue;
                };
                let revision = self.screen.pages().index().revision(at);
                if self.find.scanned(at) == Some(revision) || self.screen.pages().part(at).is_some()
                {
                    continue;
                }
                next = Some(at);
                break;
            }
            if let Some(at) = next {
                self.fetch_page(at, None, Vec::new());
            }
        }
        if !self.find.fetching()
            && self.find.current().is_none()
            && self.find.settled(self.screen.pages())
            && let Some(first) = self.find.match_at(0)
        {
            self.find.set_current(first);
        }
    }

    /// Asks for `page`'s lines from `from`, or from its first line: one
    /// `history` command of at most [`HISTORY_LINES`] lines. Captures the
    /// layout the fetch reads under, so its answer cannot stamp another.
    fn fetch_page(&mut self, page: usize, from: Option<u64>, held: Vec<contract::Envelope>) {
        if self.pass_open() {
            return;
        }
        let (Some(session), true) = (self.session().cloned(), self.outbox.is_up()) else {
            return;
        };
        let revision = self.screen.pages().index().revision(page);
        let width = self.screen.pages().wrap_width();
        let Some(entry) = self.screen.pages().index().pages().get(page) else {
            return;
        };
        let (first, last, lines) = (entry.first_seq.0, entry.last_seq.0, entry.lines);
        if lines == 0 {
            self.find.record(page, revision, Vec::new());
            return;
        }
        let from = from.unwrap_or(first);
        if from > last {
            self.find.record(page, revision, Vec::new());
            return;
        }
        let to = from.saturating_add(HISTORY_LINES - 1).min(last);
        let id = mint();
        // An ancestor page's window rides the serving session's chain
        // read: the command stays routed to the serving session, and
        // the args name the page's segment.
        let args = history_args(&session, &entry.session, Some(from), Some(to));
        let line = session_command(&id, "history", &session, Some(args)).to_string();
        self.find
            .send(id, page, entry.session.clone(), width, revision, held, line);
    }

    /// The search's unsent command lines.
    pub(in crate::app) fn find_outgoing(&mut self) -> Vec<String> {
        self.find.outgoing()
    }

    /// A session line that may answer the search's fetch: it folds into
    /// the scan, never into the pages, and the pump's next lines go out.
    /// `None` for any other line.
    pub(in crate::app) fn find_answered(
        &mut self,
        envelope: &contract::Envelope,
    ) -> Option<Vec<String>> {
        let session = self.session().cloned()?;
        if !self.find.answers(envelope, &session) {
            return None;
        }
        let mut fetch = self.find.take_fetch()?;
        // The fetch's answer settles its kept line, which no fold path
        // pops a pending entry for.
        self.outbox.answered(&fetch.id);
        if fetch.generation != self.find.generation() {
            // A query change while it was in flight sends nothing: the
            // fetch stays recorded no more, its lines fold nowhere, and
            // the pump runs for the current generation.
            self.pump_find();
            return Some(self.find_outgoing());
        }
        match history_answer(envelope) {
            Err(message) => {
                // The page stays unscanned no more: it scans again when
                // its revision moves, and the count gains ` · incomplete`.
                let revision = self.screen.pages().index().revision(fetch.page);
                self.find.unreadable(fetch.page, revision);
                self.notices.push(
                    "io_failed",
                    format!("Could not search all of history: {message}"),
                );
            }
            Ok(lines) => {
                debug_assert!(
                    lines.iter().all(|line| line.session_id == fetch.session),
                    "the fetch answers under its page's segment session"
                );
                fetch.held.extend(lines.iter().cloned());
                let last = fetch.held.last().and_then(|line| line.seq).map(|seq| seq.0);
                let end = self
                    .screen
                    .pages()
                    .index()
                    .pages()
                    .get(fetch.page)
                    .map(|page| page.last_seq.0);
                if self.screen.pages().part(fetch.page).is_some() {
                    self.scan_resident(fetch.page);
                } else {
                    let revision = self.screen.pages().index().revision(fetch.page);
                    let done = fetch.held.is_empty()
                        || last.is_some_and(|last| end.is_some_and(|end| last >= end));
                    if done {
                        // The fetch read under its captured layout: lines
                        // read before a text change, or a table read at
                        // another width, requeue for the current layout
                        // instead of stamping the new revision with old
                        // results. Anything else folds whole and scans at
                        // the current width, as before.
                        let width = self.screen.pages().wrap_width();
                        let obsolete = fetch.revision != revision
                            || (fetch.width != width && self.screen.pages().page_table(fetch.page));
                        if obsolete {
                            self.find.requeue(fetch.page);
                        } else {
                            // The page folds whole, so a chunk never scans
                            // without the lines that open its turns.
                            match self.screen.pages().fold_text_turns(fetch.page, &fetch.held) {
                                Some((rows, texts, turns, paths)) => {
                                    self.scan_scratch(
                                        fetch.page, revision, &rows, &texts, &turns, &paths,
                                    );
                                }
                                None => {
                                    self.find.record(fetch.page, revision, Vec::new());
                                }
                            }
                        }
                    } else {
                        self.fetch_page(
                            fetch.page,
                            last.map(|last| last.saturating_add(1)),
                            fetch.held,
                        );
                    }
                }
            }
        }
        self.pump_find();
        Some(self.find_outgoing())
    }

    /// The link went down: the fetch clears, and a running scan is
    /// incomplete with the notice that says why.
    pub(in crate::app) fn find_lost(&mut self) {
        if self.find.lose() {
            self.notices
                .push("connection_failed", search_incomplete_text());
        }
    }

    /// Runs after every settle while the bar is open: rescans the pages
    /// whose revision moved, reconciles the current match, fetches what
    /// dropped, and reveals the current match.
    pub(in crate::app) fn settle_find(&mut self) {
        if !self.find.due() {
            return;
        }
        // While the call's view is open the scan stays inside it: its
        // matches rebuild, and the conversation neither scans nor
        // fetches nor reveals.
        if self.call_view_open() {
            self.refresh_call_view_matches();
            return;
        }
        // Another session since the scan started resets the search:
        // its pages and revisions answer for no one else.
        if self.find.session_id().as_ref() != self.session() {
            self.close_find();
            return;
        }
        let old = self.find.current().cloned();
        let old_selected = self.find.selected_match();
        // A dropped page answers into the scratch scan, never into the
        // resident set, so the rescan completing need not rescan
        // anything here: the reach walking the whole order is the
        // completion, wherever each position scanned.
        if self.scan_resident_all() {
            if let Some(old) = old {
                self.find.reconcile(old);
            }
            if let Some(old) = old_selected {
                let height = self.results_height();
                self.find.remap_selected(old, height);
            }
        }
        if !self.find.fetching() && self.find.cursor() >= self.screen.pages().page_count() {
            // The scan is whole again with nothing on the wire: every
            // position scanned at its revision, so the search-incomplete
            // text goes. A fetch still out, or a position still
            // unscanned, leaves it standing.
            self.notices
                .clear_code("connection_failed", &search_incomplete_text());
        }
        self.pump_find();
        self.reveal_current();
    }

    /// Reveals the current match: its closed sections open, outermost
    /// first, clearing the selection, and the view scrolls to it. On a
    /// dropped page the view scrolls so it loads, and the reveal completes
    /// after.
    pub(super) fn reveal_current(&mut self) {
        if !self.find.revealing() {
            return;
        }
        let Some(current) = self.find.current().cloned() else {
            self.find.revealed();
            return;
        };
        if self.screen.pages().part(current.anchor.page).is_none() {
            // The page loads through `page_in` as every scroll does; the
            // reveal completes once it is resident. No settle here: one
            // runs after every input already.
            self.screen
                .jump(self.screen.pages().index().start(current.anchor.page));
            return;
        }
        let mut expanded = false;
        for scope in &current.anchor.scopes {
            if self.screen.force_open(scope) {
                expanded = true;
            }
        }
        if expanded {
            self.clear_selection();
            // The rows moved, but no match did: the page scans again
            // only when its text moves next.
            let revision = self.screen.pages().index().revision(current.anchor.page);
            self.find.mark_scanned(current.anchor.page, revision);
        }
        let Some((row, _)) = self.current_place(&current) else {
            self.find.revealed();
            return;
        };
        let (top, _) = self.scroll();
        let height = self.conversation_height();
        let behind_pin = self.screen.pin(height).is_some() && row == top;
        if !(top..top.saturating_add(height)).contains(&row) || behind_pin {
            self.screen.land(row);
        }
        self.find.revealed();
    }

    /// The current match's conversation row and whether a closed section
    /// hides it, on its resident page.
    fn current_place(&self, current: &Match) -> Option<(usize, bool)> {
        let at = current.anchor.page;
        let query = self.find.query().to_owned();
        let found = scan::resident(self.screen.pages(), at, &query)?;
        // in order: what locates the current match on screen.
        let mut seen: HashMap<Vec<Target>, usize> = HashMap::new();
        for (anchor, row, hidden, _) in found {
            let nth = seen.get(&anchor.scopes).copied().unwrap_or(0);
            seen.insert(anchor.scopes.clone(), nth.saturating_add(1));
            if current.anchor == anchor && current.nth == nth {
                return Some((row, hidden));
            }
        }
        None
    }

    /// Closes the bar, going home. The search it ran ends with it: its
    /// incomplete text goes, while any connection text stands until the
    /// reconnect ends that cause.
    pub(super) fn close_find(&mut self) {
        self.notices
            .clear_code("connection_failed", &search_incomplete_text());
        self.find.close();
    }

    /// How many search matches are kept (the paging jig's report).
    pub(crate) fn find_matches(&self) -> usize {
        self.find.total()
    }
}

#[cfg(test)]
#[path = "find_tests.rs"]
mod tests;
