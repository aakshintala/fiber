//! The search's state (`docs/tui.md`, "Search"): the bar's query and
//! pause, the one `history` request on the wire, the kept matches by page
//! with their scan order, and the current match with its pending reveal.
//! The driver in the parent module calls the interface below; nothing
//! here reads `App`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::FIND_PAUSE;
use super::scan::{Anchor, Snippet};
use crate::app::{Effect, results::Results};
use crate::window::Pages;

/// How many matches a search keeps: past it the scan stops and the count
/// shows `10000+` (`docs/tui.md`, "Search").
const MAX_MATCHES: usize = 10_000;

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
pub(super) struct Fetch {
    /// The command's id: only its answer folds into the scan.
    id: String,
    /// The page it reads.
    pub(super) page: usize,
    /// The generation it scans for: older than the current one is stale,
    /// and its answer folds nothing.
    pub(super) generation: u64,
    /// The width the page was fetched at: an answer for an old table
    /// layout is not recorded as scanned at the new width.
    pub(super) width: u16,
    /// The page's revision when it was fetched: lines read before a
    /// text change may miss it, so the answer requeues instead.
    pub(super) revision: u64,
    /// The page's lines read so far: a page over one answer folds whole
    /// once its last chunk arrives, so a chunk never scans without the
    /// lines that open its turns.
    pub(super) held: Vec<contract::Envelope>,
}

/// The search's state.
#[derive(Debug, Default)]
pub(in crate::app) struct Find {
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
    results: Option<Results>,
    /// A page could not be read: the count gains ` · incomplete`.
    incomplete: bool,
    /// The session the scan runs on, if it started: attaching another
    /// session resets the search, so old matches never answer for it.
    session: Option<contract::SessionId>,
    /// The page on screen when the scan started: the scan runs from it to
    /// the end, then from page 0 back to it.
    start_page: usize,
    /// The pages in scan order, append-only within a query: newly cut
    /// pages join behind the pages already in it.
    order: Vec<usize>,
    /// How far the scan reached in `order`: every earlier position
    /// scanned when resident, or fetched when dropped. Only ever moves
    /// forward within a query, so each position is reached once.
    cursor: usize,
    /// Reached pages whose revision has since moved: they rescan in
    /// page order without starving the pages the cursor still owes.
    dirty: BTreeSet<usize>,
    /// Pages still needing scan action: unreached ones, and dirtied
    /// ones. Tells completion without walking every page per answer.
    outstanding: usize,
    /// The page-revision clock the scan last verified: while it holds,
    /// no text moved, so no page needs re-examination.
    seen_clock: u64,
}

