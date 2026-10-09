//! The model picker's state (`docs/tui.md`, "Swapped views"): the
//! installed models, the scope they show under, and the read the loop
//! owes. Each open starts fresh; choosing and saving is Part 2.

use crate::catalogue::{Catalogue, ModelEntry, Refresh};

/// What the picker was opened for. Choosing and scoping are later tasks;
/// only choosing exists yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Choosing a model, and its thinking level.
    Choose,
}

/// One open picker: the selection, each row's chip, and the scope toggle.
/// Nothing carries into the next open.
#[derive(Debug)]
pub(crate) struct Open {
    /// What the picker was opened for. Choosing reads it; until then it
    /// names the open.
    #[allow(dead_code, reason = "Part 3's choosing reads the mode")]
    pub(crate) mode: Mode,
    /// The selected row's catalogue index.
    pub(crate) selected: usize,
    /// Each row's chip, by catalogue index: its level's place in the
    /// entry's levels, or no chip for a model with no levels or no
    /// declared level to preselect.
    pub(crate) chips: Vec<Option<usize>>,
    /// Each row touched by the chip keys, by catalogue index.
    pub(crate) touched: Vec<bool>,
    /// The scope toggle shows every installed model.
    pub(crate) show_all: bool,
}

/// The model picker: what it lists, and what it waits on.
#[derive(Default)]
pub(crate) struct ModelPicker {
    /// The installed models, from the latest read that answered.
    pub(crate) catalogue: Catalogue,
    /// Why the lists could not be read, when no catalogue is held yet.
    pub(crate) error: Option<String>,
    /// A `Stale` or `Every` read runs: the picker shows "refreshing…" .
    pub(crate) refreshing: bool,
    /// `scoped_models`: the references the picker shows; empty means
    /// every installed model (`docs/configuration.md`, "Keys").
    pub(crate) scoped: Vec<String>,
    /// The read the loop owes: `Stale` each time the picker opens,
    /// `Every` from its refresh button.
    pub(crate) want: Option<Refresh>,
    /// The open picker, if one is open.
    pub(crate) open: Option<Open>,
}

impl ModelPicker {
    /// Stores a read's answer: the catalogue, or the error with the old
    /// catalogue kept. What the loop owes is taken, never cleared here:
    /// an answer to the startup read must not drop a meanwhile opened
    /// picker's `Stale`.
    pub(crate) fn store(&mut self, result: Result<Catalogue, String>) {
        self.refreshing = false;
        match result {
            Ok(catalogue) => {
                self.catalogue = catalogue;
                self.error = None;
            }
            Err(error) => {
                self.error = Some(error);
            }
        }
        let len = self.catalogue.models.len();
        if let Some(open) = self.open.as_mut() {
            open.chips.resize(len, None);
            open.touched.resize(len, false);
        }
    }

    /// The read the loop owes, if one is owed.
    pub(crate) fn take_read(&mut self) -> Option<Refresh> {
        let read = self.want.take();
        // Only a `Stale` or `Every` read shows "refreshing…": the startup
        // `Cached` read shows "Reading the model lists…" instead.
        if matches!(read, Some(Refresh::Stale | Refresh::Every)) {
            self.refreshing = true;
        }
        read
    }

    /// Opens the picker fresh: the selection on the on-screen model, else
    /// the first row; each row's chip at its preselected level, untouched;
    /// "show all" off. Each open asks `Stale`, keeping a wider `Every`.
    pub(crate) fn open(&mut self, mode: Mode, on_screen: Option<(&str, Option<&str>)>) {
        let (rows, _) = visible(&self.catalogue.models, &self.scoped, false);
        let selected = on_screen
            .and_then(|(model, _)| {
                rows.iter().copied().find(|index| {
                    self.catalogue
                        .models
                        .get(*index)
                        .is_some_and(|entry| entry.reference == model)
                })
            })
            .or(rows.first().copied())
            .unwrap_or(0);
        let chips = self
            .catalogue
            .models
            .iter()
            .map(|entry| preselect(entry, on_screen))
            .collect();
        self.open = Some(Open {
            mode,
            selected,
            chips,
            touched: vec![false; self.catalogue.models.len()],
            show_all: false,
        });
        self.want = Some(match self.want {
            Some(Refresh::Every) => Refresh::Every,
            _ => Refresh::Stale,
        });
    }

