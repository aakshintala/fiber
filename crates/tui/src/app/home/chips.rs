//! The home keyboard: arrows between the entry bar, chip row and list,
//! ← → between chips, Enter clicking a chip (`docs/tui.md`, "Home").
//! Chip focus is `App.focus`; `Home` remembers only which chip was focused
//! last.

use super::super::{App, Effect};
use crate::focus::{Area, order};
use crate::home::{ListLine, Spot};
use crate::keys::{Edit, Key};
use crate::keyset::Context;
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
    /// Nothing open is the input context's word for it: the key context
    /// is Input or Conversation, with the completion panel and a
    /// steering selection still keeping the arrows, since a focused chip
    /// reads as the input box ahead of both.
    pub(in crate::app) fn home_owns_arrows(&self) -> bool {
        if !self.on_home() || self.completions().is_some() || self.steering.is_selected() {
            return false;
        }
        if !matches!(self.key_context(), Context::Input | Context::Conversation) {
            return false;
        }
        matches!(
            self.focus,
            Some(TargetId::Home(spot))
                if spot.is_chip()
                    || matches!(spot, Spot::Entry(_) | Spot::Stop(_) | Spot::More(_))
        )
    }

    /// The chips of the last frame, in focus order.
    fn chip_stops(&self) -> Vec<Spot> {
        order(&self.stops, &self.regions, Area::Conversation)
            .into_iter()
            .filter_map(|id| {
                if let TargetId::Home(spot) = id
                    && spot.is_chip()
                {
                    Some(spot)
                } else {
                    None
                }
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

    /// The list stops from the last frame, in their drawn focus order.
    /// A row's two lines and cross remain one stop.
    pub(super) fn list_stops(&self) -> Vec<Spot> {
        let mut stops = Vec::new();
        for id in order(&self.stops, &self.regions, Area::Conversation) {
            let TargetId::Home(spot) = id else {
                continue;
            };
            let Some(spot) = spot.list_stop() else {
                continue;
            };
            if stops.last() != Some(&spot) {
                stops.push(spot);
            }
        }
        stops
    }

    /// Every list stop in display order, including rows the current frame
    /// has not scrolled into view yet.
    pub(super) fn all_list_stops(&self) -> Vec<Spot> {
        self.home_screen()
            .map(|screen| {
                screen
                    .list
                    .into_iter()
                    .filter_map(|line| match line {
                        ListLine::Live { key, .. } | ListLine::Exited { key, .. } => {
                            Some(Spot::Entry(key))
                        }
                        ListLine::More { key, .. } => Some(Spot::More(key)),
                        ListLine::Head { .. } | ListLine::ExitedHead { .. } => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// ↑ on the entry bar: a draft, completion panel or recalled prompt
    /// keeps the key; otherwise focus the last list stop when one drew.
    pub(super) fn entry_up(&mut self) -> Option<Effect> {
        if !self.draft.is_empty() || self.completions().is_some() || self.recall_browsing() {
            return None;
        }
        if self.list_stops().is_empty() {
            return Some(Effect::None);
        }
        if let Some(last) = self.all_list_stops().last().copied() {
            self.focus = Some(TargetId::Home(last));
        }
        Some(Effect::None)
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

    /// A key with a chip focused: ↑ returns to the entry bar, ↓ stays on
    /// the chip, Enter clicks it and focus stays on it, Esc and Tab pass
    /// to focus, typing returns to the entry bar
    /// and then acts there. `None` passes the key on, after any focus
    /// change; every other key keeps the chip focused and acts as from
    /// the entry bar.
    pub(super) fn chip_key(&mut self, key: &Key) -> Option<Effect> {
        let chip = self.focused_chip()?;
        match key {
            Key::Up => {
                self.remember_chip(chip);
                self.focus = None;
                Some(Effect::None)
            }
            Key::Down => {
                self.remember_chip(chip);
                Some(Effect::None)
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
        let chip = self.focused_chip()?;
        match edit {
            Edit::Left | Edit::Right => {
                let stops = self.chip_stops();
                if let Some(at) = stops.iter().position(|stop| *stop == chip) {
                    let next = if matches!(edit, Edit::Left) {
                        at.checked_sub(1).unwrap_or(at)
                    } else {
                        stops.get(at.saturating_add(1)).map_or(at, |_| at + 1)
                    };
                    if let Some(landed) = stops.get(next) {
                        self.focus_chip(*landed);
                    }
                }
                Some(Effect::None)
            }
            Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::DeleteToLineStart
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {
                self.focus = None;
                None
            }
        }
    }

    /// ↑ ↓ on a drawn list stop: move through rows and the `More` row,
    /// scrolling as needed. ↑ holds at the first row; ↓ from the last row
    /// returns to the entry bar, asking for another `recent` page first
    /// when that row qualifies. A row's ✕ counts as its row. `None` for
    /// any other key or a non-list focus.
    pub(super) fn list_key(&mut self, key: &Key) -> Option<Effect> {
        let focus = self.focus?;
        if !matches!(key, Key::Up | Key::Down) {
            return None;
        }
        let drawn = self.list_stops();
        if drawn.is_empty() {
            return Some(Effect::None);
        }
        let TargetId::Home(spot) = focus else {
            return None;
        };
        let current = spot.list_stop()?;
        if !drawn.contains(&current) {
            return Some(Effect::None);
        }
        let stops = self.all_list_stops();
        let Some(at) = stops.iter().position(|stop| *stop == current) else {
            return Some(Effect::None);
        };
        if matches!(key, Key::Down) {
            if let Some(next) = stops.get(at.saturating_add(1)) {
                self.focus = Some(TargetId::Home(*next));
                return Some(Effect::None);
            }
            if let Spot::Entry(row) = current
                && let Some(effect) = self.page_recent(row)
            {
                return Some(effect);
            }
            self.focus = None;
            return Some(Effect::None);
        }
        if let Some(prev) = at.checked_sub(1).and_then(|prev| stops.get(prev)) {
            self.focus = Some(TargetId::Home(*prev));
        }
        Some(Effect::None)
    }
}

#[cfg(test)]
#[path = "chips_tests.rs"]
mod tests;
