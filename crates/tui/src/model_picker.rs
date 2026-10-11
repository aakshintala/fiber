//! The model picker's state (`docs/tui.md`, "Swapped views"): the
//! installed models, the scope they show under, the choice it sends and
//! the writes it waits on, and the read the loop owes. Each open starts
//! fresh; choosing sends one `model` command, and attached choices write
//! only when the session accepts it.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::catalogue::{Catalogue, ModelEntry, Price, Refresh};
use crate::keys::Key;
use crate::swapped::List;

mod filter;

/// What the picker was opened for: choosing a model and its level,
/// choosing a level for the current model, or marking the models a
/// `/scoped-models` save keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Choosing a model, and its thinking level.
    Choose,
    /// Choosing a thinking level for the current model: its row shows
    /// whatever the scope, and choosing it saves only the level.
    Thinking,
    /// Marking which installed models `scoped_models` keeps: every row
    /// shows whatever the scope, with a mark, and Enter saves the list.
    Scope,
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
    /// Whether the level was picked out: a chip click, or Enter or
    /// Ctrl+S on a touched row. Only then is the level saved.
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
    /// What the picker was opened for: choosing reads it for the
    /// current model's row.
    pub(crate) mode: Mode,
    /// The selected row's catalogue index.
    pub(crate) selected: usize,
    /// Each row's chip, by catalogue index: its level's place in the
    /// entry's levels, or no chip for a model with no levels or no
    /// declared level to preselect.
    pub(crate) chips: Vec<Option<usize>>,
    /// Each row touched by the chip keys, by catalogue index.
    pub(crate) touched: Vec<bool>,
    /// Each row marked for the saved list, by catalogue index: only the
    /// checklist keeps marks, starting from the saved list.
    pub(crate) marks: Vec<bool>,
    /// The on-screen model and level the open is for, kept until a read
    /// answers: an open before the first catalogue still lands on it.
    pub(crate) target: Option<(String, Option<String>)>,
    /// The scope toggle shows every installed model.
    pub(crate) show_all: bool,
    /// The filter query: what typing narrowed the list to. Empty at
    /// every open; the checklist never filters, so it stays empty there.
    pub(crate) query: String,
}

/// What the picker's overlay draws from.
pub(crate) struct PickerCtx<'a> {
    /// The last call's prompt size, what a switch rebuilds. `None` on
    /// home or with no call yet: then the cost column stays out.
    pub(crate) usage: Option<u64>,
    /// The loop's wall time, in milliseconds since the epoch.
    pub(crate) wall_ms: u64,
    /// This frame's spinner, for lists still refreshing.
    pub(crate) spinner: &'a str,
    /// The on-screen model's reference: its row shows "● current".
    pub(crate) current: Option<&'a str>,
    /// The home session-only choice's reference: its row draws the third row.
    pub(crate) home_only: Option<&'a str>,
}

/// The picker as its overlay draws it.
pub(crate) struct PickerView {
    pub(crate) filter: String,
    /// The buttons row's count.
    pub(crate) count: String,
    /// The scope toggle, or `None` with no scope or on the checklist.
    pub(crate) toggle: Option<String>,
    /// The buttons row's layout index (refresh is cell 0 there).
    pub(crate) buttons_at: usize,
    /// One section per shown provider, in list order.
    pub(crate) sections: Vec<Section>,
    /// The query hides every row: one dim line, no sections.
    pub(crate) no_match: bool,
    /// The lines under the list.
    pub(crate) status: Vec<String>,
    /// The legend's keys and labels.
    pub(crate) footer: Vec<(&'static str, &'static str)>,
    /// This frame's spinner, for lists still refreshing.
    pub(crate) spinner: String,
    /// The focused model row's layout index, else the first shown row.
    pub(crate) focused: Option<usize>,
}

/// One shown provider: its freshness and its models in list order.
pub(crate) struct Section {
    pub(crate) provider: String,
    pub(crate) state: Fresh,
    pub(crate) models: Vec<ModelRow>,
}

/// One shown provider's freshness.
pub(crate) enum Fresh {
    /// Its cached copy's age as the picker says it.
    Updated(String),
    /// Its list is refreshing now.
    Refreshing,
    /// It holds no cached copy.
    Unknown,
}

