//! The model picker on the app: the catalogue it lists, and the reads it
//! owes the loop (`docs/tui.md`, "Swapped views"). Opening, keys and
//! choosing are Part 2's; this is the read half, so no read is dead code.

use super::App;
use crate::catalogue::{Catalogue, Refresh};
use crate::keys::{Edit, Key};
use crate::model_picker::Mode;
use crate::swapped::{Frame, Spot, rows_height};

impl App {
    /// Folds a model-list read's answer: each catalogue notice shows once,
    /// and a read error shows once with the old catalogue kept.
    pub(crate) fn on_models(&mut self, result: Result<Catalogue, String>) {
        match &result {
            Ok(catalogue) => {
                for notice in &catalogue.notices {
                    self.push_notice(notice.clone());
                }
            }
            Err(error) => {
                self.push_notice(error.clone());
            }
        }
        self.model_picker.store(result);
    }

    /// The model-list read the loop owes, if one is owed.
    pub(crate) fn take_reads(&mut self) -> Option<Refresh> {
        self.model_picker.take_read()
    }

    /// Opens the model picker: each open starts fresh, on the on-screen
    /// model's row, else the first row. On home the chips name it; attached,
    /// the panel fold does. Each open asks `Stale`. One swapped view shows
    /// at a time, so another swapped view closes.
    pub(crate) fn open_model_picker(&mut self, mode: Mode) -> super::Effect {
        self.close_config_view();
        self.close_session_view();
        let on_screen = if self.on_home() {
            self.home.as_ref().and_then(|home| {
                home.launch
                    .model
                    .as_deref()
                    .map(|model| (model, home.launch.thinking.as_deref()))
            })
        } else {
            self.panel_state
                .model()
                .map(|model| (model, self.panel_state.thinking()))
        };
        self.model_picker.open(mode, on_screen);
        super::Effect::None
    }

    /// A click on the open picker's `spot`: the ✕ closes it, the buttons
    /// act as their keys, and a row or chip selects. Choosing is the next
    /// task's; here a click only selects, sending nothing.
    pub(crate) fn model_picker_click(&mut self, spot: Spot) -> super::Effect {
        match spot {
            Spot::Close => self.model_picker.close(),
            Spot::Row(at) => self.model_picker.select_frame_row(at),
            Spot::Cell(row, cell) => self.model_picker.click_cell(row, cell),
            // The picker draws no switches or rule rows.
            Spot::Switch { .. } | Spot::Revoke(_) => {}
        }
        super::Effect::None
    }

    /// The open picker's frame at `height`: the header names the rebuild
    /// size of the session on screen, if its last call is known. `None`
    /// while the picker is closed.
    pub(crate) fn model_picker_frame(&self, height: usize) -> Option<Frame> {
        self.model_picker.frame(height, self.usage_on_screen())
    }
    /// Whether the model picker is open.
    pub(crate) fn model_picker_open(&self) -> bool {
        self.model_picker.is_open()
    }

    /// A key for the open picker; `None` while it is closed, for Ctrl+C,
    /// and while the quit question is up, so quitting keeps every key.
    /// Enter and `s` choose in the next task; here every other key but
    /// the picker's own does nothing, and no key cycles.
    pub(in crate::app) fn model_picker_key(&mut self, key: &Key) -> Option<super::Effect> {
        if !self.model_picker_open() || self.quit_open() {
            return None;
        }
        match key {
            Key::CtrlC => None,
            Key::Up => {
                self.model_picker.move_row(-1);
                Some(super::Effect::None)
            }
            Key::Down => {
                self.model_picker.move_row(1);
                Some(super::Effect::None)
            }
            Key::Tab => {
                self.model_picker.toggle_show_all();
                Some(super::Effect::None)
            }
            Key::CtrlR => {
                self.model_picker.refresh();
                Some(super::Effect::None)
            }
            Key::PageUp | Key::PageDown => {
                // A page is the rows the list shows: the view's height
                // less its header, status and footer.
                let height = usize::from(if self.on_home() {
                    self.screen.height()
                } else {
                    u16::try_from(self.conversation_height()).unwrap_or(u16::MAX)
                });
                let shown = self
                    .model_picker
                    .frame(height, self.usage_on_screen())
                    .map(|frame| rows_height(&frame, height))
                    .unwrap_or(height);
                self.model_picker.move_page(key, shown);
                Some(super::Effect::None)
            }
            Key::Esc => {
                self.model_picker.close();
                Some(super::Effect::None)
            }
            // Enter and `s` choose in the next task; here they do nothing,
            // as every other key does.
            Key::Char(_)
            | Key::Backspace
            | Key::Enter
            | Key::CtrlO
            | Key::End
            | Key::AltA
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlF
            | Key::CtrlV
            | Key::CtrlL
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => Some(super::Effect::None),
        }
    }

    /// An edit for the open picker: the arrows move the selected row's
    /// chip, and every other edit does nothing. `None` while it is closed
    /// and while the quit question is up.
    pub(in crate::app) fn model_picker_edit(&mut self, edit: &Edit) -> Option<super::Effect> {
        if !self.model_picker_open() || self.quit_open() {
            return None;
        }
        match edit {
            Edit::Left => self.model_picker.move_chip(-1),
            Edit::Right => self.model_picker.move_chip(1),
            Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {}
        }
        Some(super::Effect::None)
    }
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;
