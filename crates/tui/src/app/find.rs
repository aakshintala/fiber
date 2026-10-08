//! Conversation search (`docs/tui.md`, "Search"): Ctrl+F opens the bar,
//! typing fills its query, and every keystroke schedules the scan after a
//! pause (`docs/tui.md`, "History and paging": it waits for a pause in
//! typing of a quarter of a second). The scan covers the whole session
//! log: resident pages scan at once, and each dropped page is fetched with
//! `history` without waiting, at most one request on the wire, and scanned
//! on its answer. Matches are anchored to their logical line, so a new
//! width, an opened section or live output never invalidates them.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use super::{App, Effect, Link, Target, mint, session_command};
use crate::keys::{Edit, Key};
use crate::link::history_answer;
use crate::logical;
use crate::window::Pages;

mod scan;

pub(crate) use scan::Snippet;
use scan::{Anchor, hit_chars, row_offsets};

/// How long after the last keystroke the scan starts (`docs/tui.md`,
/// "History and paging").
pub(in crate::app) const FIND_PAUSE: Duration = Duration::from_millis(250);

/// How many matches a search keeps: past it the scan stops and the count
/// shows `10000+` (`docs/tui.md`, "Search").
pub(crate) const MAX_MATCHES: usize = 10_000;

/// One `history` answer's lines at most (`docs/invocation.md`, "Driver
/// commands").
const HISTORY_LINES: u64 = 256;

/// What a lost connection fails the scan with.
const LOST: &str = "connection lost";

/// The search bar as drawn: its query and its match count.
pub(crate) struct FindBar {
    /// What was typed into the bar.
    pub(crate) query: String,
    /// The count beside it, as drawn: `3 of 41`, `3 of 41…` while
    /// scanning, `no matches`, `10000+` past the cap, and ` · incomplete`
    /// when a page could not be read (`docs/tui.md`, "Search").
    pub(crate) count: String,
}

/// One match: its identity, and which of its page's matches with equal
/// scopes it is, in order, which locates it on screen.
#[derive(Debug, Clone)]
pub(crate) struct Match {
    /// Its identity.
    pub(crate) anchor: Anchor,
    /// Its ordinal among the page's matches with equal scopes.
    pub(crate) nth: usize,
    /// Its display lines for the results view.
    pub(crate) snippet: Snippet,
}

impl PartialEq for Match {
    /// Occurrence identity is the anchor and the ordinal: the display
    /// snippet never compares (`docs/tui.md`, "Search").
    fn eq(&self, other: &Self) -> bool {
        self.anchor == other.anchor && self.nth == other.nth
    }
}

impl Eq for Match {}

/// The search's one `history` request on the wire, if any.
#[derive(Debug)]
struct Fetch {
    /// The command's id: only its answer folds into the scan.
    id: String,
    /// The page it reads.
    page: usize,
    /// The generation it scans for: older than the current one is stale,
    /// and its answer folds nothing.
    generation: u64,
    /// The page's lines read so far: a page over one answer folds whole
    /// once its last chunk arrives, so a chunk never scans without the
    /// lines that open its turns.
    held: Vec<contract::Envelope>,
}

/// The search's state.
#[derive(Debug, Default)]
pub(super) struct Find {
    /// Whether the bar is open.
    open: bool,
    /// What was typed into the bar.
    query: String,
    /// Bumped on every query change, never reset; a pause or an answer
    /// tagged with another is stale.
    generation: u64,
    /// Whether the pause passed for the current generation and the scan
    /// started.
    due: bool,
    /// The one `history` request on the wire, if any.
    fetch: Option<Fetch>,
    /// Command lines the pump made that no one sent yet.
    pending_out: Vec<String>,
    /// The matches kept, by page, each page's list in render order.
    matches: BTreeMap<usize, Vec<Match>>,
    /// Each scanned page's revision then: a page scans again when its
    /// revision moved, and a page never scanned scans too.
    scanned: HashMap<usize, u64>,
    /// How many matches are kept, capped at [`MAX_MATCHES`].
    total: usize,
    /// The cap stopped the scan.
    capped: bool,
    /// The current match.
    current: Option<Match>,
    /// The current match is not shown yet: its sections open and the view
    /// scrolls to it.
    reveal: bool,
    /// The results view, while open: the bar stays open under it
    /// (`docs/tui.md`, "Search").
    results: Option<super::results::Results>,
    /// A page could not be read: the count gains ` · incomplete`.
    incomplete: bool,
    /// The page on screen when the scan started: the scan runs from it to
    /// the end, then from page 0 back to it.
    start_page: usize,
    /// The pages in scan order.
    order: Vec<usize>,
}