/// One shown model: what both its rows draw. `at` is the `RowAt` index
/// that `Spot::Cell(at, n)` and `select_frame_row` use.
pub(crate) struct ModelRow {
    pub(crate) at: usize,
    pub(crate) id: String,
    /// One flag per id character: whether the query matched it.
    pub(crate) hits: Vec<bool>,
    pub(crate) roles: Vec<String>,
    pub(crate) current: bool,
    /// Whether the scope keeps it, while the toggle shows every model.
    pub(crate) scoped: bool,
    /// Its rebuild cost, or "—" on the current model.
    pub(crate) cost: Option<String>,
    pub(crate) levels: Vec<String>,
    /// The saved level's place in `levels`, when one is declared.
    pub(crate) saved: Option<usize>,
    /// The chosen chip's place in `levels`, when the row names one.
    pub(crate) chip: Option<usize>,
    /// Whether the checklist keeps the row. `Some` only there.
    pub(crate) mark: Option<bool>,
    pub(crate) session_only: bool,
}

/// One layout row: the filter, the buttons, a provider heading, a model
/// by catalogue index, or the empty-filter line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowAt {
    /// The filter query, first in every choosing frame.
    Filter,
    /// The buttons row: refresh, the count, and the scope toggle.
    Buttons,
    /// A provider's heading, naming it.
    Heading(String),
    /// A model, by catalogue index.
    Model(usize),
    /// The typed query matches no model: one dim line, no sections.
    NoMatch,
}

