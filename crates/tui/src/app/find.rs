//! Conversation search (`docs/tui.md`, "Search"): Ctrl+F opens the bar,
//! typing fills its query, and every keystroke schedules the scan after a
//! pause (`docs/tui.md`, "History and paging": it waits for a pause in
//! typing of a quarter of a second). The scan itself, its marks and the
//! results view land in later tasks of this ticket.

use std::time::Duration;

use super::{App, Effect};
use crate::keys::{Edit, Key};

/// How long after the last keystroke the scan starts (`docs/tui.md`,
/// "History and paging").
pub(in crate::app) const FIND_PAUSE: Duration = Duration::from_millis(250);

/// The search bar as drawn: its query and its match count, empty while no
/// scan has run.
pub(crate) struct FindBar {
    /// What was typed into the bar.
    pub(crate) query: String,
    /// The count beside it, as drawn.
    pub(crate) count: String,
}

/// The search's state: the bar and its query. The scan's fetch, matches
/// and current match land with the task that reads them.
#[derive(Debug, Default)]
pub(super) struct Find {
    /// Whether the bar is open.
    open: bool,
    /// What was typed into the bar.
    query: String,
    /// Bumped on every query change, never reset; a pause or an answer
    /// tagged with another is stale.
    generation: u64,
}

impl Find {
    /// Closes the bar and drops its query.
    fn close(&mut self) {
        self.open = false;
        self.query.clear();
    }

    /// Schedules the scan after the pause for the new generation.
    fn pause(&mut self) -> Effect {
        self.generation = self.generation.saturating_add(1);
        Effect::FindPause {
            generation: self.generation,
            after: FIND_PAUSE,
        }
    }
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
            // A second Ctrl+F opens the results view, which lands with
            // the task that draws it; until then the bar stays open.
            Key::CtrlF => Some(Effect::None),
            // The view's keys scroll on with the bar open.
            Key::PageUp | Key::PageDown => None,
            Key::Backspace
            | Key::Enter
            | Key::Up
            | Key::Down
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
        Some(Effect::None)
    }

    /// The bar as drawn, while it is open.
    pub(crate) fn find_bar(&self) -> Option<FindBar> {
        self.find.open.then(|| FindBar {
            query: self.find.query.clone(),
            count: String::new(),
        })
    }

    /// The match marks on screen: none until the scan lands.
    pub(crate) fn find_marks(
        &self,
        _area: ratatui::layout::Rect,
    ) -> Vec<(ratatui::layout::Rect, bool)> {
        Vec::new()
    }

    /// The scan due after the pause for `generation`: nothing until the
    /// scan lands.
    pub(crate) fn find_due(&mut self, _generation: u64) -> Vec<String> {
        Vec::new()
    }

    /// Closes the bar, going home.
    pub(super) fn close_find(&mut self) {
        self.find.close();
    }
}

#[cfg(test)]
#[path = "find_tests.rs"]
mod tests;