impl Find {
    /// Closes the bar and drops the whole scan.
    fn close(&mut self) {
        *self = Self::default();
    }

    /// A new query: the generation bumps, and the marks and the count
    /// clear; a fetch on the wire stays recorded and is now stale, so no
    /// new request goes out for it, and its unsent lines go.
    fn pause(&mut self) -> Effect {
        self.generation = self.generation.saturating_add(1);
        let fetch = self.fetch.take();
        let open = self.open;
        let query = std::mem::take(&mut self.query);
        *self = Self {
            open,
            query,
            generation: self.generation,
            fetch,
            ..Self::default()
        };
        Effect::FindPause {
            generation: self.generation,
            after: FIND_PAUSE,
        }
    }

    /// Whether `envelope` answers the fetch: an accepted or rejected
    /// answer with its command id, for the attached session.
    fn answers(&self, envelope: &contract::Envelope, session: &contract::SessionId) -> bool {
        let Some(fetch) = &self.fetch else {
            return false;
        };
        matches!(
            envelope.kind.as_str(),
            "command_accepted" | "command_rejected"
        ) && envelope.session_id == *session
            && envelope
                .payload
                .get("command_id")
                .and_then(serde_json::Value::as_str)
                == Some(fetch.id.as_str())
    }

    /// Records page `at`'s matches, whole, in render order, stopping at
    /// the cap: the flattened order stays page order, then render order.
    /// Returns the kept matches with their rows.
    fn record(
        &mut self,
        at: usize,
        revision: u64,
        found: Vec<(Anchor, usize, bool, Snippet)>,
    ) -> Vec<(Match, usize, bool)> {
        // A rescan replaces the page's list whole: its old matches go
        // first, so the flattened order stays page order, then render
        // order, and the total counts every kept match once.
        let old = self.matches.get(&at).map(Vec::len).unwrap_or(0);
        self.total = self.total.saturating_sub(old);
        let left = MAX_MATCHES.saturating_sub(self.total);
        if found.len() > left {
            self.capped = true;
        }
        let mut matches = Vec::new();
        let mut kept = Vec::new();
        for (anchor, row, hidden, snippet) in found.into_iter().take(left) {
            let nth = matches
                .iter()
                .filter(|kept: &&Match| kept.anchor.scopes == anchor.scopes)
                .count();
            let kept_match = Match {
                anchor,
                nth,
                snippet,
            };
            kept.push((kept_match.clone(), row, hidden));
            matches.push(kept_match);
        }
        self.total = self.total.saturating_add(matches.len());
        self.matches.insert(at, matches);
        self.scanned.insert(at, revision);
        kept
    }

    /// Picks the current match from page `at`'s freshly kept matches, if
    /// none is current yet: the first at or after the top row on the page
    /// on screen, the first anywhere past it. Later finds never move it.
    fn consider(&mut self, at: usize, found: &[(Match, usize, bool)], start: usize, top: usize) {
        if self.current.is_some() {
            return;
        }
        if let Some((kept, _, _)) = found.iter().find(|(_, row, _)| at != start || *row >= top) {
            self.current = Some(kept.clone());
            self.reveal = true;
        }
    }

    /// Whether any page still wants scanning: never scanned, or scanned
    /// at an older revision. A scanned page keeps its matches when it
    /// drops; its revision moves only when its text does.
    fn wants(&self, pages: &Pages) -> bool {
        (0..pages.page_count()).any(|at| self.scanned.get(&at) != Some(&pages.index().revision(at)))
    }

    /// Whether the count shows it is still scanning: a fetch is on the
    /// wire, or a page still wants scanning and the cap did not stop it.
    fn scanning(&self, pages: &Pages) -> bool {
        self.fetch.is_some() || (!self.capped && self.wants(pages))
    }

    /// The kept matches in page order, then render order.
    pub(super) fn flat(&self) -> Vec<&Match> {
        self.matches.values().flatten().collect()
    }