impl Find {
    /// Whether the bar is open.
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the bar with an empty query and no scan due.
    pub(super) fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.due = false;
    }

    /// Closes the bar and drops the whole scan; the generation stays, so a
    /// pause scheduled before the close is stale after a reopen.
    pub(super) fn close(&mut self) {
        *self = Self {
            generation: self.generation,
            ..Self::default()
        };
    }

    /// Types `text` into the bar and schedules the scan after the pause.
    pub(super) fn type_query(&mut self, text: &str) -> Effect {
        self.query.push_str(text);
        self.pause()
    }

    /// Deletes the bar's last query char and schedules the scan after
    /// the pause.
    pub(super) fn pop_query(&mut self) -> Effect {
        self.query.pop();
        self.pause()
    }

    /// What was typed into the bar.
    pub(in crate::app) fn query(&self) -> &str {
        &self.query
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

    /// The count beside the query, as drawn.
    pub(super) fn count(&self, pages: &Pages) -> String {
        // A new query clears the count until its pause passes.
        if !self.due {
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

    /// The query's generation: a pause or an answer tagged with another
    /// is stale.
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the pause passed for the current generation and the scan
    /// started.
    pub(super) fn due(&self) -> bool {
        self.due
    }

    /// Starts the scan for the current query from the page on screen:
    /// the matches, the current match and the count clear, and the scan
    /// runs from `start_page` to the end, then from page 0 back to it.
    /// Every page is outstanding until it scans at its revision.
    pub(super) fn start(
        &mut self,
        start_page: usize,
        page_count: usize,
        clock: u64,
        session: Option<contract::SessionId>,
    ) {
        self.due = true;
        self.matches.clear();
        self.scanned.clear();
        self.total = 0;
        self.capped = false;
        self.current = None;
        self.reveal = false;
        self.incomplete = false;
        self.session = session;
        self.start_page = start_page;
        self.order = (start_page..page_count).chain(0..start_page).collect();
        self.cursor = 0;
        self.dirty.clear();
        self.outstanding = page_count;
        self.seen_clock = clock;
    }

    /// The session the scan runs on, if it started.
    pub(super) fn session_id(&self) -> Option<contract::SessionId> {
        self.session.clone()
    }

    /// Pulls pages cut since the query started into the scan order
    /// behind the pages already in it: each new page is outstanding
    /// until it scans (`docs/tui.md`, "Search": search covers the
    /// whole session log). Appending a contiguous range needs no
    /// membership walk and no copy of the order.
    pub(super) fn extend_order(&mut self, page_count: usize) {
        let fresh = page_count.saturating_sub(self.order.len());
        if fresh == 0 {
            return;
        }
        crate::work::add(|work| work.search_order_pages += fresh);
        self.order.extend(self.order.len()..page_count);
        self.outstanding = self.outstanding.saturating_add(fresh);
    }

    /// The page at scan-order `pos`, if it has one.
    pub(super) fn page_at(&self, pos: usize) -> Option<usize> {
        self.order.get(pos).copied()
    }

    /// How far the scan reached in scan order.
    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Moves the reach past `through`: the scan settled every earlier
    /// position, scanning resident pages and fetching dropped ones.
    pub(super) fn reach(&mut self, through: usize) {
        self.cursor = self.cursor.max(through);
    }

    /// Moves the reach past positions already settled at their
    /// revision: scanning or fetching them again would repeat work.
    /// Counts each position skipped; each position is skipped once,
    /// since the reach never moves back within a query.
    pub(super) fn skip_settled(&mut self, pages: &Pages) {
        while let Some(at) = self.order.get(self.cursor).copied() {
            if self.scanned.get(&at) != Some(&pages.index().revision(at)) {
                break;
            }
            crate::work::add(|work| work.search_order_pages += 1);
            self.cursor = self.cursor.saturating_add(1);
        }
    }

    /// The reached positions waiting on a rescan, earliest in scan
    /// order first.
    pub(super) fn dirty(&self) -> &BTreeSet<usize> {
        &self.dirty
    }

    /// Drops `pos` from the rescan set: it scanned at its revision, and
    /// a later verification soils it again if its text moved since.
    pub(super) fn clean(&mut self, pos: usize) {
        self.dirty.remove(&pos);
    }

    /// Marks the reached position `pos` for a rescan: its page scanned
    /// before at another revision. Newly soiled pages join outstanding,
    /// so completion waits for them; pages never scanned stay
    /// outstanding from their order extension.
    fn soil(&mut self, pos: usize) {
        let settled = self
            .order
            .get(pos)
            .is_some_and(|at| self.scanned.contains_key(at));
        if self.dirty.insert(pos) && settled {
            self.outstanding = self.outstanding.saturating_add(1);
        }
    }

    /// Requeues `page` after a stale answer: its fetch read lines the
    /// page no longer holds, so it scans again at the current layout
    /// without issuing a second request. The page was outstanding under
    /// its fetch and stays so. Counts each order entry the lookup
    /// passes, so no walk hides here.
    pub(super) fn requeue(&mut self, page: usize) {
        if let Some(pos) = self.position_of(page) {
            self.dirty.insert(pos);
        }
    }

    /// `page`'s position in scan order, if it has one. Counts each
    /// entry examined: requeues are rare, but their walk is still work.
    pub(super) fn position_of(&self, page: usize) -> Option<usize> {
        for (pos, at) in self.order.iter().enumerate() {
            crate::work::add(|work| work.search_order_pages += 1);
            if *at == page {
                return Some(pos);
            }
        }
        None
    }

    /// Whether the scan verified the current page-revision clock: while
    /// it holds, no text moved anywhere.
    pub(super) fn verified(&self, clock: u64) -> bool {
        self.seen_clock == clock
    }

    /// Re-examines every scanned page's revision after the clock moved:
    /// a reached position scanned before whose revision moved since
    /// soils for a rescan. Counts one bookkeeping visit per order entry
    /// examined, and remembers the clock, so a later settle with no
    /// moves costs nothing. Whether any position soiled.
    pub(super) fn verify(&mut self, pages: &Pages) -> bool {
        let clock = pages.index().clock();
        if self.seen_clock == clock {
            return false;
        }
        crate::work::add(|work| work.search_order_pages += self.order.len());
        self.seen_clock = clock;
        let mut soiled = false;
        for pos in 0..self.order.len() {
            let Some(at) = self.order.get(pos).copied() else {
                continue;
            };
            let revision = pages.index().revision(at);
            if self.scanned.get(&at).is_some_and(|seen| *seen != revision) {
                self.soil(pos);
                soiled = true;
            }
        }
        soiled
    }

    /// The revision page `at` scanned at, if it scanned.
    pub(super) fn scanned(&self, at: usize) -> Option<u64> {
        self.scanned.get(&at).copied()
    }

    /// Records that page `at` scanned at `revision` with its matches
    /// kept.
    pub(super) fn mark_scanned(&mut self, at: usize, revision: u64) {
        self.scanned.insert(at, revision);
    }

    /// Whether any page still wants scanning: outstanding pages, or
    /// text that moved since the last verification. Tests only: the
    /// count reads completion without walking every page per frame.
    #[cfg(test)]
    pub(super) fn wants(&self, pages: &Pages) -> bool {
        self.outstanding > 0 || !self.verified(pages.index().clock())
    }

    /// Whether the cap stopped the scan.
    pub(super) fn capped(&self) -> bool {
        self.capped
    }

    /// How many matches are kept, capped at [`MAX_MATCHES`].
    pub(super) fn total(&self) -> usize {
        self.total
    }

    /// Whether the count shows it is still scanning: a fetch is on the
    /// wire, or pages are outstanding and the cap did not stop the scan.
    /// Reads the outstanding count and the clock, never the pages.
    fn scanning(&self, pages: &Pages) -> bool {
        self.fetch.is_some()
            || (!self.capped && (self.outstanding > 0 || !self.verified(pages.index().clock())))
    }

    /// Whether the scan settled every page at the verified clock: no
    /// page outstanding and no text moved since.
    pub(super) fn settled(&self, pages: &Pages) -> bool {
        self.outstanding == 0 && self.verified(pages.index().clock())
    }

    /// Records page `at`'s matches, whole, in render order, stopping at
    /// the cap: the flattened order stays page order, then render order.
    /// A page newly settled at its revision leaves outstanding;
    /// re-recording the same revision changes nothing.
    /// Returns the kept matches with their rows.
    pub(super) fn record(
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
        if self.scanned.get(&at) != Some(&revision) {
            self.outstanding = self.outstanding.saturating_sub(1);
        }
        self.scanned.insert(at, revision);
        kept
    }

    /// Picks the current match from page `at`'s freshly kept matches, if
    /// none is current yet: the first at or after the top row on the page
    /// on screen, the first anywhere past it. Later finds never move it.
    pub(super) fn consider(&mut self, at: usize, found: &[(Match, usize, bool)], top: usize) {
        if self.current.is_some() {
            return;
        }
        if let Some((kept, _, _)) = found
            .iter()
            .find(|(_, row, _)| at != self.start_page || *row >= top)
        {
            self.current = Some(kept.clone());
            self.reveal = true;
        }
    }

    /// The page could not be read: it stays unscanned no more, so it
    /// scans again when its revision moves, and the count gains
    /// ` · incomplete`.
    pub(super) fn unreadable(&mut self, at: usize, revision: u64) {
        self.record(at, revision, Vec::new());
        self.incomplete = true;
    }

    /// Whether a `history` request is on the wire.
    pub(super) fn fetching(&self) -> bool {
        self.fetch.is_some()
    }

    /// Sends `line` for `page`'s lines, holding `held` for its next
    /// chunk: at most one request is ever on the wire, whatever the
    /// generation. Captures the layout the fetch reads under, so an
    /// answer for an old table layout never stamps the new width.
    pub(super) fn send(
        &mut self,
        id: String,
        page: usize,
        width: u16,
        revision: u64,
        held: Vec<contract::Envelope>,
        line: String,
    ) {
        self.fetch = Some(Fetch {
            id,
            page,
            generation: self.generation,
            width,
            revision,
            held,
        });
        self.pending_out.push(line);
    }

    /// Whether `envelope` answers the fetch: an accepted or rejected
    /// answer with its command id, for the attached session.
    pub(super) fn answers(
        &self,
        envelope: &contract::Envelope,
        session: &contract::SessionId,
    ) -> bool {
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

    /// Takes the `history` request on the wire, if any, to fold its
    /// answer.
    pub(super) fn take_fetch(&mut self) -> Option<Fetch> {
        self.fetch.take()
    }

    /// The search's unsent command lines.
    pub(super) fn outgoing(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_out)
    }

    /// The link went down: with no fetch on the wire nothing is lost,
    /// else the fetch clears and a running scan is incomplete. Returns
    /// whether a fetch was lost; the driver then pushes the notice.
    pub(super) fn lose(&mut self) -> bool {
        if self.fetch.is_none() {
            return false;
        }
        self.fetch = None;
        self.pending_out.clear();
        self.incomplete = true;
        true
    }

    /// The current match, if any.
    pub(super) fn current(&self) -> Option<&Match> {
        self.current.as_ref()
    }

    /// Makes `next` current and shows it: its sections open and the view
    /// scrolls to it (`docs/tui.md`, "Search").
    pub(in crate::app) fn set_current(&mut self, next: Match) {
        self.current = Some(next);
        self.reveal = true;
    }

    /// Moves `down` through the matches in page order, then render order,
    /// wrapping at either end; nothing with no matches. The newly current
    /// match is revealed (`docs/tui.md`, "Search": Enter or ↓ moves to
    /// the next match, Shift+Enter or ↑ to the previous, both wrapping).
    pub(super) fn step(&mut self, down: bool) {
        let flat = self.flat();
        if flat.is_empty() {
            return;
        }
        let at = self
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
            self.set_current((*kept).clone());
        }
    }

    /// Whether the current match is not shown yet: its sections open and
    /// the view scrolls to it.
    pub(super) fn revealing(&self) -> bool {
        self.reveal
    }

    /// The current match is shown: no reveal is pending.
    pub(super) fn revealed(&mut self) {
        self.reveal = false;
    }

    /// Reconciles the current match with rescanned pages: an equal anchor
    /// stays current and keeps a pending reveal, else the first match
    /// after the old key's place in page order, wrapping, else none, and
    /// nothing is revealed (`docs/tui.md`, "Search").
    pub(super) fn reconcile(&mut self, old: Match) {
        let flat = self.flat();
        let next = remapped(&flat, &old).and_then(|at| flat.get(at).cloned().cloned());
        let equal = next.as_ref().is_some_and(|got| got.anchor == old.anchor);
        self.current = next;
        if !equal {
            self.reveal = false;
        }
    }

    /// The results view's selected entry's match, if any.
    pub(super) fn selected_match(&self) -> Option<Match> {
        let selected = self.results.as_ref()?.selected;
        self.match_at(selected)
    }

    /// Reconciles the results view's selected entry with rescanned pages
    /// the same way: the same occurrence stays selected, else the
    /// first match after the old one's place in page order, wrapping,
    /// else the first one, and none with no matches (`docs/tui.md`,
    /// "Search").
    pub(super) fn remap_selected(&mut self, old: Match, height: usize) {
        let flat = self.flat();
        let at = remapped(&flat, &old).unwrap_or(0);
        let len = flat.len();
        if let Some(results) = self.results.as_mut() {
            results.go(at, len, height);
        }
    }

    /// The kept matches in page order, then render order.
    pub(in crate::app) fn flat(&self) -> Vec<&Match> {
        self.matches.values().flatten().collect()
    }

    /// The current match's place in page order, then render order.
    pub(in crate::app) fn current_index(&self) -> Option<usize> {
        let current = self.current.as_ref()?;
        self.flat().iter().position(|kept| *kept == current)
    }

    /// The kept match at `at` in page order, then render order.
    pub(in crate::app) fn match_at(&self, at: usize) -> Option<Match> {
        self.flat().get(at).cloned().cloned()
    }

    /// Shows the results view, selecting the current match's entry or
    /// the first one (`docs/tui.md`, "Search").
    pub(in crate::app) fn show_results(&mut self, selected: usize) {
        self.results = Some(Results {
            selected,
            top: selected,
        });
    }

    /// Closes the results view; the bar stays open.
    pub(in crate::app) fn hide_results(&mut self) {
        self.results = None;
    }

    /// Whether the results view is open.
    pub(in crate::app) fn has_results(&self) -> bool {
        self.results.is_some()
    }

    /// The results view, for moving its selection.
    pub(in crate::app) fn results_mut(&mut self) -> Option<&mut Results> {
        self.results.as_mut()
    }

    /// The results view's selected entry and its top row.
    pub(in crate::app) fn results_at(&self) -> Option<(usize, usize)> {
        self.results
            .as_ref()
            .map(|results| (results.selected, results.top))
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
        .or((!flat.is_empty()).then_some(0))
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
