//! The search's state (`docs/tui.md`, "Search"): the bar's query and
//! pause, the one `history` request on the wire, the kept matches by page
//! with their scan order, and the current match with its pending reveal.
//! The driver in the parent module calls the interface below; nothing
//! here reads `App`.

use std::collections::{BTreeMap, HashMap};

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
    /// The page on screen when the scan started: the scan runs from it to
    /// the end, then from page 0 back to it.
    start_page: usize,
    /// The pages in scan order.
    order: Vec<usize>,
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

    /// Closes the bar and drops the whole scan.
    pub(super) fn close(&mut self) {
        *self = Self::default();
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
    pub(super) fn start(&mut self, start_page: usize, page_count: usize) {
        self.due = true;
        self.matches.clear();
        self.scanned.clear();
        self.total = 0;
        self.capped = false;
        self.current = None;
        self.reveal = false;
        self.incomplete = false;
        self.start_page = start_page;
        self.order = (start_page..page_count).chain(0..start_page).collect();
    }

    /// Pulls pages cut since the query started into the scan order
    /// behind the pages already in it, and returns the order: a page
    /// never scanned scans too (`docs/tui.md`, "Search": search covers
    /// the whole session log).
    pub(super) fn scan_order(&mut self, page_count: usize) -> Vec<usize> {
        for at in 0..page_count {
            if !self.order.contains(&at) {
                self.order.push(at);
            }
        }
        self.order.clone()
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

    /// Whether any page still wants scanning: never scanned, or scanned
    /// at an older revision. A scanned page keeps its matches when it
    /// drops; its revision moves only when its text does.
    pub(super) fn wants(&self, pages: &Pages) -> bool {
        (0..pages.page_count()).any(|at| self.scanned.get(&at) != Some(&pages.index().revision(at)))
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
    /// wire, or a page still wants scanning and the cap did not stop it.
    fn scanning(&self, pages: &Pages) -> bool {
        self.fetch.is_some() || (!self.capped && self.wants(pages))
    }

    /// Records page `at`'s matches, whole, in render order, stopping at
    /// the cap: the flattened order stays page order, then render order.
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
    /// generation.
    pub(super) fn send(
        &mut self,
        id: String,
        page: usize,
        held: Vec<contract::Envelope>,
        line: String,
    ) {
        self.fetch = Some(Fetch {
            id,
            page,
            generation: self.generation,
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
    /// else the fetch clears and a running scan is incomplete. Whether
    /// anything was lost: the driver pushes the notice that says why.
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
        .or_else(|| flat.first().map(|_| 0))
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
