//! The chip row's keyboard: ↓ from the entry bar, ← → between chips,
//! Enter clicking the chip, and ↓ ↑ between the chip row and the session
//! list (`docs/tui.md`, "Home"). Chip focus is `App.focus`; `Home`
//! remembers only which chip was focused last.

use super::super::{App, Effect};
use crate::focus::{Area, order};
use crate::home::Spot;
use crate::keys::{Edit, Key};
use crate::mouse::TargetId;

impl App {
    /// The focused chip, when focus is a chip of the chip row on home.
    pub(in crate::app) fn focused_chip(&self) -> Option<Spot> {
        if !self.on_home() {
            return None;
        }
        match self.focus {
            Some(TargetId::Home(spot)) if spot.is_chip() => Some(spot),
            Some(_) | None => None,
        }
    }

    /// Whether home owns plain arrows: on home with nothing open, and
    /// focus on a chip or a list stop. There they dispatch as home's own
    /// keys, ahead of the bindings (`docs/tui.md`, "Keys", "Bindings").
    pub(in crate::app) fn home_owns_arrows(&self) -> bool {
        if !self.on_home()
            || self.quit_open()
            || self.home_modal()
            || self.model_picker_open()
            || self.config_view_open()
            || self.session_view_open()
        {
            return false;
        }
        if self.keymap_top().is_some()
            || self.panel().is_some()
            || self.offer_open()
            || self.search_panel().is_some()
            || self.find_open()
            || self.completions().is_some()
            || self.steering.is_selected()
        {
            return false;
        }
        matches!(
            self.focus,
            Some(TargetId::Home(spot))
                if spot.is_chip()
                    || matches!(spot, Spot::Entry(_) | Spot::Stop(_) | Spot::Toggle)
        )
    }

    /// The chips of the last frame, in focus order.
    fn chip_stops(&self) -> Vec<Spot> {
        order(&self.stops, &self.regions, Area::Conversation)
            .into_iter()
            .filter_map(|id| match id {
                TargetId::Home(spot) if spot.is_chip() => Some(spot),
                _ => None,
            })
            .collect()
    }

    /// Focuses `chip`, remembering it as the chip focused last.
    fn focus_chip(&mut self, chip: Spot) {
        self.focus = Some(TargetId::Home(chip));
        if let Some(home) = self.home.as_mut() {
            home.last_chip = chip;
        }
    }

    /// Remembers the focused chip, for a key leaving the chip row.
    fn remember_chip(&mut self, chip: Spot) {
        if let Some(home) = self.home.as_mut() {
            home.last_chip = chip;
        }
    }

    /// The chip ↓ from the entry bar and ↑ from the first row land on:
    /// the chip focused last while drawn, else the first chip stop.
    fn landing_chip(&self) -> Option<Spot> {
        let stops = self.chip_stops();
        if stops.is_empty() {
            return None;
        }
        let last = self.home.as_ref().map(|home| home.last_chip);
        if last.is_some_and(|last| stops.contains(&last)) {
            last
        } else {
            stops.into_iter().next()
        }
    }

    /// Whether the last frame drew a session row: row moves act only
    /// then, and do nothing when no row fits.
    fn drew_a_row(&self) -> bool {
        self.stops
            .iter()
            .any(|target| matches!(target.id, TargetId::Home(Spot::Entry(_))))
    }

    /// The rows home draws, scoped as drawn: their keys top to bottom.
    fn shown_keys(&self) -> Vec<u64> {
        let Some(home) = self.home.as_ref() else {
            return Vec::new();
        };
        let scoped = home.launch.git && !home.sessions.show_all();
        home.sessions
            .shown(&home.launch.project, scoped)
            .iter()
            .map(|row| row.key)
            .collect()
    }

    /// ↓ on the entry bar: the completion panel and a recalled prompt
    /// keep it; a draft whose cursor is above its last wrapped row moves
    /// the cursor down a row; otherwise the chip row takes focus and the
    /// draft is kept.
    pub(super) fn entry_down(&mut self) -> Option<Effect> {
        if self.completions().is_some() || self.recall_browsing() {
            return None;
        }
        if self.draft.down(self.draft_width()) {
            return Some(Effect::None);
        }
        match self.landing_chip() {
            Some(chip) => {
                self.focus_chip(chip);
                Some(Effect::None)
            }
            None => Some(Effect::None),
        }
    }

