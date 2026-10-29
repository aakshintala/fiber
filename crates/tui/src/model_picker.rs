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
use crate::text::controls_to_space;

mod filter;
#[cfg(test)]
pub(crate) mod fixtures;
mod install;

use install::Stop;
pub(crate) use install::{InstallStart, Installs, Landed, Outcome, Progress, credential_text};

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
    /// shows whatever the scope, with a mark. Typing filters the list
    /// as in the picker, Enter toggles a mark, and Ctrl+S saves.
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
    /// Each row the person's keys or clicks touched, by catalogue
    /// index: once touched a row keeps the person's mark, so a row
    /// they never touched adopts the saved list, while a read
    /// answering over the open keeps each kept model's mark, and a
    /// new row starts untouched.
    pub(crate) toggled: Vec<bool>,
    /// The on-screen model and level the open is for, kept until a read
    /// answers: an open before the first catalogue still lands on it.
    pub(crate) target: Option<(String, Option<String>)>,
    /// The scope toggle shows every installed model.
    pub(crate) show_all: bool,
    /// The filter query: what typing narrowed the list to. Empty at
    /// every open; Space counts as a typed query on the checklist too.
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
    /// The buttons row's layout index (the filter is row 0 there).
    pub(crate) buttons_at: usize,
    /// One section per shown provider, in list order.
    pub(crate) sections: Vec<Section>,
    /// The query hides every row: one dim line, no sections.
    pub(crate) no_match: bool,
    /// The lines under the list.
    pub(crate) status: Vec<String>,
    /// The legend's keys and labels.
    pub(crate) footer: Vec<(&'static str, &'static str)>,
    /// Whether the buttons row draws the refresh button and its click
    /// target: false only on the checklist, which has no refresh button.
    pub(crate) refresh: bool,
    /// This frame's spinner, for lists still refreshing.
    pub(crate) spinner: String,
    /// The focused model row's layout index, else the first shown row.
    pub(crate) focused: Option<usize>,
    /// One row per not-installed provider the picker lists, in order.
    pub(crate) installs: Vec<InstallRow>,
    /// What landed installs show under the list.
    pub(crate) outcomes: Vec<Outcome>,
}

/// One shown provider: its freshness and its models in list order.
pub(crate) struct Section {
    pub(crate) provider: String,
    pub(crate) state: Fresh,
    pub(crate) models: Vec<ModelRow>,
    /// One count row per left-out group the query keeps, in provider
    /// then setting order.
    pub(crate) left_out: Vec<LeftOutRow>,
}

/// One left-out group's count row: what both its rows draw. `at` is
/// the `RowAt` index that `select_frame_row` and `click_cell` ignore.
pub(crate) struct LeftOutRow {
    pub(crate) at: usize,
    pub(crate) text: String,
}

/// One left-out group the open picker counts: its provider and
/// setting, and its count row's text.
pub(crate) struct ShownGroup {
    pub(crate) provider: String,
    pub(crate) setting: String,
    pub(crate) text: String,
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
    /// Its input and output prices at the session's size.
    pub(crate) price: Option<String>,
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

/// One not-installed provider's row. `at` is the `RowAt` index that
/// `Spot::Row(at)` and `Spot::Cell(at, 0)` use.
pub(crate) struct InstallRow {
    pub(crate) at: usize,
    /// The provider's name, control characters turned to spaces.
    pub(crate) provider: String,
    /// One flag per name character: whether the query matched it.
    pub(crate) hits: Vec<bool>,
    /// `None` is "not installed".
    pub(crate) progress: Option<Progress>,
}

/// One layout row: the filter, the buttons, a provider heading, a model
/// by catalogue index, a not-installed provider, or the empty-filter line.
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
    /// A not-installed provider, by name, after the last model.
    Install(String),
    /// A placeholder-with-no-value count row, naming its provider and
    /// setting: never a stop, so focus skips it and clicks ignore it.
    LeftOut { provider: String, setting: String },
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
    /// The catalogue reads owed but not yet answered, at most two:
    /// the reader runs one read with at most one queued behind it, and
    /// a failed spawn stays busy until its errors are consumed, so takes
    /// past that merge into the queued slot and answer once. Each
    /// answer counts one down; the mark clears only when none is owed,
    /// so a queued read starting after an older answer keeps showing
    /// "refreshing…" until its own answer lands.
    owed: usize,
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
    /// Every sent `model` command's choice, by its id: the reference and
    /// the level, for the pending notice a send while a turn runs
    /// pushes. An answer takes its own entry: acceptance discards it,
    /// and rejection replaces its notice. Entries from dropped sessions
    /// linger until their answer takes them.
    pub(crate) sent: HashMap<String, (String, Option<String>)>,
    /// The session-only choice its session accepted: the session and
    /// the reference, drawn while it is the one on screen.
    pub(crate) session_only: Option<(contract::SessionId, String)>,
    /// The session-only choice on home: the next `start` carries it.
    pub(crate) start_model: Option<Choice>,
    /// The open picker, if one is open.
    pub(crate) open: Option<Open>,
    /// The not-installed rows: the install that runs, the failures, the
    /// outcomes and the row focus. A running install outlives an open.
    pub(crate) installs: Installs,
}