/// The model picker: what it lists, and what it waits on.
#[derive(Default)]
pub(crate) struct ModelPicker {
    /// The installed models, from the latest read that answered.
    pub(crate) catalogue: Catalogue,
    /// Why the lists could not be read, when no catalogue is held yet.
    pub(crate) error: Option<String>,
    /// Which `Stale` or `Every` read runs, if one does: the picker shows
    /// "refreshing…" beside its lists.
    pub(crate) refreshing: Option<Refresh>,
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
    /// The session-only `model` commands waiting on their session's
    /// answer, by command id: the session and the reference.
    pub(crate) only_pending: HashMap<String, (contract::SessionId, String)>,
    /// The session-only choice its session accepted: the session and
    /// the reference, drawn while it is the one on screen.
    pub(crate) session_only: Option<(contract::SessionId, String)>,
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
        self.refreshing = None;
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
        let mut selected = selected_reference
            .and_then(|reference| models.iter().position(|entry| entry.reference == reference))
            .unwrap_or_else(|| {
                if old.models.is_empty() {
                    // An open before the first catalogue answered has no
                    // model to keep: it lands on the on-screen model, else
                    // the first row.
                    let rows = shown_in(
                        &self.catalogue.models,
                        &self.scoped,
                        open.show_all,
                        open.mode,
                        open.target.as_ref(),
                        &open.query,
                    );
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
        // A refresh that drops the selected row can leave the selection
        // on a hidden catalogue index while the frame highlights the
        // first shown row: move it onto the shown rows, so choosing
        // takes the highlighted row.
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
        if !rows.contains(&selected)
            && let Some(first) = rows.first()
        {
            selected = *first;
        }
        open.selected = selected;
        open.chips = chips;
        open.touched = touched;
        // A read answering over the checklist keeps each kept model's
        // mark, and a new row starts marked from the saved list, even
        // when the open began with no catalogue. Any other open keeps
        // no marks.
        open.marks = if open.mode == Mode::Scope {
            self.catalogue
                .models
                .iter()
                .map(|entry| {
                    old.models
                        .iter()
                        .position(|old_entry| old_entry.reference == entry.reference)
                        .and_then(|at| open.marks.get(at).copied())
                        .unwrap_or_else(|| self.scoped.contains(&entry.reference))
                })
                .collect()
        } else {
            Vec::new()
        };
    }

    /// The read the loop owes, if one is owed.
    pub(crate) fn take_read(&mut self) -> Option<Refresh> {
        let read = self.want.take();
        // Only a `Stale` or `Every` read shows "refreshing…": the startup
        // `Cached` read shows "Reading the model lists…" instead.
        if matches!(read, Some(Refresh::Stale | Refresh::Every)) {
            self.refreshing = read;
        }
        read
    }

    /// Opens the picker fresh: the selection on the on-screen model, else
    /// the first row; each row's chip at its preselected level, untouched;
    /// "show all" off. A `/thinking` open touches the current model's
    /// row, so Enter or Ctrl+S on it carries the chip's level. Each open asks
    /// `Stale`, keeping a wider `Every`.
    pub(crate) fn open(&mut self, mode: Mode, on_screen: Option<(&str, Option<&str>)>) {
        let target = on_screen.map(|(model, level)| (model.to_owned(), level.map(str::to_owned)));
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            false,
            mode,
            target.as_ref(),
            "",
        );
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
        let mut touched = vec![false; self.catalogue.models.len()];
        // The checklist starts marked from the saved list; any other
        // open keeps no marks.
        let marks = if mode == Mode::Scope {
            self.catalogue
                .models
                .iter()
                .map(|entry| self.scoped.contains(&entry.reference))
                .collect()
        } else {
            Vec::new()
        };
        // The current model's row starts touched: choosing it is a level
        // choice, saving only the level.
        if mode == Mode::Thinking
            && let Some((model, _)) = &target
            && let Some(at) = self
                .catalogue
                .models
                .iter()
                .position(|entry| &entry.reference == model)
            && let Some(touched) = touched.get_mut(at)
        {
            *touched = true;
        }
        self.open = Some(Open {
            mode,
            selected,
            chips,
            touched,
            marks,
            show_all: false,
            target,
            // Every open starts with an empty query.
            query: String::new(),
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
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
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
    /// row touched. A model with no levels has no chip to move, and with
    /// no row shown the selection sits hidden off the shown rows, so the
    /// chip keys change nothing.
    pub(crate) fn move_chip(&mut self, delta: isize) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        // Checklist rows have no level chips to move.
        if open.mode == Mode::Scope {
            return;
        }
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
        if !rows.contains(&open.selected) {
            return;
        }
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
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
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

    /// Whether the open picker is the `/scoped-models` checklist.
    pub(crate) fn is_scope(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.mode == Mode::Scope)
    }

    /// Flips the selected row's mark. Only the checklist keeps marks,
    /// so everywhere else this changes nothing.
    pub(crate) fn toggle_mark(&mut self) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        // The checklist shows every installed model, so the selection
        // always names a marked row.
        if let Some(mark) = open.marks.get_mut(open.selected) {
            *mark = !*mark;
        }
    }

    /// Flips "show all", only when a scope is set. The selection stays on
    /// its model when shown, else moves to the first row.
    pub(crate) fn toggle_show_all(&mut self) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        // The checklist already shows every installed model: the toggle
        // has nothing to show.
        if open.mode == Mode::Scope {
            return;
        }
        if self.scoped.is_empty() {
            return;
        }
        open.show_all = !open.show_all;
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
        restick(open, &rows);
    }

    /// Appends `c` to the filter query: typing narrows the choosing list
    /// on every letter. The selection stays on its model when it still
    /// shows, else moves to the first shown row. Nothing while closed.
    pub(crate) fn push_query(&mut self, c: char) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        open.query.push(c);
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
        restick(open, &rows);
    }

