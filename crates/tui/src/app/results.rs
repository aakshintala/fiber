//! The search results as a swapped view (`docs/tui.md`, "Search"): every
//! match, one row each, with the lines around it. [`Results`] owns the
//! selected entry and its top row; the file's `impl App` block routes keys
//! and hands the kept matches to the view.

use super::{App, Effect, Snippet};
use crate::keys::Key;

/// The results view as drawn: its header, one entry per kept match in
/// page order, then render order, and the selected entry with its top
/// row.
pub(crate) struct ResultsView {
    /// The header: `41 matches for “foo”` (`docs/tui.md`, "Search").
    pub(crate) header: String,
    /// One entry per kept match: its display lines, cloned from the
    /// kept match so the view needs no page (`docs/tui.md`, "Search").
    pub(crate) entries: Vec<Snippet>,
    /// The selected entry.
    pub(crate) selected: usize,
    /// The first entry drawn.
    pub(crate) top: usize,
}

/// The results view's selection: the selected entry and its top row.
#[derive(Debug, Default)]
pub(in crate::app) struct Results {
    /// The selected entry.
    pub(super) selected: usize,
    /// The first entry drawn.
    pub(super) top: usize,
}

impl Results {
    /// Selects `at`, keeping it drawn: with nothing kept the selection is
    /// the top, past the end it is the last entry, and the top moves only
    /// far enough to show it (`docs/tui.md`, "Search").
    pub(super) fn go(&mut self, at: usize, len: usize, height: usize) {
        if len == 0 {
            self.selected = 0;
            self.top = 0;
            return;
        }
        self.selected = at.min(len.saturating_sub(1));
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top.saturating_add(height) {
            self.top = self.selected.saturating_add(1).saturating_sub(height);
        }
    }

    /// ↓ or ↑ (and `j` or `k`) moves by one entry, holding at either end;
    /// PageUp or PageDown by a screen (`docs/tui.md`, "Search").
    fn step(&mut self, down: bool, by: usize, len: usize, height: usize) {
        let at = if down {
            self.selected.saturating_add(by)
        } else {
            self.selected.saturating_sub(by)
        };
        self.go(at, len, height);
    }
}

impl App {
    /// Whether the results view is open: one check per frame while it is
    /// closed (`docs/tui.md`, "Performance").
    pub(crate) fn results_open(&self) -> bool {
        self.find.has_results()
    }

    /// The results view's entry rows: the conversation's rows less the
    /// header's first row.
    pub(super) fn results_height(&self) -> usize {
        self.conversation_height().saturating_sub(1)
    }

    /// The results view as drawn, while it is open: every kept match in
    /// page order, then render order, with the lines around it.
    pub(crate) fn find_results(&self) -> Option<ResultsView> {
        let (selected, top) = self.find.results_at()?;
        let query = self.find.query();
        let entries: Vec<Snippet> = self
            .find
            .flat()
            .iter()
            .map(|kept| kept.snippet.clone())
            .collect();
        Some(ResultsView {
            header: format!("{} matches for “{query}”", entries.len()),
            entries,
            selected,
            top,
        })
    }

    /// Opens the results view, selecting the current match's entry or the
    /// first one; the bar stays open (`docs/tui.md`, "Search").
    pub(in crate::app) fn open_results(&mut self) {
        let selected = self.find.current_index().unwrap_or(0);
        self.find.show_results(selected);
    }

    /// Closes the results view; the bar stays open.
    pub(in crate::app) fn close_results(&mut self) {
        self.find.hide_results();
    }

    /// A key for the open results view, before the search bar in
    /// [`App::route_key`], so Enter jumps instead of stepping and ↑/↓ move
    /// the selection instead of the current match (`docs/tui.md`,
    /// "Search"). `None` while the view is closed, for Esc with the notice
    /// overlay open, and for the keys the bar and the handlers below it
    /// take: typing edits the query and closes the view.
    pub(in crate::app) fn results_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.results_open() {
            return None;
        }
        // Esc with the notice overlay open closes it first, as the
        // overlay's own arm does (`docs/tui.md`, "Keys": Esc closes
        // whatever is on top).
        if *key == Key::Esc && self.notice_overlay().is_some() {
            return None;
        }
        let height = self.results_height();
        let len = self.find.flat().len();
        match key {
            Key::Esc | Key::CtrlF => {
                self.close_results();
                Some(Effect::None)
            }
            Key::Enter => {
                if let Some(results) = self.find.results_at() {
                    self.jump_to_match(results.0);
                }
                Some(Effect::None)
            }
            Key::Up | Key::Char('k') => {
                if let Some(results) = self.find.results_mut() {
                    results.step(false, 1, len, height);
                }
                Some(Effect::None)
            }
            Key::Down | Key::Char('j') => {
                if let Some(results) = self.find.results_mut() {
                    results.step(true, 1, len, height);
                }
                Some(Effect::None)
            }
            Key::PageUp => {
                if let Some(results) = self.find.results_mut() {
                    results.step(false, height, len, height);
                }
                Some(Effect::None)
            }
            Key::PageDown => {
                if let Some(results) = self.find.results_mut() {
                    results.step(true, height, len, height);
                }
                Some(Effect::None)
            }
            // Typing, edits and any other key reach the bar and the
            // handlers below: a new query closes the view.
            Key::Char(_)
            | Key::Backspace
            | Key::Tab
            | Key::BackTab
            | Key::End
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
            | Key::AltDigit(_) => None,
        }
    }

    /// Jumps to the `at`th kept match and closes the view with the bar
    /// still open: the match becomes current and is revealed, its sections
    /// expanding (`docs/tui.md`, "Search"). Nothing with no such match.
    pub(in crate::app) fn jump_to_match(&mut self, at: usize) {
        let Some(next) = self.find.match_at(at) else {
            return;
        };
        self.find.set_current(next);
        self.close_results();
        self.reveal_current();
    }
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