    /// A key with a chip focused: ↑ returns to the entry bar, ↓ leaves
    /// for the first session row, Enter clicks the chip and focus stays
    /// on it, Esc and Tab pass to focus, typing returns to the entry bar
    /// and then acts there. `None` passes the key on, after any focus
    /// change; every other key keeps the chip focused and acts as from
    /// the entry bar.
    pub(super) fn chip_key(&mut self, key: &Key) -> Option<Effect> {
        let Some(chip) = self.focused_chip() else {
            return None;
        };
        match key {
            Key::Up => {
                self.remember_chip(chip);
                self.focus = None;
                Some(Effect::None)
            }
            Key::Down => {
                self.remember_chip(chip);
                if !self.drew_a_row() {
                    return Some(Effect::None);
                }
                match self.shown_keys().into_iter().next() {
                    Some(first) => {
                        self.focus = Some(TargetId::Home(Spot::Entry(first)));
                        Some(Effect::None)
                    }
                    None => Some(Effect::None),
                }
            }
            Key::Enter => {
                self.remember_chip(chip);
                Some(self.home_click(chip))
            }
            Key::Esc | Key::Tab | Key::BackTab => None,
            Key::Char(_) | Key::Backspace | Key::CtrlR | Key::CtrlV | Key::CtrlG => {
                self.focus = None;
                None
            }
            Key::CtrlC
            | Key::CtrlO
            | Key::CtrlL
            | Key::CtrlF
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::F1
            | Key::AltA
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => None,
        }
    }

    /// An edit with a chip focused: ← → move between the chips and stop
    /// at the ends; every other edit returns focus to the entry bar and
    /// applies there. `None` passes the edit on.
    pub(super) fn chip_edit(&mut self, edit: &Edit) -> Option<Effect> {
        let Some(chip) = self.focused_chip() else {
            return None;
        };
        match edit {
            Edit::Left | Edit::Right => {
                let stops = self.chip_stops();
                if let Some(at) = stops.iter().position(|stop| *stop == chip) {
                    let next = if matches!(edit, Edit::Left) {
                        at.checked_sub(1).map_or(at, |prev| prev)
                    } else {
                        stops.get(at.saturating_add(1)).map_or(at, |_| at + 1)
                    };
                    if let Some(landed) = stops.get(next) {
                        self.focus_chip(*landed);
                    }
                }
                self.remember_chip(chip);
                Some(Effect::None)
            }
            Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {
                self.focus = None;
                None
            }
        }
    }

    /// ↑ ↓ on a session row or the scope toggle: ↓ moves down the rows,
    /// skipping the toggle, and at the end asks the next `recent` page;
    /// ↑ moves up the rows, from the first row to the chip focused last,
    /// and from the toggle to that chip. A row's ✕ counts as its row.
    /// `None` for any other key, or when no row was drawn.
    pub(super) fn list_key(&mut self, key: &Key) -> Option<Effect> {
        let focus = self.focus?;
        if !matches!(key, Key::Up | Key::Down) {
            return None;
        }
        if !self.drew_a_row() {
            return Some(Effect::None);
        }
        let down = matches!(key, Key::Down);
        let shown = self.shown_keys();
        let row = match focus {
            TargetId::Home(Spot::Entry(row) | Spot::Stop(row)) => Some(row),
            TargetId::Home(Spot::Toggle) => None,
            _ => return None,
        };
        if down {
            let next = match row {
                // The toggle is not a ↓ stop: ↓ on it skips to the
                // first row.
                None => shown.into_iter().next(),
                Some(row) => {
                    let mut keys = shown.iter();
                    match keys.position(|key| *key == row) {
                        Some(at) => shown.get(at.saturating_add(1)).copied(),
                        None => None,
                    }
                }
            };
            if let Some(next) = next {
                self.focus = Some(TargetId::Home(Spot::Entry(next)));
                return Some(Effect::None);
            }
            // Past the last row, ↓ asks the next `recent` page when the
            // focused row ends the list. Focus stays.
            if let Some(row) = row {
                return self.page_recent(row);
            }
            return Some(Effect::None);
        }
        let prev = match row {
            Some(row) => {
                let mut above = None;
                for key in &shown {
                    if *key == row {
                        break;
                    }
                    above = Some(*key);
                }
                above
            }
            // ↑ on the toggle returns to the chip focused last.
            None => None,
        };
        match prev {
            Some(prev) => {
                self.focus = Some(TargetId::Home(Spot::Entry(prev)));
                Some(Effect::None)
            }
            None => match self.landing_chip() {
                Some(chip) => {
                    self.focus_chip(chip);
                    Some(Effect::None)
                }
                None => Some(Effect::None),
            },
        }
    }
}

#[cfg(test)]
#[path = "chips_tests.rs"]
mod tests;