    /// Drops the query's last letter: Backspace shortens the filter.
    /// The selection stays on its model when it still shows, else moves
    /// to the first shown row. Nothing while closed or already empty.
    pub(crate) fn pop_query(&mut self) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.query.pop().is_none() {
            return;
        }
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
        restick(open, &rows);
    }

    /// Clears the filter query: the first Esc while one is typed. The
    /// selection stays on its model when it still shows, else moves to
    /// the first shown row. Nothing while closed or already empty.
    pub(crate) fn clear_query(&mut self) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.query.is_empty() {
            return;
        }
        open.query.clear();
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
        restick(open, &rows);
    }

    /// The filter query: what typing narrowed the list to. Empty while
    /// closed and at every open.
    pub(crate) fn query(&self) -> &str {
        self.open
            .as_ref()
            .map(|open| open.query.as_str())
            .unwrap_or("")
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
        let Some(layout) = self.layout() else {
            return;
        };
        if let Some(RowAt::Model(index)) = layout.get(frame_row)
            && let Some(open) = self.open.as_mut()
        {
            open.selected = *index;
        }
    }

    /// The open picker's choice at the selection: the row's reference at
    /// its chip, `level_chosen` exactly when the row was touched. A
    /// `/thinking` choice of the current model's own row saves only the
    /// level; any other row is an ordinary choice. `None` while closed,
    /// with no scoped row to choose, or while the checklist is open: it
    /// saves its marks with Enter and never chooses.
    pub(crate) fn choice(&self, session_only: bool) -> Option<Choice> {
        if self.is_scope() {
            return None;
        }
        let open = self.open.as_ref()?;
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
        // The selection can sit off the shown rows: choosing takes the
        // first shown row, the one the frame highlights.
        let selected = if rows.contains(&open.selected) {
            open.selected
        } else {
            rows.first().copied()?
        };
        let entry = self.catalogue.models.get(selected)?;
        let level = open
            .chips
            .get(selected)
            .copied()
            .flatten()
            .and_then(|chip| entry.levels.get(chip).cloned());
        let level_chosen = open.touched.get(selected).copied().unwrap_or(false);
        Some(choice_at(entry, open, level, level_chosen, session_only))
    }

    /// Clicks `cell` of `row`: the refresh button refreshes every list,
    /// the scope toggle flips it, a roles cell selects its row, a name
    /// cell chooses its row at its chip, and a chip chooses its row at
    /// that level. Choosing from a click always saves: Ctrl+S is the only
    /// path to a session-only choice. The filter and the empty line take
    /// no click.
    pub(crate) fn click_cell(&mut self, row: usize, cell: usize) -> Option<Choice> {
        let layout = self.layout()?;
        match layout.get(row) {
            Some(RowAt::Buttons) => {
                match cell {
                    0 => self.refresh(),
                    1 => self.toggle_show_all(),
                    _ => {}
                }
                None
            }
            Some(RowAt::Filter | RowAt::Heading(_) | RowAt::NoMatch) | None => None,
            Some(RowAt::Model(index)) => {
                let index = *index;
                // The checklist has no chips to choose: the mark cell
                // toggles its row, and any other cell selects it.
                if self.is_scope() {
                    let open = self.open.as_mut()?;
                    open.selected = index;
                    if cell == 0
                        && let Some(mark) = open.marks.get_mut(index)
                    {
                        *mark = !*mark;
                    }
                    return None;
                }
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
                    return Some(choice_at(entry, open, level, level_chosen, false));
                }
                if let Some(level) = cell.checked_sub(chips_at)
                    && level < levels
                {
                    if let Some(chip) = open.chips.get_mut(index) {
                        *chip = Some(level);
                    }
                    if let Some(touched) = open.touched.get_mut(index) {
                        *touched = true;
                    }
                    return Some(choice_at(
                        entry,
                        open,
                        entry.levels.get(level).cloned(),
                        true,
                        false,
                    ));
                }
                None
            }
        }
    }

    /// The picker as its overlay draws it: the filter, the count with
    /// its toggle, one section per shown provider, and the keys. `None`
    /// while closed.
    pub(crate) fn view(&self, ctx: &PickerCtx) -> Option<PickerView> {
        let open = self.open.as_ref()?;
        let layout = self.layout()?;
        let total = self.catalogue.models.len();
        let buttons_at = layout.iter().position(|row| *row == RowAt::Buttons)?;
        // The selection may sit off the shown rows after a read
        // answered, so it falls to the first model row shown.
        let focused = layout
            .iter()
            .position(|row| *row == RowAt::Model(open.selected))
            .or_else(|| layout.iter().position(|row| matches!(row, RowAt::Model(_))));
        // The shown models are the layout's model rows, in order.
        let mut shown = 0;
        let mut sections = Vec::new();
        for (at, row) in layout.iter().enumerate() {
            if let RowAt::Heading(provider) = row {
                let listed = self
                    .catalogue
                    .lists
                    .iter()
                    .find(|list| &list.provider == provider);
                let stale = listed.is_none_or(|list| list.stale);
                let refreshing = self.refreshing == Some(Refresh::Every)
                    || self.refreshing == Some(Refresh::Stale) && stale;
                sections.push(Section {
                    provider: provider.clone(),
                    state: if refreshing {
                        Fresh::Refreshing
                    } else {
                        match listed.and_then(|list| list.updated_ms) {
                            Some(updated) => {
                                Fresh::Updated(age(ctx.wall_ms.saturating_sub(updated)))
                            }
                            None => Fresh::Unknown,
                        }
                    },
                    models: Vec::new(),
                });
            } else if let RowAt::Model(index) = row
                && let Some(entry) = self.catalogue.models.get(*index)
                && let Some(section) = sections.last_mut()
            {
                shown += 1;
                let current = ctx.current.is_some_and(|model| model == entry.reference);
                let only = self
                    .session_only
                    .as_ref()
                    .is_some_and(|(_, r)| *r == entry.reference)
                    || ctx.home_only.is_some_and(|r| r == entry.reference);
                section.models.push(ModelRow {
                    at,
                    id: entry.id.clone(),
                    hits: filter::id_hits(&entry.id, &open.query),
                    roles: entry.roles.clone(),
                    current,
                    scoped: open.show_all && self.scoped.contains(&entry.reference),
                    cost: ctx.usage.map(|tokens| {
                        if current {
                            "—".to_owned()
                        } else {
                            rebuild_cost(tokens, entry.price.as_ref())
                        }
                    }),
                    levels: entry.levels.clone(),
                    saved: entry
                        .configured
                        .as_ref()
                        .and_then(|saved| entry.levels.iter().position(|level| level == saved)),
                    chip: open.chips.get(*index).copied().flatten(),
                    mark: (open.mode == Mode::Scope)
                        .then(|| open.marks.get(*index).copied())
                        .flatten(),
                    session_only: only,
                });
            }
        }
        Some(PickerView {
            filter: open.query.clone(),
            count: if !open.query.is_empty() {
                format!("{shown} of {total} models")
            } else if open.mode != Mode::Scope && !self.scoped.is_empty() && !open.show_all {
                format!("scoped · {shown} of {total}")
            } else {
                format!("{shown} models")
            },
            toggle: (open.mode != Mode::Scope && !self.scoped.is_empty()).then(|| {
                if open.show_all {
                    "[show scoped]"
                } else {
                    "[show all]"
                }
                .to_owned()
            }),
            buttons_at,
            sections,
            no_match: !open.query.is_empty() && shown == 0 && total > 0,
            status: self.status(),
            spinner: ctx.spinner.to_owned(),
            // The checklist marks rows and saves the list: no model
            // or level is chosen here.
            footer: if open.mode == Mode::Scope {
                vec![("Space", "mark"), ("Enter", "save"), ("Esc", "back")]
            } else {
                vec![
                    ("↑↓", "move"),
                    ("←→", "levels"),
                    ("Enter", "choose"),
                    ("Tab", "all"),
                    ("Ctrl+S", "session"),
                    ("Ctrl+R", "refresh"),
                    ("Esc", "close"),
                ]
            },
            // The selection may sit off the shown rows after a read
            // answered, so it falls to the first model row shown.
            focused,
        })
    }

    /// The layout rows in draw order: the filter (never on the
    /// checklist), the buttons, headings with their models, or the
    /// empty-filter line. `None` while closed.
    fn layout(&self) -> Option<Vec<RowAt>> {
        let open = self.open.as_ref()?;
        let rows = shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        );
        // The checklist takes no filter: its frame starts at the buttons
        // with no filter row, so no row number shifts there.
        let mut layout = if open.mode == Mode::Scope {
            vec![RowAt::Buttons]
        } else {
            vec![RowAt::Filter, RowAt::Buttons]
        };
        // A typed query hiding every installed model shows one dim line
        // and no provider sections.
        if !open.query.is_empty() && rows.is_empty() && !self.catalogue.models.is_empty() {
            layout.push(RowAt::NoMatch);
            return Some(layout);
        }
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
        Some(layout)
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
            if self.want.is_some() || self.refreshing.is_some() {
                return vec!["Reading the model lists…".to_owned()];
            }
            return vec![
                "No models. Install a provider: fiber extension install <name>.".to_owned(),
            ];
        }
        if self.refreshing.is_some() {
            return vec!["refreshing…".to_owned()];
        }
        Vec::new()
    }
}

