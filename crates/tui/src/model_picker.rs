//! The model picker's state (`docs/tui.md`, "Swapped views"): the
//! installed models, the scope they show under, the choice it sends and
//! the writes it waits on, and the read the loop owes. Each open starts
//! fresh; choosing sends one `model` command, and attached choices write
//! only when the session accepts it.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::catalogue::{Catalogue, ModelEntry, Refresh};
use crate::keys::Key;
use crate::swapped::{Frame, Ink, List, Spot, about, rows_height};

/// What the picker was opened for: choosing a model and its level, or
/// choosing a level for the current model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Choosing a model, and its thinking level.
    Choose,
}

/// One choice: the model and level under the cursor, whether the level
/// was picked out, whether the global `model` is saved, and whether the
/// choice holds for this session only, saving nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    /// The chosen model's reference, byte for byte.
    pub(crate) reference: String,
    /// The chosen level, `None` for a model with no levels.
    pub(crate) level: Option<String>,
    /// Whether the level was picked out: a chip click, or Enter or `s`
    /// on a touched row. Only then is the level saved.
    pub(crate) level_chosen: bool,
    /// Whether the global `model` is saved: false only for a level
    /// picked for the current model.
    pub(crate) save_model: bool,
    /// Whether the choice holds for this session only: then nothing is
    /// saved.
    pub(crate) session_only: bool,
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
    /// The on-screen model and level the open is for, kept until a read
    /// answers: an open before the first catalogue still lands on it.
    pub(crate) target: Option<(String, Option<String>)>,
    /// The scope toggle shows every installed model.
    pub(crate) show_all: bool,
}

/// One frame row: the buttons, a provider heading, or a model by
/// catalogue index. Clicks map back through it, so headings are never
/// stops.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowAt {
    /// The refresh button and the scope line.
    Buttons,
    /// A provider's heading, naming it.
    Heading(String),
    /// A model, by catalogue index.
    Model(usize),
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
    /// The writes each sent `model` command waits on, by its id: what
    /// the session's acceptance writes, in order. An answer touches only
    /// its own entry.
    pub(crate) awaiting: HashMap<String, Vec<(String, String)>>,
    /// The session-only choice on home: the next `start` carries it.
    pub(crate) start_model: Option<Choice>,
    /// The open picker, if one is open.
    pub(crate) open: Option<Open>,
}