    /// The current match's place in page order, then render order.
    pub(super) fn current_index(&self) -> Option<usize> {
        let current = self.current.as_ref()?;
        self.flat().iter().position(|kept| *kept == current)
    }

    /// The kept match at `at` in page order, then render order.
    pub(super) fn match_at(&self, at: usize) -> Option<Match> {
        self.flat().get(at).cloned().cloned()
    }

    /// The results view's selected entry's match, if any.
    fn selected_match(&self) -> Option<Match> {
        let selected = self.results.as_ref()?.selected;
        self.match_at(selected)
    }

    /// Shows the results view, selecting the current match's entry or
    /// the first one (`docs/tui.md`, "Search").
    pub(super) fn show_results(&mut self, selected: usize) {
        self.results = Some(super::results::Results {
            selected,
            top: selected,
        });
    }

    /// Closes the results view; the bar stays open.
    pub(super) fn hide_results(&mut self) {
        self.results = None;
    }

    /// Whether the results view is open.
    pub(super) fn has_results(&self) -> bool {
        self.results.is_some()
    }

    /// What was typed into the bar.
    pub(super) fn query(&self) -> &str {
        &self.query
    }

    /// The results view, for moving its selection.
    pub(super) fn results_mut(&mut self) -> Option<&mut super::results::Results> {
        self.results.as_mut()
    }

    /// The results view's selected entry and its top row.
    pub(super) fn results_at(&self) -> Option<(usize, usize)> {
        self.results
            .as_ref()
            .map(|results| (results.selected, results.top))
    }

    /// Makes `next` current and shows it: its sections open and the view
    /// scrolls to it (`docs/tui.md`, "Search").
    pub(super) fn set_current(&mut self, next: Match) {
        self.current = Some(next);
        self.reveal = true;
    }

    /// Pulls pages cut since the query started into the scan order
    /// behind the pages already in it: a page never scanned scans too
    /// (`docs/tui.md`, "Search": search covers the whole session
    /// log).
    fn sync_order(&mut self, page_count: usize) {
        for at in 0..page_count {
            if !self.order.contains(&at) {
                self.order.push(at);
            }
        }
    }

    /// Reconciles the current match with rescanned pages: an equal anchor
    /// stays current and keeps a pending reveal, else the first match
    /// after the old key's place in page order, wrapping, else none, and
    /// nothing is revealed (`docs/tui.md`, "Search").
    fn reconcile(&mut self, old: Match) {
        let flat = self.flat();
        let next = remapped(&flat, &old).and_then(|at| flat.get(at).cloned().cloned());
        let equal = next.as_ref().is_some_and(|got| got.anchor == old.anchor);
        self.current = next;
        if !equal {
            self.reveal = false;
        }
    }

    /// Reconciles the results view's selected entry with rescanned pages
    /// the same way: the same occurrence stays selected, else the
    /// first match after the old one's place in page order, wrapping,
    /// else the first one, and none with no matches (`docs/tui.md`,
    /// "Search").
    fn remap_selected(&mut self, old: Match, height: usize) {
        let flat = self.flat();
        let at = remapped(&flat, &old).unwrap_or(0);
        let len = flat.len();
        if let Some(results) = self.results.as_mut() {
            results.go(at, len, height);
        }
    }

    /// The count beside the query, as drawn.
    fn count(&self, pages: &Pages) -> String {
        // A new query clears the count until its pause passes.
        if self.query.is_empty() || !self.due {
            return String::new();
        }
        let mut count = if self.total == 0 {
            if self.scanning(pages) {
                "…".to_owned()
            } else {
                "no matches".to_owned()
            }
        } else {
            let flat = self.flat();
            let at = self
                .current
                .as_ref()
                .and_then(|current| flat.iter().position(|kept| *kept == current))
                .map_or(flat.len(), |at| at.saturating_add(1));
            let total = if self.capped {
                "10000+".to_owned()
            } else {
                self.total.to_string()
            };
            format!("{at} of {total}")
        };
        if self.total > 0 && self.scanning(pages) {
            count.push('…');
        }
        if self.incomplete {
            count.push_str(" · incomplete");
        }
        count
    }
}

