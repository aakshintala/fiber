//! The scope switches `/tools` and `/skills` share (`docs/tui.md`,
//! "Swapped views"): this project's switch and the everywhere switch,
//! which write one layer's lists and read them back.

use crate::configure::{ConfigureError, SwitchScope};
use crate::keys::Edit;
use crate::swapped::Ink;

/// The two switches' shared state: the switch Space flips.
#[derive(Debug)]
pub(crate) struct ScopeSwitches {
    /// The switch Space flips: this project at open.
    pub(crate) focus: SwitchScope,
}

impl ScopeSwitches {
    /// The switch Space flips.
    pub(crate) fn focused(&self) -> SwitchScope {
        self.focus
    }

    /// The switch a click at `at` flips: 0 is this project, anything
    /// else is everywhere (`docs/tui.md`, "Swapped views").
    pub(crate) fn scope_for_at(at: usize) -> SwitchScope {
        if at == 0 {
            SwitchScope::Project
        } else {
            SwitchScope::Everywhere
        }
    }

    /// The mark before a switch: a bold `›` when the row is selected
    /// and the switch is focused, else a space, so the columns do not
    /// move (`docs/tui.md`, "Swapped views").
    pub(crate) fn mark(selected: bool, focused: bool) -> (String, Ink) {
        if selected && focused {
            ("›".to_owned(), Ink::Heading)
        } else {
            (" ".to_owned(), Ink::Plain)
        }
    }

    /// Handles one editing key: ← and → choose the row's switch.
    pub(crate) fn edit_key(&mut self, edit: &Edit) {
        match edit {
            Edit::Left => self.focus = SwitchScope::Project,
            Edit::Right => self.focus = SwitchScope::Everywhere,
            Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::DeleteToLineStart
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {}
        }
    }

    /// Flips the `scope` switch of `state`: the write, then the reread,
    /// then the success or the error message. A write may land before
    /// it fails, so the error path rereads before it says why. The
    /// predicate reads the state first and its borrow ends before the
    /// mutable callbacks run in order; the callbacks receive the state
    /// and never capture the view.
    pub(crate) fn write_then_reread<State>(
        scope: SwitchScope,
        state: &mut State,
        is_on: impl Fn(&State, SwitchScope) -> bool,
        write: impl FnOnce(&mut State, SwitchScope, bool) -> Result<(), ConfigureError>,
        reread: impl FnOnce(&mut State),
        on_ok: impl FnOnce(&mut State),
        on_err: impl FnOnce(&mut State, String),
    ) {
        let on = !is_on(state, scope);
        match write(state, scope, on) {
            Ok(()) => {
                reread(state);
                on_ok(state);
            }
            Err(error) => {
                reread(state);
                on_err(state, error.message);
            }
        }
    }
}

#[cfg(test)]
#[path = "scope_switches_tests.rs"]
mod tests;