impl ModelPicker {
    /// Stores a read's answer: the catalogue, or the error with the old
    /// catalogue kept. What the loop owes is taken, never cleared here:
    /// an answer to the startup read must not drop a meanwhile opened
    /// picker's `Stale`. A replacement reconciles the open picker by
    /// model reference: the selection stays on its model, each touched
    /// chip stays on its level's name, every other row preselects, and
    /// a removed selection clamps into the answered catalogue.
    pub(crate) fn store(&mut self, result: Result<Catalogue, String>) {
        self.refreshing = false;
        let incoming = match result {
            Ok(catalogue) => catalogue,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let target = self.open.as_ref().and_then(|open| open.target.clone());
        let old = std::mem::replace(&mut self.catalogue, incoming);
        self.error = None;
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let on_screen = target
            .as_ref()
            .map(|(model, level)| (model.as_str(), level.as_deref()));
        let models = &self.catalogue.models;
        let mut chips = Vec::with_capacity(models.len());
        let mut touched = Vec::with_capacity(models.len());
        for entry in models {
            let old_at = old
                .models
                .iter()
                .position(|old_entry| old_entry.reference == entry.reference);
            let was_touched = old_at
                .and_then(|at| open.touched.get(at).copied())
                .unwrap_or(false);
            let chip = if !was_touched {
                preselect(entry, on_screen)
            } else {
                let level = old_at.and_then(|at| {
                    open.chips
                        .get(at)
                        .copied()
                        .flatten()
                        .and_then(|chip| {
                            old.models
                                .get(at)
                                .and_then(|old_entry| old_entry.levels.get(chip))
                        })
                        .cloned()
                });
                level
                    .as_ref()
                    .and_then(|level| entry.levels.iter().position(|declared| declared == level))
                    .or_else(|| preselect(entry, on_screen))
            };
            chips.push(chip);
            touched.push(was_touched);
        }
        let selected_reference = old
            .models
            .get(open.selected)
            .map(|entry| entry.reference.clone());
        let selected = selected_reference
            .and_then(|reference| models.iter().position(|entry| entry.reference == reference))
            .unwrap_or_else(|| {
                if old.models.is_empty() {
                    // An open before the first catalogue answered has no
                    // model to keep: it lands on the on-screen model, else
                    // the first row.
                    let (rows, _) = visible(&self.catalogue.models, &self.scoped, open.show_all);
                    on_screen
                        .and_then(|(model, _)| {
                            rows.iter().copied().find(|index| {
                                self.catalogue
                                    .models
                                    .get(*index)
                                    .is_some_and(|entry| entry.reference == model)
                            })
                        })
                        .or(rows.first().copied())
                        .unwrap_or(0)
                } else {
                    open.selected.min(models.len().saturating_sub(1))
                }
            });
        open.selected = selected;
        open.chips = chips;
        open.touched = touched;
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
            target: on_screen.map(|(model, level)| (model.to_owned(), level.map(str::to_owned))),
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

    /// Moves the selection by a page through the list's own page step:
    /// the shown height less one, clamped at both ends. Headings are
    /// not stops, and any other key leaves the selection where it was.
    pub(crate) fn move_page(&mut self, key: &Key, height: usize) {
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
        let mut list = List::default();
        list.select(at, rows.len(), height);
        if list.key(key, rows.len(), height)
            && let Some(selected) = rows.get(list.selected())
        {
            open.selected = *selected;
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

    /// Selects the model `frame_row` names. Headings and the buttons are
    /// not stops, so they select nothing.
    pub(crate) fn select_frame_row(&mut self, frame_row: usize) {
        let Some((layout, _)) = self.layout() else {
            return;
        };
        if let Some(RowAt::Model(index)) = layout.get(frame_row)
            && let Some(open) = self.open.as_mut()
        {
            open.selected = *index;
        }
    }

    /// The open picker's choice at the selection: the row's reference at
    /// its chip, `level_chosen` exactly when the row was touched. `None`
    /// while closed, or with no scoped row to choose.
    pub(crate) fn choice(&self, session_only: bool) -> Option<Choice> {
        let open = self.open.as_ref()?;
        let (rows, _) = visible(&self.catalogue.models, &self.scoped, open.show_all);
        if rows.is_empty() {
            return None;
        }
        let entry = self.catalogue.models.get(open.selected)?;
        let level = open
            .chips
            .get(open.selected)
            .copied()
            .flatten()
            .and_then(|chip| entry.levels.get(chip).cloned());
        let level_chosen = open.touched.get(open.selected).copied().unwrap_or(false);
        Some(Choice {
            reference: entry.reference.clone(),
            level,
            level_chosen,
            save_model: true,
            session_only,
        })
    }

    /// Clicks `cell` of `row`: the refresh button refreshes every list,
    /// the scope line toggles it, a roles cell selects its row, a name
    /// cell chooses its row at its chip, and a chip chooses its row at
    /// that level. Choosing from a click always saves: `s` is the only
    /// path to a session-only choice.
    pub(crate) fn click_cell(&mut self, row: usize, cell: usize) -> Option<Choice> {
        let (layout, _) = self.layout()?;
        match layout.get(row) {
            Some(RowAt::Buttons) => {
                match cell {
                    0 => self.refresh(),
                    1 => self.toggle_show_all(),
                    _ => {}
                }
                None
            }
            Some(RowAt::Heading(_)) | None => None,
            Some(RowAt::Model(index)) => {
                let index = *index;
                let entry = self.catalogue.models.get(index)?;
                // The chips start past the name, and past the roles when
                // the row shows any; a click on the name chooses the row
                // at its chip, while the roles cell only selects.
                let chips_at = if entry.roles.is_empty() { 1 } else { 2 };
                let levels = entry.levels.len();
                let open = self.open.as_mut()?;
                open.selected = index;
                if cell == 0 {
                    let level = open
                        .chips
                        .get(index)
                        .copied()
                        .flatten()
                        .and_then(|chip| entry.levels.get(chip).cloned());
                    let level_chosen = open.touched.get(index).copied().unwrap_or(false);
                    Some(Choice {
                        reference: entry.reference.clone(),
                        level,
                        level_chosen,
                        save_model: true,
                        session_only: false,
                    })
                } else if let Some(level) = cell.checked_sub(chips_at)
                    && level < levels
                {
                    if let Some(chip) = open.chips.get_mut(index) {
                        *chip = Some(level);
                    }
                    if let Some(touched) = open.touched.get_mut(index) {
                        *touched = true;
                    }
                    Some(Choice {
                        reference: entry.reference.clone(),
                        level: entry.levels.get(level).cloned(),
                        level_chosen: true,
                        save_model: true,
                        session_only: false,
                    })
                } else {
                    None
                }
            }
        }
    }

    /// The frame to draw at `height`: the buttons, the scoped providers
    /// with their models and chips, the status below, and the keys.
    /// `usage` is the last call's prompt size on the session on screen,
    /// which names the header's rebuild size. `None` while closed.
    pub(crate) fn frame(&self, height: usize, usage: Option<u64>) -> Option<Frame> {
        let open = self.open.as_ref()?;
        let (layout, scope) = self.layout()?;
        let title = match usage {
            Some(tokens) => format!(
                "Models · switching rebuilds the cache: about {} tokens",
                about(tokens)
            ),
            None => "Models".to_owned(),
        };
        let rows = layout
            .iter()
            .enumerate()
            .map(|(at, row)| self.cells(at, row, scope.as_deref()))
            .collect::<Vec<_>>();
        let mut frame = Frame {
            title,
            rows,
            list: List::default(),
            below: self.status(),
            field: None,
            footer: "Enter set as default · s this session only · ↑↓ move · ←→ level · PageUp PageDown page · Tab scope · Ctrl+R refresh · Esc close"
                .to_owned(),
        };
        // The selection may sit off the scoped rows after a read
        // answered, so it falls to the first model row shown.
        let selected = layout
            .iter()
            .position(|row| *row == RowAt::Model(open.selected))
            .or_else(|| layout.iter().position(|row| matches!(row, RowAt::Model(_))))
            .unwrap_or(0);
        frame
            .list
            .select(selected, layout.len(), rows_height(&frame, height));
        Some(frame)
    }

    /// The frame's rows with the scope line: the buttons, then each
    /// scoped provider's heading with its models in catalogue order.
    /// `None` while the picker is closed.
    fn layout(&self) -> Option<(Vec<RowAt>, Option<String>)> {
        let open = self.open.as_ref()?;
        let (rows, scope) = visible(&self.catalogue.models, &self.scoped, open.show_all);
        let mut layout = vec![RowAt::Buttons];
        let mut provider: Option<&str> = None;
        for index in rows {
            let Some(entry) = self.catalogue.models.get(index) else {
                continue;
            };
            if provider != Some(entry.provider.as_str()) {
                provider = Some(entry.provider.as_str());
                layout.push(RowAt::Heading(entry.provider.clone()));
            }
            layout.push(RowAt::Model(index));
        }
        Some((layout, scope))
    }

    /// One frame row's cells: the buttons with their targets, a heading,
    /// or a model's name, roles and chips, each with its own target and
    /// the row's chip in brackets.
    fn cells(
        &self,
        at: usize,
        row: &RowAt,
        scope: Option<&str>,
    ) -> Vec<(String, Option<Spot>, Ink)> {
        match row {
            RowAt::Buttons => {
                let mut cells = vec![("↻ refresh".to_owned(), Some(Spot::Cell(at, 0)), Ink::Plain)];
                if let Some(line) = scope {
                    cells.push((format!("  {line}"), Some(Spot::Cell(at, 1)), Ink::Plain));
                }
                cells
            }
            RowAt::Heading(provider) => vec![(provider.clone(), None, Ink::Heading)],
            RowAt::Model(index) => {
                let Some(entry) = self.catalogue.models.get(*index) else {
                    return Vec::new();
                };
                let chip = self
                    .open
                    .as_ref()
                    .and_then(|open| open.chips.get(*index).copied().flatten());
                let mut cells = vec![(
                    if entry.roles.is_empty() {
                        format!("{}   ", entry.id)
                    } else {
                        entry.id.clone()
                    },
                    Some(Spot::Cell(at, 0)),
                    Ink::Plain,
                )];
                let mut cell = 1;
                if !entry.roles.is_empty() {
                    cells.push((
                        format!(" · {}   ", entry.roles.join(", ")),
                        Some(Spot::Cell(at, cell)),
                        Ink::Muted,
                    ));
                    cell += 1;
                }
                for (level_at, level) in entry.levels.iter().enumerate() {
                    let text = if chip == Some(level_at) {
                        format!("[{level}] ")
                    } else {
                        format!("{level} ")
                    };
                    cells.push((text, Some(Spot::Cell(at, cell)), Ink::Plain));
                    cell += 1;
                }
                cells
            }
        }
    }

    /// The lines below the rows: a read error with no catalogue yet shows
    /// the error; an empty catalogue otherwise says the lists are coming
    /// while a read is out or owed, and that no provider is installed
    /// once one answered; a running refresh shows beside its catalogue.
    fn status(&self) -> Vec<String> {
        if self.catalogue.models.is_empty() {
            if let Some(error) = &self.error {
                return vec![error.clone()];
            }
            if self.want.is_some() || self.refreshing {
                return vec!["Reading the model lists…".to_owned()];
            }
            return vec![
                "No models. Install a provider: fiber extension install <name>.".to_owned(),
            ];
        }
        if self.refreshing {
            return vec!["refreshing…".to_owned()];
        }
        Vec::new()
    }
}

/// What a choice writes through the configuration seam, in order: the
/// global `model` when the model is saved, then the model's thinking
/// level when one was picked out. A session-only choice saves nothing.
/// A key's reference is the entry's `reference` byte for byte.
pub(crate) fn saves(choice: &Choice) -> Vec<(String, String)> {
    if choice.session_only {
        return Vec::new();
    }
    let mut out = Vec::new();
    if choice.save_model {
        out.push(("model".to_owned(), choice.reference.clone()));
    }
    if choice.level_chosen
        && let Some(level) = &choice.level
    {
        out.push((
            format!("models.\"{}\".thinking", choice.reference),
            level.clone(),
        ));
    }
    out
}

/// A choice as the `model` command's args: `thinking` rides along
/// exactly when the choice names a level.
pub(crate) fn command_args(choice: &Choice) -> Value {
    match &choice.level {
        Some(level) => json!({"model": choice.reference, "thinking": level}),
        None => json!({"model": choice.reference}),
    }
}

/// A session-only choice on home as the next `start` carries it: the
/// model exactly, and the level as a per-run override, outranking every
/// file. No `:level` suffix: the session tries the exact string first.
pub(crate) fn start_args(choice: &Choice) -> (String, Option<String>) {
    let override_text = choice
        .level
        .as_ref()
        .map(|level| format!("models.\"{}\".thinking={level}", choice.reference));
    (choice.reference.clone(), override_text)
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

#[cfg(test)]
#[path = "model_picker_view_tests.rs"]
mod view_tests;