impl ModelPicker {
    /// Adopts a new scoped list while the open stays: a row the person
    /// never touched adopts the new list, and a toggled row keeps the
    /// person's mark.
    pub(crate) fn adopt_scoped(&mut self, scoped: Vec<String>) {
        self.scoped = scoped;
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.mode != Mode::Scope {
            return;
        }
        for (index, entry) in self.catalogue.models.iter().enumerate() {
            if open.toggled.get(index).copied().unwrap_or(false) {
                continue;
            }
            if let Some(mark) = open.marks.get_mut(index) {
                *mark = self.scoped.contains(&entry.reference);
            }
        }
    }
    /// Stores a read's answer: the catalogue, or the error with the old
    /// catalogue kept. What the loop owes is taken, never cleared here:
    /// an answer to the startup read must not drop a meanwhile opened
    /// picker's `Stale`. One owed read answered counts the owed reads
    /// down, and the mark clears only when none is owed: an older
    /// answer never drops a queued read's mark. A replacement reconciles
    /// the open picker by model reference: the selection stays on its
    /// model, each touched chip stays on its level's name, every other
    /// row preselects, and a removed selection clamps into the answered
    /// catalogue.
    pub(crate) fn store(&mut self, result: Result<Catalogue, String>) {
        self.owed = self.owed.saturating_sub(1);
        if self.owed == 0 {
            self.refreshing = None;
        }
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
        self.prune_focus();
        let on_screen = target
            .as_ref()
            .map(|(model, level)| (model.as_str(), level.as_deref()));
        // Reconcile the answered catalogue under one short borrow: each
        // touched chip stays on its level's name, every other row
        // preselects, the selection keeps its model, and the checklist
        // keeps each kept mark. The rows computation waits until the
        // borrow ends.
        let (chips, touched, kept, old_empty, open_selected, model_count, toggled, marks) = {
            let Some(open) = self.open.as_mut() else {
                return;
            };
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
                        .and_then(|level| {
                            entry.levels.iter().position(|declared| declared == level)
                        })
                        .or_else(|| preselect(entry, on_screen))
                };
                chips.push(chip);
                touched.push(was_touched);
            }
            let kept = old.models.get(open.selected).and_then(|kept| {
                models
                    .iter()
                    .position(|entry| entry.reference == kept.reference)
            });
            (
                chips,
                touched,
                kept,
                old.models.is_empty(),
                open.selected,
                models.len(),
                // A read answering over the checklist keeps each kept
                // model's mark, and a new row starts marked from the saved
                // list, even when the open began with no catalogue. Any
                // other open keeps no marks. The touches carry the same
                // way, except a new row starts untouched: the person never
                // touched it.
                if open.mode == Mode::Scope {
                    self.catalogue
                        .models
                        .iter()
                        .map(|entry| {
                            old.models
                                .iter()
                                .position(|old_entry| old_entry.reference == entry.reference)
                                .and_then(|at| open.toggled.get(at).copied())
                                .unwrap_or(false)
                        })
                        .collect()
                } else {
                    Vec::new()
                },
                if open.mode == Mode::Scope {
                    let mut marks = Vec::with_capacity(models.len());
                    for entry in models {
                        marks.push(
                            old.models
                                .iter()
                                .position(|old_entry| old_entry.reference == entry.reference)
                                .and_then(|at| open.marks.get(at).copied())
                                .unwrap_or_else(|| self.scoped.contains(&entry.reference)),
                        );
                    }
                    marks
                } else {
                    Vec::new()
                },
            )
        };
        let rows = {
            let Some(open) = self.open.as_ref() else {
                return;
            };
            self.shown(open)
        };
        let mut selected = if old_empty {
            // An open before the first catalogue answered has no model
            // to keep: it lands on the on-screen model, else the first
            // row.
            self.selected_for(on_screen, &rows)
        } else {
            kept.unwrap_or(open_selected)
                .min(model_count.saturating_sub(1))
        };
        // A refresh that drops the selected row can leave the selection
        // on a hidden catalogue index while the frame highlights the
        // first shown row: move it onto the shown rows, so choosing
        // takes the highlighted row.
        if !rows.contains(&selected)
            && let Some(first) = rows.first()
        {
            selected = *first;
        }
        let Some(open) = self.open.as_mut() else {
            return;
        };
        open.selected = selected;
        open.chips = chips;
        open.touched = touched;
        open.toggled = toggled;
        open.marks = marks;
    }

    /// The read the loop owes, if one is owed. Taking a read counts one
    /// more owed while fewer than two are: the reader runs one read with
    /// at most one queued behind it, so takes past that merge into the
    /// queued slot and answer once. Only a `Stale` or `Every` read shows
    /// "refreshing…": the startup `Cached` read shows "Reading the
    /// model lists…" instead.
    pub(crate) fn take_read(&mut self) -> Option<Refresh> {
        let read = self.want.take();
        if read.is_some() {
            self.count_owed();
        }
        if matches!(read, Some(Refresh::Stale | Refresh::Every)) {
            self.refreshing = read;
        }
        read
    }

    /// Counts the startup read as owed while fewer than two are: the
    /// loop asks it directly, not through `take_read`, so its answer
    /// counts down with the rest. An answer to the startup read landing
    /// after a meanwhile taken read then keeps that read's mark, instead
    /// of clearing it early.
    pub(crate) fn note_asked(&mut self) {
        self.count_owed();
    }

    /// Counts one more owed read, up to the two the reader can owe at
    /// once: its running read and its one queued slot. A failed spawn
    /// counts as the running one until its errors are consumed, so the
    /// bound holds with no assumed cap on failures: takes past that
    /// merge into the queued slot, so they answer once and count nothing
    /// more; each answer then pairs with exactly one counted read.
    fn count_owed(&mut self) {
        self.owed = (self.owed + 1).min(2);
    }

    /// Opens the picker fresh: the selection on the on-screen model, else
    /// the first row; each row's chip at its preselected level, untouched;
    /// "show all" off. A `/thinking` open touches the current model's
    /// row, so Enter or Ctrl+S on it carries the chip's level. Each open asks
    /// `Stale`, keeping a wider `Every`.
    pub(crate) fn open(&mut self, mode: Mode, on_screen: Option<(&str, Option<&str>)>) {
        let target = on_screen.map(|(model, level)| (model.to_owned(), level.map(str::to_owned)));
        let chips = self
            .catalogue
            .models
            .iter()
            .map(|entry| preselect(entry, on_screen))
            .collect();
        let mut touched = vec![false; self.catalogue.models.len()];
        // The checklist starts marked from the saved list; any other
        // open keeps no marks. No row starts toggled: the person never
        // touched one, so every row adopts the list until they do; each
        // toggle flips both its mark and its toggle.
        let toggled = if mode == Mode::Scope {
            vec![false; self.catalogue.models.len()]
        } else {
            Vec::new()
        };
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
        // The rows and selection read the staged open: "show all" off
        // with an empty query, the selection on the on-screen model, else
        // the first row.
        let mut open = Open {
            mode,
            selected: 0,
            chips,
            touched,
            marks,
            toggled,
            show_all: false,
            target,
            // Every open starts with an empty query.
            query: String::new(),
        };
        let rows = self.shown(&open);
        open.selected = self.selected_for(on_screen, &rows);
        self.open = Some(open);
        self.installs.reset();
        self.ask_stale();
    }

    /// Closes the picker, sending nothing and saving nothing.
    pub(crate) fn close(&mut self) {
        self.open = None;
    }

    /// Whether the picker is open.
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Moves the focus over the model rows and then the not-installed
    /// rows by `delta`, clamped: headings are not stops, and no key
    /// cycles past either end. With nothing focused, only a step down
    /// lands, on the first stop.
    pub(crate) fn move_row(&mut self, delta: isize) {
        let stops = self.stops();
        if stops.is_empty() {
            return;
        }
        let next = match self.focus() {
            Some(now) => {
                let at = stops.iter().position(|stop| *stop == now).unwrap_or(0);
                at.saturating_add_signed(delta)
                    .clamp(0, stops.len().saturating_sub(1))
            }
            None if delta > 0 => 0,
            None => return,
        };
        if let Some(stop) = stops.get(next).cloned() {
            self.go(stop);
        }
    }

    /// Moves the selected row's chip by `delta`, clamped, and marks the
    /// row touched. A model with no levels has no chip to move, and with
    /// no row shown the selection sits hidden off the shown rows, so the
    /// chip keys change nothing.
    pub(crate) fn move_chip(&mut self, delta: isize) {
        // A not-installed row has no chip, and the hidden selection must
        // not move under it.
        if matches!(self.focus(), Some(Stop::Install(_))) {
            return;
        }
        let (selected, chip) = {
            let Some(open) = self.open.as_ref() else {
                return;
            };
            // Checklist rows have no level chips to move.
            if open.mode == Mode::Scope {
                return;
            }
            let rows = self.shown(open);
            if !rows.contains(&open.selected) {
                return;
            }
            let selected = open.selected;
            let Some(entry) = self.catalogue.models.get(selected) else {
                return;
            };
            if entry.levels.is_empty() {
                return;
            }
            let chip = match open.chips.get(selected).copied().flatten() {
                Some(chip) => chip
                    .saturating_add_signed(delta)
                    .clamp(0, entry.levels.len().saturating_sub(1)),
                None if delta >= 0 => 0,
                None => entry.levels.len().saturating_sub(1),
            };
            (selected, chip)
        };
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if let Some(slot) = open.chips.get_mut(selected) {
            *slot = Some(chip);
        }
        if let Some(touched) = open.touched.get_mut(selected) {
            *touched = true;
        }
    }

    /// Moves the focus by a page through the list's own page step: the
    /// shown height less one, clamped at both ends, over the models and
    /// then the not-installed rows. Headings are not stops, and any
    /// other key leaves the focus where it was.
    pub(crate) fn move_page(&mut self, key: &Key, height: usize) {
        let stops = self.stops();
        if stops.is_empty() {
            return;
        }
        let at = self
            .focus()
            .and_then(|now| stops.iter().position(|stop| *stop == now))
            .unwrap_or(0);
        let mut list = List::default();
        list.select(at, stops.len(), height);
        if list.key(key, stops.len(), height)
            && let Some(stop) = stops.get(list.selected()).cloned()
        {
            self.go(stop);
        }
    }

    /// Whether the open picker is the `/scoped-models` checklist.
    pub(crate) fn is_scope(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.mode == Mode::Scope)
    }

    /// Flips the selected row's mark, only when the selection is one of
    /// the shown rows: with a query matching nothing, Enter toggles
    /// nothing. Only the checklist keeps marks, so everywhere else this
    /// changes nothing.
    pub(crate) fn toggle_mark(&mut self) {
        let selected = {
            let Some(open) = self.open.as_ref() else {
                return;
            };
            let rows = self.shown(open);
            if !rows.contains(&open.selected) {
                return;
            }
            open.selected
        };
        let Some(open) = self.open.as_mut() else {
            return;
        };
        // Every toggle flips the mark and touches the row: a touched
        // row keeps the person's mark against later lists, even
        // flipped twice back to marked.
        if let Some(mark) = open.marks.get_mut(selected) {
            *mark = !*mark;
        }
        if let Some(touched) = open.toggled.get_mut(selected) {
            *touched = true;
        }
    }

    /// Flips "show all", only when a scope is set. The selection stays on
    /// its model when shown, else moves to the first row.
    pub(crate) fn toggle_show_all(&mut self) {
        {
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
        }
        let rows = {
            let Some(open) = self.open.as_ref() else {
                return;
            };
            self.shown(open)
        };
        let Some(open) = self.open.as_mut() else {
            return;
        };
        restick(open, &rows);
    }

    /// Appends `c` to the filter query: typing narrows the choosing list
    /// on every letter. The selection stays on its model when it still
    /// shows, else moves to the first shown row. Nothing while closed.
    pub(crate) fn push_query(&mut self, c: char) {
        {
            let Some(open) = self.open.as_mut() else {
                return;
            };
            open.query.push(c);
        }
        self.installs.focus = None;
        let rows = {
            let Some(open) = self.open.as_ref() else {
                return;
            };
            self.shown(open)
        };
        let Some(open) = self.open.as_mut() else {
            return;
        };
        restick(open, &rows);
    }

    /// Drops the query's last letter: Backspace shortens the filter.
    /// The selection stays on its model when it still shows, else moves
    /// to the first shown row. Nothing while closed or already empty.
    pub(crate) fn pop_query(&mut self) {
        {
            let Some(open) = self.open.as_mut() else {
                return;
            };
            if open.query.pop().is_none() {
                return;
            }
        }
        self.installs.focus = None;
        let rows = {
            let Some(open) = self.open.as_ref() else {
                return;
            };
            self.shown(open)
        };
        let Some(open) = self.open.as_mut() else {
            return;
        };
        restick(open, &rows);
    }

    /// Clears the filter query: the first Esc while one is typed. The
    /// selection stays on its model when it still shows, else moves to
    /// the first shown row. Nothing while closed or already empty.
    pub(crate) fn clear_query(&mut self) {
        {
            let Some(open) = self.open.as_mut() else {
                return;
            };
            if open.query.is_empty() {
                return;
            }
            open.query.clear();
        }
        self.installs.focus = None;
        let rows = {
            let Some(open) = self.open.as_ref() else {
                return;
            };
            self.shown(open)
        };
        let Some(open) = self.open.as_mut() else {
            return;
        };
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

    /// The rows the open picker shows, by catalogue index: the open's
    /// scope toggle, mode, target and query over the catalogue and its
    /// scope.
    pub(crate) fn shown(&self, open: &Open) -> Vec<usize> {
        shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        )
    }

    /// Whether the open picker may count `provider`'s left-out models:
    /// the choosing picker only, and under a set scope without "show
    /// all" only a provider with an installed in-scope model. The query
    /// plays no part: it filters the count, never the section.
    fn group_visible(&self, open: &Open, provider: &str) -> bool {
        if open.mode != Mode::Choose {
            return false;
        }
        if self.scoped.is_empty() || open.show_all {
            return true;
        }
        visible(&self.catalogue.models, &self.scoped, false)
            .into_iter()
            .filter_map(|index| self.catalogue.models.get(index))
            .any(|entry| entry.provider.as_str() == provider)
    }

    /// Whether any raw left-out group may count under the open's mode
    /// and scope, whatever the query hides: what admits the
    /// empty-filter line when no listed model does.
    fn has_visible_group(&self, open: &Open) -> bool {
        self.catalogue
            .left_out
            .iter()
            .any(|group| self.group_visible(open, &group.provider))
    }

    /// The left-out groups the open picker counts, with their row
    /// texts: the choosing picker only, one entry per group the
    /// query keeps. The text names the supplied file verbatim.
    pub(crate) fn shown_left_out(&self, open: &Open) -> Vec<ShownGroup> {
        self.catalogue
            .left_out
            .iter()
            .filter(|group| self.group_visible(open, &group.provider))
            .filter_map(|group| {
                let count = group
                    .models
                    .iter()
                    .filter(|model| {
                        filter::matches_left_out(
                            &group.provider,
                            &model.id,
                            model.name.as_deref(),
                            &open.query,
                        )
                    })
                    .count();
                if count == 0 {
                    return None;
                }
                Some(ShownGroup {
                    provider: group.provider.clone(),
                    setting: group.setting.clone(),
                    text: left_out_text(
                        &controls_to_space(&group.provider),
                        &controls_to_space(&group.file),
                        &controls_to_space(&group.setting),
                        group.login,
                        count,
                    ),
                })
            })
            .collect()
    }

    /// The selection for an open: the on-screen model's row when it
    /// shows, else the first shown row, else `0` with nothing shown.
    fn selected_for(&self, on_screen: Option<(&str, Option<&str>)>, rows: &[usize]) -> usize {
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
    }

    /// Asks `Every`: the refresh button refreshes every list, keeping
    /// whatever the queue holds wider.
    pub(crate) fn refresh(&mut self) {
        if self.open.is_some() {
            self.want = Some(Refresh::Every);
        }
    }

    /// Focuses the model or not-installed row `frame_row` names. Headings
    /// and the buttons are not stops, so they select nothing.
    pub(crate) fn select_frame_row(&mut self, frame_row: usize) {
        let Some(layout) = self.layout() else {
            return;
        };
        match layout.get(frame_row) {
            Some(RowAt::Model(index)) => self.go(Stop::Model(*index)),
            Some(RowAt::Install(name)) => self.go(Stop::Install(name.clone())),
            Some(
                RowAt::Filter
                | RowAt::Buttons
                | RowAt::Heading(_)
                | RowAt::LeftOut { .. }
                | RowAt::NoMatch,
            )
            | None => {}
        }
    }

    /// The open picker's choice at the selection: the row's reference at
    /// its chip, `level_chosen` exactly when the row was touched. A
    /// `/thinking` choice of the current model's own row saves only the
    /// level; any other row is an ordinary choice. `None` while closed,
    /// with no scoped row to choose, or while the checklist is open: it
    /// toggles its marks with Enter and saves them with Ctrl+S, and
    /// never chooses.
    pub(crate) fn choice(&self, session_only: bool) -> Option<Choice> {
        if self.is_scope() || matches!(self.focus(), Some(Stop::Install(_))) {
            return None;
        }
        let open = self.open.as_ref()?;
        let rows = self.shown(open);
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
            Some(
                RowAt::Filter
                | RowAt::Heading(_)
                | RowAt::NoMatch
                | RowAt::LeftOut { .. }
                | RowAt::Install(_),
            )
            | None => None,
            Some(RowAt::Model(index)) => {
                let index = *index;
                self.installs.focus = None;
                // The checklist has no chips to choose: the mark cell
                // toggles its row, and any other cell selects it.
                if self.is_scope() {
                    let open = self.open.as_mut()?;
                    open.selected = index;
                    // Every toggle flips the mark and touches the row:
                    // a touched row keeps the person's mark against
                    // later lists, even flipped twice back to marked.
                    if cell == 0 {
                        if let Some(mark) = open.marks.get_mut(index) {
                            *mark = !*mark;
                        }
                        if let Some(touched) = open.toggled.get_mut(index) {
                            *touched = true;
                        }
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
        let focused = match self.focus() {
            Some(Stop::Model(index)) => layout.iter().position(|row| *row == RowAt::Model(index)),
            Some(Stop::Install(name)) => layout
                .iter()
                .position(|row| *row == RowAt::Install(name.clone())),
            None => None,
        };
        // The shown models are the layout's model rows, in order.
        let mut shown = 0;
        let mut sections = Vec::new();
        let mut installs = Vec::new();
        let left_out = self.shown_left_out(open);
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
                    left_out: Vec::new(),
                });
            } else if let RowAt::Install(provider) = row {
                installs.push(InstallRow {
                    at,
                    provider: controls_to_space(provider),
                    hits: filter::id_hits(provider, &open.query),
                    progress: self.installs.progress(provider).cloned(),
                });
            } else if let RowAt::LeftOut { provider, setting } = row
                && let Some(section) = sections.last_mut()
                && let Some(group) = left_out
                    .iter()
                    .find(|group| &group.provider == provider && &group.setting == setting)
            {
                section.left_out.push(LeftOutRow {
                    at,
                    text: group.text.clone(),
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
                // A checklist row carries no level chip and never the
                // session-only third row: its second row is dim text.
                let checklist = open.mode == Mode::Scope;
                section.models.push(ModelRow {
                    at,
                    id: entry.id.clone(),
                    hits: filter::id_hits(&entry.id, &open.query),
                    roles: entry.roles.clone(),
                    current,
                    scoped: open.show_all && self.scoped.contains(&entry.reference),
                    price: price_text(entry.price.as_ref(), ctx.usage),
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
                    chip: if checklist {
                        None
                    } else {
                        open.chips.get(*index).copied().flatten()
                    },
                    mark: checklist.then(|| open.marks.get(*index).copied()).flatten(),
                    session_only: !checklist && only,
                });
            }
        }
        // The checklist counts its marked installed rows, hidden rows
        // included: one leading and one trailing space either way.
        let marked = open.marks.iter().filter(|mark| **mark).count();
        let count = if open.mode == Mode::Scope {
            if open.query.is_empty() {
                format!(" {marked} of {total} marked ")
            } else {
                format!(" {shown} of {total} models · {marked} marked ")
            }
        } else if !open.query.is_empty() {
            format!("{shown} of {total} models")
        } else if !self.scoped.is_empty() && !open.show_all {
            format!("scoped · {shown} of {total}")
        } else {
            format!("{shown} models")
        };
        Some(PickerView {
            filter: open.query.clone(),
            count,
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
            no_match: !open.query.is_empty()
                && shown == 0
                && installs.is_empty()
                && left_out.is_empty(),
            status: self.status(),
            spinner: ctx.spinner.to_owned(),
            // The checklist marks rows and saves the list: no model
            // or level is chosen here.
            footer: if open.mode == Mode::Scope {
                vec![
                    ("↑↓", "move"),
                    ("Enter", "toggle"),
                    ("Ctrl+S", "save"),
                    ("Esc", "close"),
                ]
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
            installs,
            outcomes: self.installs.outcomes().to_vec(),
            // The checklist draws no refresh button and no refresh
            // click target.
            refresh: open.mode != Mode::Scope,
        })
    }

    /// The layout rows in draw order: the filter first in every mode,
    /// then the buttons, headings with their models and count rows, or
    /// the empty-filter line. A provider with only left-out models
    /// still reads its heading, with no model rows. `None` while
    /// closed.
    fn layout(&self) -> Option<Vec<RowAt>> {
        let open = self.open.as_ref()?;
        let rows = self.shown(open);
        // The filter row is first in every mode, so the buttons row
        // is 1 everywhere.
        let mut layout = vec![RowAt::Filter, RowAt::Buttons];
        let installs = self.shown_installs();
        let left_out = self.shown_left_out(open);
        // A typed query hiding every installed model, every
        // not-installed row and every matching count row shows one dim
        // line and no provider sections. With no listed model only a
        // group the open may count admits the line: a zero-count group
        // emits no heading.
        if !open.query.is_empty()
            && rows.is_empty()
            && installs.is_empty()
            && left_out.is_empty()
            && (!self.catalogue.models.is_empty() || self.has_visible_group(open))
        {
            layout.push(RowAt::NoMatch);
            return Some(layout);
        }
        // Shown models and count rows merge by provider, in provider
        // name order on both sides: a provider's count rows flush at
        // its first shown model, so each section reads heading, count
        // rows, then models.
        let mut groups = left_out.iter().peekable();
        let mut provider: Option<&str> = None;
        for index in rows {
            let Some(entry) = self.catalogue.models.get(index) else {
                continue;
            };
            while groups
                .peek()
                .is_some_and(|group| group.provider.as_str() <= entry.provider.as_str())
            {
                let Some(group) = groups.next() else {
                    break;
                };
                if provider != Some(group.provider.as_str()) {
                    provider = Some(group.provider.as_str());
                    layout.push(RowAt::Heading(group.provider.clone()));
                }
                layout.push(RowAt::LeftOut {
                    provider: group.provider.clone(),
                    setting: group.setting.clone(),
                });
            }
            if provider != Some(entry.provider.as_str()) {
                provider = Some(entry.provider.as_str());
                layout.push(RowAt::Heading(entry.provider.clone()));
            }
            layout.push(RowAt::Model(index));
        }
        for group in groups {
            if provider != Some(group.provider.as_str()) {
                provider = Some(group.provider.as_str());
                layout.push(RowAt::Heading(group.provider.clone()));
            }
            layout.push(RowAt::LeftOut {
                provider: group.provider.clone(),
                setting: group.setting.clone(),
            });
        }
        layout.extend(installs.into_iter().map(RowAt::Install));
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
            // A group the choosing picker counts reads instead of the
            // install prompt: its count row says what is missing. Any
            // other mode keeps the normal prompt.
            if self
                .open
                .as_ref()
                .is_some_and(|open| open.mode == Mode::Choose && self.has_visible_group(open))
            {
                return Vec::new();
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

/// The in-force per-million input and output prices, or base prices before
/// the first call. Free prices carry no tier mark; missing costs stay absent.
pub(crate) fn price_text(price: Option<&Price>, tokens: Option<u64>) -> Option<String> {
    let price = price?;
    let tier = tokens.and_then(|tokens| price.tier_at(tokens));
    let (input, output) = tier
        .map(|tier| (tier.input, tier.output))
        .unwrap_or((price.input, price.output));
    if input == 0 && output == 0 {
        return Some("free".to_owned());
    }
    let mut text = format!(
        "${:.2} in · ${:.2} out /M",
        input as f64 / 1_000_000.0,
        output as f64 / 1_000_000.0,
    );
    if let Some(tier) = tier {
        let above = tier.input_tokens_above;
        let threshold = if above >= 1000 {
            format!("{}k", above / 1000)
        } else {
            above.to_string()
        };
        text.push_str(&format!(" · tier {threshold}+"));
    }
    Some(text)
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
    let tier = price.tier_at(tokens);
    let size = if tier.is_some() {
        size.trim_end_matches(" tokens")
    } else {
        &size
    };
    let micros = tier
        .map(|tier| tier.micros_per_mtok)
        .unwrap_or(price.micros_per_mtok);
    let dollars = tokens as f64 * micros as f64 / 1_000_000_000_000.0;
    format!("{size} · {}", crate::format::money(dollars))
}

/// One left-out group's count row: how many models need the setting,
/// and the one fix. A provider with a login reads the login form; a
/// provider with a credential label reads the setting form, naming the
/// supplied file verbatim. Never the environment-variable clause.
fn left_out_text(provider: &str, file: &str, setting: &str, login: bool, count: usize) -> String {
    let subject = if count == 1 {
        format!("1 model needs {setting}")
    } else {
        format!("{count} models need {setting}")
    };
    if login {
        format!("{subject}: run `fiber login {provider}`")
    } else {
        format!("{subject}: set it in `{file}`")
    }
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
    // scope, with no toggle: the query filters it as in the picker.
    if mode == Mode::Scope {
        return (0..models.len())
            .filter(|index| {
                models
                    .get(*index)
                    .is_some_and(|entry| filter::matches(entry, query))
            })
            .collect();
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