    /// Closes the picker, sending nothing and saving nothing.
    pub(crate) fn close(&mut self) {
        self.open = None;
    }

    /// Whether the picker is open.
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Moves the selection over the model rows by `delta`, clamped:
    /// headings are not stops, and no key cycles past either end.
    pub(crate) fn move_row(&mut self, delta: isize) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let (rows, _) = visible(&self.catalogue.models, &self.scoped, open.show_all);
        if rows.is_empty() {
            return;
        }
        let at = rows
            .iter()
            .position(|index| *index == open.selected)
            .unwrap_or(0);
        let next = at
            .saturating_add_signed(delta)
            .clamp(0, rows.len().saturating_sub(1));
        if let Some(selected) = rows.get(next) {
            open.selected = *selected;
        }
    }

    /// Moves the selected row's chip by `delta`, clamped, and marks the
    /// row touched. A model with no levels has no chip to move.
    pub(crate) fn move_chip(&mut self, delta: isize) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let Some(entry) = self.catalogue.models.get(open.selected) else {
            return;
        };
        if entry.levels.is_empty() {
            return;
        }
        let chip = match open.chips.get(open.selected).copied().flatten() {
            Some(chip) => chip
                .saturating_add_signed(delta)
                .clamp(0, entry.levels.len().saturating_sub(1)),
            None if delta >= 0 => 0,
            None => entry.levels.len().saturating_sub(1),
        };
        if let Some(slot) = open.chips.get_mut(open.selected) {
            *slot = Some(chip);
        }
        if let Some(touched) = open.touched.get_mut(open.selected) {
            *touched = true;
        }
    }

    /// Flips "show all", only when a scope is set. The selection stays on
    /// its model when shown, else moves to the first row.
    pub(crate) fn toggle_show_all(&mut self) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if self.scoped.is_empty() {
            return;
        }
        open.show_all = !open.show_all;
        let (rows, _) = visible(&self.catalogue.models, &self.scoped, open.show_all);
        if !rows.is_empty()
            && !rows.contains(&open.selected)
            && let Some(first) = rows.first()
        {
            open.selected = *first;
        }
    }

    /// Asks `Every`: the refresh button refreshes every list, keeping
    /// whatever the queue holds wider.
    pub(crate) fn refresh(&mut self) {
        if self.open.is_some() {
            self.want = Some(Refresh::Every);
        }
    }
}

/// The rows in scope, by catalogue index, and the scope line: with no
/// scope, every row and no line. A set scope lists only its installed
/// entries in catalogue order until "show all" is toggled; with none of
/// them installed, no rows and a line saying so.
pub(crate) fn visible(
    models: &[ModelEntry],
    scoped: &[String],
    show_all: bool,
) -> (Vec<usize>, Option<String>) {
    if scoped.is_empty() || show_all {
        let rows = (0..models.len()).collect();
        let line =
            (!scoped.is_empty()).then(|| format!("all {} · Tab shows scoped_models", models.len()));
        return (rows, line);
    }
    let rows: Vec<usize> = models
        .iter()
        .enumerate()
        .filter(|(_, entry)| scoped.contains(&entry.reference))
        .map(|(index, _)| index)
        .collect();
    let line = if rows.is_empty() {
        format!(
            "None of scoped_models is installed. Tab shows all {}.",
            models.len()
        )
    } else {
        format!(
            "scoped_models: {} of {} · Tab shows all",
            rows.len(),
            models.len()
        )
    };
    (rows, Some(line))
}

/// The selected row's chip: its place in the entry's levels. The on-screen
/// model's row preselects the on-screen level when declared; every other
/// row preselects its configured level when declared, else its default
/// when declared, else no chip. An undeclared configured level is passed
/// over without a notice.
pub(crate) fn preselect(
    entry: &ModelEntry,
    on_screen: Option<(&str, Option<&str>)>,
) -> Option<usize> {
    if let Some((model, level)) = on_screen
        && entry.reference == model
        && let Some(level) = level
        && let Some(at) = entry.levels.iter().position(|declared| declared == level)
    {
        return Some(at);
    }
    if let Some(configured) = entry.configured.as_deref()
        && let Some(at) = entry
            .levels
            .iter()
            .position(|declared| declared == configured)
    {
        return Some(at);
    }
    if let Some(default) = entry.default_level.as_deref()
        && let Some(at) = entry.levels.iter().position(|declared| declared == default)
    {
        return Some(at);
    }
    None
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;