/// A switch's rebuild size at `tokens` with `price`, and the rebuild's
/// price when the model names one. The tier is the highest whose
/// `input_tokens_above` is below the token count, else the base price.
pub(crate) fn rebuild_cost(tokens: u64, price: Option<&Price>) -> String {
    let size = if tokens < 1000 {
        format!("~{tokens} tokens")
    } else {
        format!("~{}k tokens", tokens.saturating_add(500) / 1000)
    };
    let Some(price) = price else {
        return size;
    };
    let micros = price
        .tiers
        .iter()
        .rev()
        .find(|(above, _)| *above < tokens)
        .map(|(_, micros)| *micros)
        .unwrap_or(price.micros_per_mtok);
    let dollars = tokens as f64 * micros as f64 / 1_000_000_000_000.0;
    format!("{size} · {}", crate::format::money(dollars))
}

/// An age in milliseconds as the picker says it.
pub(crate) fn age(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
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

/// What the `/scoped-models` checklist saves, in order: the marked
/// references in catalogue order, then the old list's entries that are
/// not installed, in their old order. An entry that is installed but
/// unmarked is dropped. With none marked, nothing: written as `[]`, an
/// empty list reads as every model (`docs/configuration.md`, "Keys").
pub(crate) fn scoped_save(models: &[ModelEntry], marks: &[bool], old: &[String]) -> Vec<String> {
    let picked: Vec<String> = models
        .iter()
        .enumerate()
        .filter(|(at, _)| marks.get(*at).copied().unwrap_or(false))
        .map(|(_, entry)| entry.reference.clone())
        .collect();
    if picked.is_empty() {
        // Marking none clears the list, dropping every old entry.
        return Vec::new();
    }
    let mut out = picked;
    for name in old {
        if !models.iter().any(|entry| &entry.reference == name) {
            out.push(name.clone());
        }
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

/// A choice of `entry` at `level`: the reference byte for byte, with
/// the `/thinking` current-row rule on `save_model`.
fn choice_at(
    entry: &ModelEntry,
    open: &Open,
    level: Option<String>,
    level_chosen: bool,
    session_only: bool,
) -> Choice {
    // The person asked about thinking: choosing the current model's own
    // row saves only the level, never the model.
    let save_model = match (&open.mode, &open.target) {
        (Mode::Thinking, Some((model, _))) => *model != entry.reference,
        _ => true,
    };
    Choice {
        reference: entry.reference.clone(),
        level,
        level_chosen,
        save_model,
        session_only,
    }
}

/// Moves the selection onto the shown rows: it stays where it was when
/// that row still shows, else moves to the first shown row. With no row
/// shown it stays where it was, hidden.
fn restick(open: &mut Open, rows: &[usize]) {
    if !rows.is_empty()
        && !rows.contains(&open.selected)
        && let Some(first) = rows.first()
    {
        open.selected = *first;
    }
}

/// The rows an open picker shows, by catalogue index: the scoped rows,
/// with a `/thinking` open adding the current model's row in catalogue
/// order whatever the scope, and the filter dropping whatever the query
/// hides last, keeping catalogue order.
pub(crate) fn shown_in(
    models: &[ModelEntry],
    scoped: &[String],
    show_all: bool,
    mode: Mode,
    target: Option<&(String, Option<String>)>,
    query: &str,
) -> Vec<usize> {
    // The checklist opens over every installed model whatever the
    // scope, with no toggle and no filter.
    if mode == Mode::Scope {
        return (0..models.len()).collect();
    }
    let mut rows = visible(models, scoped, show_all);
    if mode == Mode::Thinking
        && let Some((model, _)) = target
        && let Some(at) = models.iter().position(|entry| &entry.reference == model)
    {
        let insertion = rows.partition_point(|index| *index < at);
        if rows.get(insertion) != Some(&at) {
            rows.insert(insertion, at);
        }
    }
    // The filter runs last and only drops rows: an empty query matches
    // every row.
    rows.retain(|index| {
        models
            .get(*index)
            .is_some_and(|entry| filter::matches(entry, query))
    });
    rows
}

/// The rows in scope, by catalogue index: with no scope, or past the
/// "show all" toggle, every row. A set scope lists only its installed
/// entries in catalogue order.
pub(crate) fn visible(models: &[ModelEntry], scoped: &[String], show_all: bool) -> Vec<usize> {
    if scoped.is_empty() || show_all {
        return (0..models.len()).collect();
    }
    models
        .iter()
        .enumerate()
        .filter(|(_, entry)| scoped.contains(&entry.reference))
        .map(|(index, _)| index)
        .collect()
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