/// The kept match for `old` among rescanned pages, by index: the equal
/// anchor whose ordinal is nearest the old one, else the first match
/// after the old key's place in page order, wrapping, else the first
/// one, and none with no matches. The anchor matches independently of
/// the ordinal: `nth` only locates the match on screen, so matches
/// appearing before it shift it without losing the occurrence
/// (`docs/tui.md`, "Search").
fn remapped(flat: &[&Match], old: &Match) -> Option<usize> {
    if let Some((at, _)) = flat
        .iter()
        .enumerate()
        .filter(|(_, kept)| kept.anchor == old.anchor)
        .min_by_key(|(_, kept)| kept.nth.abs_diff(old.nth))
    {
        return Some(at);
    }
    flat.iter()
        .position(|kept| (kept.anchor.page, kept.nth) > (old.anchor.page, old.nth))
        .or_else(|| flat.first().map(|_| 0))
}

impl App {
    /// A key for the search bar, right after the Ctrl+R panel in
    /// [`App::route_key`], so an approval's typing and Esc, the offer's
    /// keys and the Ctrl+R panel's reach their panels first (`docs/tui.md`,
    /// "Search": search is Ctrl+F, and Cmd+F where the terminal forwards
    /// it). `None` when the bar is closed and the key is not Ctrl+F, and
    /// for the keys the bar leaves to the handlers below it.
    pub(in crate::app) fn find_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.find.open {
            if *key != Key::CtrlF {
                return None;
            }
            // Home draws no conversation, and over the offer the offer's
            // own keys win; both leave Ctrl+F unhandled above, so the bar
            // opens only on a session's conversation.
            if self.session().is_none() || self.home_screen().is_some() {
                return None;
            }
            self.find.open = true;
            self.find.query.clear();
            return Some(Effect::None);
        }
        match key {
            Key::Char(ch) => {
                self.find.query.push(*ch);
                Some(self.find.pause())
            }
            Key::Backspace if !self.find.query.is_empty() => {
                self.find.query.pop();
                Some(self.find.pause())
            }
            // Esc with the notice overlay open closes it first, as the
            // overlay's own arm does (`docs/tui.md`, "Keys": Esc closes
            // whatever is on top).
            Key::Esc if self.notice_overlay().is_some() => None,
            Key::Esc => {
                self.find.close();
                Some(Effect::None)
            }
            // A second Ctrl+F opens the results view; until then the
            // bar stays open.
            Key::CtrlF => {
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
                self.step_find(true);
                Some(Effect::None)
            }
            Key::Up => {
                self.step_find(false);
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
        if !self.find.open || self.panel().is_some() || self.search_panel().is_some() {
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
            self.find.query.push_str(&joined);
            return Some(self.find.pause());
        }
        if *edit == Edit::ShiftEnter {
            self.step_find(false);
            return Some(Effect::None);
        }
        Some(Effect::None)
    }

    /// The bar as drawn, while it is open.
    pub(crate) fn find_bar(&self) -> Option<FindBar> {
        self.find.open.then(|| FindBar {
            query: self.find.query.clone(),
            count: self.find.count(self.screen.pages()),
        })
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
        if !self.find.open || !self.find.due || self.find.query.is_empty() || self.find.total == 0 {
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
                for hit in logical::matches(&line.text, &self.find.query) {
                    let anchor = Anchor::new(at, &line, hit.clone());
                    let nth = seen.get(&anchor.scopes).copied().unwrap_or(0);
                    seen.insert(anchor.scopes.clone(), nth.saturating_add(1));
                    let got = Match {
                        anchor,
                        nth,
                        snippet: Snippet::default(),
                    };
                    let current = self
                        .find
                        .current
                        .as_ref()
                        .is_some_and(|current| *current == got);
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
        if generation != self.find.generation || !self.find.open || self.find.query.is_empty() {
            return Vec::new();
        }
        if self.find.due {
            return self.find_outgoing();
        }
        self.find.due = true;
        self.start_scan();
        // Scans resident pages, fetches the first dropped one, and
        // reveals the current match in the same step: its sections open
        // and the view scrolls to it, and a dropped page it sits on
        // loads with the frame's other pages.
        self.settle_find();
        self.find_outgoing()
    }

    /// (Re)starts the scan for the current query: the matches, the current
    /// match and the count clear, and the scan runs from the page on
    /// screen to the end, then from page 0 back to it.
    fn start_scan(&mut self) {
        let pages = self.screen.pages();
        let (top, _) = self.scroll();
        let start = pages.index().locate(top).map_or(0, |(at, _)| at);
        let count = pages.page_count();
        self.find.matches.clear();
        self.find.scanned.clear();
        self.find.total = 0;
        self.find.capped = false;
        self.find.current = None;
        self.find.reveal = false;
        self.find.incomplete = false;
        self.find.start_page = start;
        self.find.order = (start..count).chain(0..start).collect();
    }

    /// Scans every resident page whose revision moved, in scan order.
    /// Whether any page scanned.
    fn scan_resident_all(&mut self) -> bool {
        let count = self.screen.pages().page_count();
        self.find.sync_order(count);
        let mut rescanned = false;
        for at in self.find.order.clone() {
            let revision = self.screen.pages().index().revision(at);
            if self.find.scanned.get(&at) == Some(&revision) {
                continue;
            }
            if self.screen.pages().part(at).is_some() {
                self.scan_resident(at);
                rescanned = true;
            }
        }
        rescanned
    }

    /// Scans resident page `at` for the current query, recording its
    /// matches whole and picking the current match when it can become it.
    fn scan_resident(&mut self, at: usize) {
        let revision = self.screen.pages().index().revision(at);
        if self.find.scanned.get(&at) == Some(&revision) {
            return;
        }
        let query = self.find.query.clone();
        let Some(found) = scan::resident(self.screen.pages(), at, &query) else {
            return;
        };
        let kept = self.find.record(at, revision, found);
        let start_page = self.find.start_page;
        let (top, _) = self.scroll();
        self.find.consider(at, &kept, start_page, top);
    }

    /// Scans the lines folded for dropped page `at`, recording its matches
    /// whole and picking the current match when it can become it.
    fn scan_scratch(
        &mut self,
        at: usize,
        revision: u64,
        rows: &[crate::turn::Row],
        texts: &[crate::rows::RowText],
    ) {
        let query = self.find.query.clone();
        let found = scan::scratch(self.screen.pages(), at, rows, texts, &query);
        let kept = self.find.record(at, revision, found);
        let start_page = self.find.start_page;
        let (top, _) = self.scroll();
        self.find.consider(at, &kept, start_page, top);
    }

    /// Fetches the next dropped page the scan has not read, if any, then
    /// falls back to the first match in page order when the scan found
    /// none at or after the top row anywhere after it. At most one request
    /// is ever on the wire, whatever the generation.
    fn pump_find(&mut self) {
        if !self.find.open || !self.find.due || self.find.query.is_empty() {
            return;
        }
        let count = self.screen.pages().page_count();
        self.find.sync_order(count);
        if !self.find.capped && self.find.fetch.is_none() {
            let mut next = None;
            for at in self.find.order.clone() {
                let revision = self.screen.pages().index().revision(at);
                if self.find.scanned.get(&at) != Some(&revision)
                    && self.screen.pages().part(at).is_none()
                {
                    next = Some(at);
                    break;
                }
            }
            if let Some(at) = next {
                self.fetch_page(at, None);
            }
        }
        if self.find.fetch.is_none()
            && self.find.current.is_none()
            && !self.find.wants(self.screen.pages())
            && let Some(first) = self.find.flat().first().cloned().cloned()
        {
            self.find.current = Some(first);
            self.find.reveal = true;
        }
    }

    /// Asks for `page`'s lines from `from`, or from its first line: one
    /// `history` command of at most [`HISTORY_LINES`] lines.
    fn fetch_page(&mut self, page: usize, from: Option<u64>) {
        let (Some(session), true) = (self.session().cloned(), self.link == Link::Up) else {
            return;
        };
        let revision = self.screen.pages().index().revision(page);
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
        let args = serde_json::json!({"from_seq": from, "to_seq": to});
        let line = session_command(&id, "history", &session, Some(args)).to_string();
        self.find.fetch = Some(Fetch {
            id,
            page,
            generation: self.find.generation,
            held: Vec::new(),
        });
        self.find.pending_out.push(line);
    }

    /// The search's unsent command lines.
    pub(in crate::app) fn find_outgoing(&mut self) -> Vec<String> {
        std::mem::take(&mut self.find.pending_out)
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
        let mut fetch = self.find.fetch.take()?;
        if fetch.generation != self.find.generation {
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
                self.find.record(fetch.page, revision, Vec::new());
                self.find.incomplete = true;
                self.notices
                    .push(format!("Could not search all of history: {message}"));
            }
            Ok(lines) => {
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
                        // The page folds whole, so a chunk never scans
                        // without the lines that open its turns.
                        match self.screen.pages().fold_text(fetch.page, &fetch.held) {
                            Some((rows, texts)) => {
                                self.scan_scratch(fetch.page, revision, &rows, &texts);
                            }
                            None => {
                                self.find.record(fetch.page, revision, Vec::new());
                            }
                        }
                    } else {
                        self.fetch_page(fetch.page, last.map(|last| last.saturating_add(1)));
                        if let Some(next) = &mut self.find.fetch {
                            next.held = fetch.held;
                        }
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
        if self.find.fetch.is_none() || !self.find.open {
            return;
        }
        self.find.fetch = None;
        self.find.pending_out.clear();
        self.find.incomplete = true;
        self.notices
            .push(format!("Could not search all of history: {LOST}"));
    }

    /// Runs after every settle while the bar is open: rescans the pages
    /// whose revision moved, reconciles the current match, fetches what
    /// dropped, and reveals the current match.
    pub(in crate::app) fn settle_find(&mut self) {
        if !self.find.open || !self.find.due || self.find.query.is_empty() {
            return;
        }
        let old = self.find.current.clone();
        let old_selected = self.find.selected_match();
        if self.scan_resident_all() {
            if let Some(old) = old {
                self.find.reconcile(old);
            }
            if let Some(old) = old_selected {
                let height = self.results_height();
                self.find.remap_selected(old, height);
            }
        }
        self.pump_find();
        self.reveal_current();
    }

    /// Reveals the current match: its closed sections open, outermost
    /// first, clearing the selection, and the view scrolls to it. On a
    /// dropped page the view scrolls so it loads, and the reveal completes
    /// after.
    pub(super) fn reveal_current(&mut self) {
        if !self.find.reveal {
            return;
        }
        let Some(current) = self.find.current.clone() else {
            self.find.reveal = false;
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
            self.find.scanned.insert(current.anchor.page, revision);
        }
        let Some((row, _)) = self.current_place(&current) else {
            self.find.reveal = false;
            return;
        };
        let (top, _) = self.scroll();
        let height = self.conversation_height();
        if !(top..top.saturating_add(height)).contains(&row) {
            self.screen.jump(row);
        }
        self.find.reveal = false;
    }

    /// The current match's conversation row and whether a closed section
    /// hides it, on its resident page.
    fn current_place(&self, current: &Match) -> Option<(usize, bool)> {
        let at = current.anchor.page;
        let query = self.find.query.clone();
        let found = scan::resident(self.screen.pages(), at, &query)?;
        // Which of the page's matches with equal scopes each hit is,
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

    /// Moves `down` through the matches in page order, then render order,
    /// wrapping at either end; nothing with no matches. The newly current
    /// match is revealed (`docs/tui.md`, "Search": Enter or ↓ moves to
    /// the next match, Shift+Enter or ↑ to the previous, both wrapping).
    fn step_find(&mut self, down: bool) {
        let flat = self.find.flat();
        if flat.is_empty() {
            return;
        }
        let at = self
            .find
            .current
            .as_ref()
            .and_then(|current| flat.iter().position(|kept| *kept == current))
            .unwrap_or(0);
        let next = if down {
            at.saturating_add(1) % flat.len()
        } else {
            at.checked_sub(1).unwrap_or(flat.len().saturating_sub(1))
        };
        if let Some(kept) = flat.get(next) {
            self.find.current = Some((*kept).clone());
            self.find.reveal = true;
        }
    }

    /// Closes the bar, going home.
    pub(super) fn close_find(&mut self) {
        self.find.close();
    }

    /// How many search matches are kept (the paging jig's report).
    pub(crate) fn find_matches(&self) -> usize {
        self.find.total
    }
}

#[cfg(test)]
#[path = "find_tests.rs"]
mod tests;
