//! The `/keys` rebinding screen (`docs/tui.md`, "Bindings"): every action
//! with its keys, id and description; Enter then a key rebinds, a key
//! another action holds offers swap or cancel, `r` resets, and Delete
//! unbinds. The screen's own keys are fixed, never resolved through
//! bindings, so a person's bindings never move its controls.

use crate::bindings::BINDINGS;
use crate::keys::Key;
use crate::keyset::Keyset;
use crate::keyset::edit::Refused;
use crate::rows::Join;
use crate::stroke::{Code, Mods, Stroke};
use crate::swapped::{Frame, GUTTER, Ink, List, Spot, rows_height};

/// The browse footer: the screen's fixed controls.
const BROWSE_FOOTER: &str = "↑↓ move · Enter rebind · r reset · Delete unbind · Esc close";

/// The clash prompt's footer.
const SWAP_FOOTER: &str = "Enter swap · Esc cancel";

/// What Enter, `r` and Delete say on `clear_then_quit`: Ctrl+C always
/// clears, then quits, and cannot be rebound (`docs/tui.md`, "Bindings").
const FIXED_MESSAGE: &str = "Ctrl+C always clears, then quits.";

/// The plain Esc stroke.
fn esc() -> Stroke {
    Stroke {
        code: Code::Esc,
        mods: Mods::NONE,
    }
}

/// The Ctrl+C stroke: passed through to the bindings in every mode, never
/// captured, because the second Ctrl+C always quits (`docs/tui.md`,
/// "Input and focus").
fn ctrl_c() -> Stroke {
    Stroke {
        code: Code::Char('c'),
        mods: Mods::CTRL,
    }
}

/// The plain Enter stroke.
fn enter() -> Stroke {
    Stroke {
        code: Code::Enter,
        mods: Mods::NONE,
    }
}

/// The `r` stroke resetting an action to its defaults.
fn reset() -> Stroke {
    Stroke {
        code: Code::Char('r'),
        mods: Mods::NONE,
    }
}

/// A stroke moving the selection; `None` for any other stroke. Only the
/// bare keys move: with a modifier held the stroke is swallowed, as every
/// other key while browsing is.
fn nav(stroke: &Stroke) -> Option<Key> {
    if stroke.mods != Mods::NONE {
        return None;
    }
    match stroke.code {
        Code::Up => Some(Key::Up),
        Code::Down => Some(Key::Down),
        Code::PageUp => Some(Key::PageUp),
        Code::PageDown => Some(Key::PageDown),
        Code::Char(_)
        | Code::Enter
        | Code::Esc
        | Code::Tab
        | Code::Backspace
        | Code::Delete
        | Code::Insert
        | Code::Home
        | Code::End
        | Code::Left
        | Code::Right
        | Code::Space
        | Code::F(_) => None,
    }
}

/// Whether `stroke` unbinds: Delete or Backspace, bare, so the key
/// labelled "delete" on a Mac keyboard unbinds too (`docs/tui.md`,
/// "Bindings").
fn unbinds(stroke: &Stroke) -> bool {
    *stroke
        == (Stroke {
            code: Code::Delete,
            mods: Mods::NONE,
        })
        || *stroke
            == (Stroke {
                code: Code::Backspace,
                mods: Mods::NONE,
            })
}

/// What the screen answers a stroke with.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The stroke changed nothing on screen.
    Nothing,
    /// The screen closes.
    Close,
    /// The stroke goes through the bindings: Ctrl+C in every mode.
    Pass,
    /// The capture, swap, reset or unbind landed: the new keyset to save.
    Apply(Keyset),
}

/// What the screen waits for.
#[derive(Debug, Default)]
enum Mode {
    /// Moving over the rows.
    #[default]
    Browse,
    /// Taking one key per variant for the action at `at`.
    Capture {
        /// The action's index in the table.
        at: usize,
        /// The keys taken so far, in order.
        got: Vec<Stroke>,
    },
    /// A captured key another action holds: swap or cancel.
    Clash {
        /// The action's index in the table.
        at: usize,
        /// The captured keys.
        got: Vec<Stroke>,
        /// The actions already swapped with, in prompt order.
        agreed: Vec<&'static str>,
        /// The action the prompt names.
        other: &'static str,
        /// The captured key it holds.
        stroke: Stroke,
    },
}

/// The rebinding screen: the selected action and the first shown
/// display line, what it waits for, and the message line below the
/// rows. Selection moves by action, never by display line: each
/// action wraps inside its own columns and takes as many lines as
/// its tallest column (`docs/tui.md`, "Bindings"). The mouse wheel
/// is the exception: it moves the scroll by display line, three
/// lines a step, and may rest it inside a wrapped action.
#[derive(Debug, Default)]
pub(crate) struct KeysScreen {
    /// The selected action's index in the table.
    selected: usize,
    /// The first shown display line in the table.
    top: usize,
    mode: Mode,
    message: Option<String>,
}

impl KeysScreen {
    /// Handles one stroke over `keys`: the fixed controls in every mode.
    /// `width` is the draw width in columns, `height` the view's rows,
    /// for paging.
    pub(crate) fn press(
        &mut self,
        stroke: &Stroke,
        keys: &Keyset,
        width: usize,
        height: usize,
    ) -> Outcome {
        if *stroke == ctrl_c() {
            return Outcome::Pass;
        }
        self.message = None;
        let shown = rows_height(&self.frame(keys, width, height), height);
        match std::mem::replace(&mut self.mode, Mode::Browse) {
            Mode::Browse => self.browse(stroke, keys, width, shown),
            Mode::Capture { at, mut got } => {
                if *stroke == esc() {
                    self.mode = Mode::Browse;
                    return Outcome::Nothing;
                }
                let Some(binding) = BINDINGS.get(at) else {
                    self.mode = Mode::Browse;
                    return Outcome::Nothing;
                };
                if got.contains(stroke) {
                    let name = got
                        .iter()
                        .position(|key| key == stroke)
                        .and_then(|slot| Keyset::variant(binding.id, slot))
                        .unwrap_or_else(|| "?".to_owned());
                    self.message =
                        Some(format!("{} is already the key for {name}", stroke.label()));
                    self.mode = Mode::Capture { at, got };
                    return Outcome::Nothing;
                }
                got.push(*stroke);
                if got.len() == binding.events.len().max(1) {
                    self.finish(at, binding.id, got, Vec::new(), keys)
                } else {
                    self.mode = Mode::Capture { at, got };
                    Outcome::Nothing
                }
            }
            Mode::Clash {
                at,
                got,
                mut agreed,
                other,
                stroke: held,
            } => {
                if *stroke == esc() {
                    self.mode = Mode::Browse;
                    return Outcome::Nothing;
                }
                if *stroke != enter() {
                    self.mode = Mode::Clash {
                        at,
                        got,
                        agreed,
                        other,
                        stroke: held,
                    };
                    return Outcome::Nothing;
                }
                let Some(binding) = BINDINGS.get(at) else {
                    self.mode = Mode::Browse;
                    return Outcome::Nothing;
                };
                agreed.push(other);
                self.finish(at, binding.id, got, agreed, keys)
            }
        }
    }

    /// Handles one stroke while browsing.
    fn browse(&mut self, stroke: &Stroke, keys: &Keyset, width: usize, shown: usize) -> Outcome {
        self.mode = Mode::Browse;
        if let Some(key) = nav(stroke) {
            self.move_by(&key, keys, width, shown);
            return Outcome::Nothing;
        }
        let selected = self.selected.min(BINDINGS.len().saturating_sub(1));
        let Some(binding) = BINDINGS.get(selected) else {
            return Outcome::Nothing;
        };
        if *stroke == enter() {
            if binding.id == "clear_then_quit" {
                self.message = Some(FIXED_MESSAGE.to_owned());
                return Outcome::Nothing;
            }
            self.mode = Mode::Capture {
                at: selected,
                got: Vec::new(),
            };
            return Outcome::Nothing;
        }
        if *stroke == reset() {
            if binding.id == "clear_then_quit" {
                self.message = Some(FIXED_MESSAGE.to_owned());
                return Outcome::Nothing;
            }
            let defaults = keys.defaults_of(binding.id).to_vec();
            return self.finish(selected, binding.id, defaults, Vec::new(), keys);
        }
        if unbinds(stroke) {
            if binding.id == "clear_then_quit" {
                self.message = Some(FIXED_MESSAGE.to_owned());
                return Outcome::Nothing;
            }
            if keys.current(binding.id).is_empty() {
                return Outcome::Nothing;
            }
            return self.finish(selected, binding.id, Vec::new(), Vec::new(), keys);
        }
        if *stroke == esc() {
            return Outcome::Close;
        }
        Outcome::Nothing
    }

    /// Moves the selection by `key`: up and down by one action, PageUp
    /// and PageDown by the whole actions fitting from the top less one,
    /// at least one, clamped at both ends of the table.
    fn move_by(&mut self, key: &Key, keys: &Keyset, width: usize, shown: usize) {
        let total = BINDINGS.len();
        if total == 0 {
            return;
        }
        let grown = heights(keys, width);
        let total_lines: usize = grown.iter().sum();
        // Pages count from the action holding the stored display line:
        // a wheel may rest the top inside a wrapped action, and only
        // an action index enters the fitting count.
        let top_action = action_at(self.top.min(total_lines.saturating_sub(1)), &grown);
        match key {
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = self.selected.saturating_add(1).min(total - 1),
            Key::PageUp => {
                let page = fits_from(top_action, &grown, shown)
                    .saturating_sub(1)
                    .max(1);
                self.selected = self.selected.saturating_sub(page);
            }
            Key::PageDown => {
                let page = fits_from(top_action, &grown, shown)
                    .saturating_sub(1)
                    .max(1);
                self.selected = self.selected.saturating_add(page).min(total - 1);
            }
            Key::Char(_)
            | Key::Backspace
            | Key::Enter
            | Key::Esc
            | Key::CtrlC
            | Key::CtrlO
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::CtrlF
            | Key::End
            | Key::AltA
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::Tab
            | Key::BackTab
            | Key::F1 => return,
        }
        self.settle(&grown, shown);
    }

    /// Keeps the selected action whole on screen: the scroll
    /// reanchors through the action holding the stored display line,
    /// then moves down until the selection sits in the window fitting
    /// whole from it, and the stored top becomes that action's first
    /// display line. An action taller than the window shows its top
    /// lines, with the selection held.
    fn settle(&mut self, grown: &[usize], shown: usize) {
        let total = BINDINGS.len();
        if total == 0 {
            return;
        }
        self.selected = self.selected.min(total - 1);
        let total_lines: usize = grown.iter().sum();
        let mut top = action_at(self.top.min(total_lines.saturating_sub(1)), grown);
        top = top.min(self.selected);
        loop {
            let fits = fits_from(top, grown, shown).max(1);
            if self.selected < top.saturating_add(fits) {
                break;
            }
            top = top.saturating_add(1);
        }
        self.top = grown.iter().take(top).sum();
    }

    /// Applies captured keys through [`Keyset::set`]: a clash prompts for
    /// the action holding them, each prompt naming one action until none
    /// is left.
    fn finish(
        &mut self,
        at: usize,
        id: &'static str,
        got: Vec<Stroke>,
        agreed: Vec<&'static str>,
        keys: &Keyset,
    ) -> Outcome {
        match keys.set(id, got.clone(), &agreed) {
            Ok(next) => {
                self.mode = Mode::Browse;
                Outcome::Apply(next)
            }
            Err(Refused::Clash { other, stroke }) => {
                self.mode = Mode::Clash {
                    at,
                    got,
                    agreed,
                    other,
                    stroke,
                };
                Outcome::Nothing
            }
            // The screen refuses repeats inline while capturing, and
            // Ctrl+C passes through, so only a clash can refuse a
            // finished capture.
            Err(_) => {
                self.mode = Mode::Browse;
                Outcome::Nothing
            }
        }
    }

    /// Shifts the window three display lines for a wheel step while
    /// browsing, clearing the transient message as a stroke does; in
    /// Capture or Clash the wheel is swallowed, never a captured key.
    /// The scroll is a display-line offset over the legal window
    /// range, so a step into a wrapped action rests inside it instead
    /// of snapping to its start, moving one to three lines off an
    /// end and holding at one. The selection stays while any of its
    /// lines shows, else moves to the action at the nearer window
    /// edge. A zero-height view changes nothing, not even the
    /// message. `width` is the draw width in columns, `height` the
    /// view's rows.
    pub(crate) fn wheel(
        &mut self,
        up: bool,
        keys: &Keyset,
        width: usize,
        height: usize,
    ) -> Outcome {
        if !matches!(self.mode, Mode::Browse) {
            return Outcome::Nothing;
        }
        let shown = rows_height(&self.frame(keys, width, height), height);
        if shown == 0 {
            return Outcome::Nothing;
        }
        self.message = None;
        let grown = heights(keys, width);
        let total = BINDINGS.len();
        if total == 0 {
            return Outcome::Nothing;
        }
        let total_lines: usize = grown.iter().sum();
        let end = total_lines.saturating_sub(shown);
        let next = if up {
            self.top.min(end).saturating_sub(3)
        } else {
            self.top.min(end).saturating_add(3).min(end)
        };
        self.top = next;
        let selected = self.selected.min(total - 1);
        let start: usize = grown.iter().take(selected).sum();
        let height = grown.get(selected).copied().unwrap_or(0);
        if start.saturating_add(height) <= next {
            self.selected = action_at(next, &grown);
        } else if start >= next.saturating_add(shown) {
            self.selected = action_at(next.saturating_add(shown).saturating_sub(1), &grown);
        } else {
            self.selected = selected;
        }
        Outcome::Nothing
    }

    /// Handles a click: the ✕ closes in every mode, a display line
    /// selects its action while browsing. `width` is the draw width in
    /// columns, `height` the view's rows.
    pub(crate) fn click(
        &mut self,
        spot: Spot,
        keys: &Keyset,
        width: usize,
        height: usize,
    ) -> Outcome {
        match spot {
            Spot::Close => Outcome::Close,
            Spot::Row(line) => {
                if matches!(self.mode, Mode::Browse) {
                    let grown = heights(keys, width);
                    let shown = rows_height(&self.frame(keys, width, height), height);
                    self.selected = action_at(line, &grown).min(BINDINGS.len().saturating_sub(1));
                    self.settle(&grown, shown);
                }
                Outcome::Nothing
            }
            Spot::Cell(_, _) | Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_) => {
                Outcome::Nothing
            }
        }
    }

    /// A save's failure: its message below the rows, back to browsing.
    pub(crate) fn failed(&mut self, message: String) {
        self.mode = Mode::Browse;
        self.message = Some(format!("Not saved: {message}"));
    }

    /// The frame at `height`: every action wrapped inside its own
    /// columns, one display line per wrapped row, with how each line
    /// joins the one before, the message or the clash line below them,
    /// and the footer naming the mode's keys. `width` is the draw width
    /// in columns; the selection stays action-indexed while the scroll
    /// is a display-line offset, and the frame carries both as display
    /// lines.
    pub(crate) fn frame(&self, keys: &Keyset, width: usize, height: usize) -> Frame {
        let (label, id, description) = columns(keys, width);
        let grown = heights(keys, width);
        let mut rows = Vec::new();
        let mut joins = Vec::new();
        for binding in BINDINGS {
            let keys = column_joined(&keys.labels(binding.id), label);
            let ids = column_joined(binding.id, id);
            let descriptions = column_joined(binding.description, description);
            let tall = keys.len().max(ids.len()).max(descriptions.len());
            for line in 0..tall {
                let key = keys
                    .get(line)
                    .map(|(text, _)| text.clone())
                    .unwrap_or_else(|| " ".repeat(label.max(1)));
                let id = ids
                    .get(line)
                    .map(|(text, _)| text.clone())
                    .unwrap_or_else(|| " ".repeat(id.max(1)));
                let description = descriptions
                    .get(line)
                    .map(|(text, _)| text.clone())
                    .unwrap_or_else(|| " ".repeat(description.max(1)));
                rows.push(vec![
                    (key, None, Ink::Plain),
                    (format!(" {id} "), None, Ink::Muted),
                    (description, None, Ink::Plain),
                ]);
                joins.push(join_at(
                    line,
                    [keys.as_slice(), ids.as_slice(), descriptions.as_slice()],
                ));
            }
        }
        let mut below = Vec::new();
        if let Some(message) = &self.message {
            below.push(message.clone());
        } else if let Mode::Clash { other, stroke, .. } = &self.mode {
            below.push(clash_line(other, stroke));
        }
        let mut frame = Frame {
            title: "Keys".to_owned(),
            command: "/keys".to_owned(),
            rows,
            joins,
            list: List::gutter(),
            below,
            field: None,
            footer: self.footer(),
        };
        let shown = rows_height(&frame, height);
        frame.list = place(self.selected, self.top, &grown, shown);
        frame
    }

    /// The footer naming the mode's keys.
    fn footer(&self) -> String {
        match &self.mode {
            Mode::Browse => BROWSE_FOOTER.to_owned(),
            Mode::Capture { at, got } => {
                let capture = BINDINGS.get(*at).map_or_else(
                    || "?".to_owned(),
                    |binding| {
                        let variant = Keyset::variant(binding.id, got.len())
                            .map_or_else(String::new, |name| format!(" ({name})"));
                        format!("{}{variant}", binding.description)
                    },
                );
                format!("Press the key for {capture} · Esc cancel")
            }
            Mode::Clash { .. } => SWAP_FOOTER.to_owned(),
        }
    }
}

/// The keys, id and description widths sharing `width`: the natural
/// widths while they fit in the room past the gutter and the two
/// separators, else the id keeps its widest capped at half the room
/// and the keys and description share the rest in proportion, one
/// column each at least, the spare column to the keys.
pub(crate) fn columns(keys: &Keyset, width: usize) -> (usize, usize, usize) {
    let mut wide = (1usize, 1usize, 1usize);
    for binding in BINDINGS {
        wide.0 = wide.0.max(crate::format::width(&keys.labels(binding.id)));
        wide.1 = wide.1.max(crate::format::width(binding.id));
        wide.2 = wide.2.max(crate::format::width(binding.description));
    }
    let room = width.saturating_sub(GUTTER + 2);
    if wide.0 + wide.1 + wide.2 <= room {
        return wide;
    }
    let id = wide.1.min(room / 2).max(1);
    let rest = room.saturating_sub(id);
    let total = wide.0.saturating_add(wide.2).max(1);
    let share = |wide: usize| wide.saturating_mul(rest) / total;
    let (mut label, description) = (share(wide.0).max(1), share(wide.2).max(1));
    // The floored shares leave at most one column unused, so one step
    // fills it: the keys take it while narrower than their widest text.
    if label + description < rest && label < wide.0 {
        label = label.saturating_add(1);
    }
    (label, id, description)
}

/// How many display lines each action takes at `width`: every column
/// wraps inside its own width and the row takes its tallest column.
pub(crate) fn heights(keys: &Keyset, width: usize) -> Vec<usize> {
    let (label, id, description) = columns(keys, width);
    BINDINGS
        .iter()
        .map(|binding| {
            crate::format::wrap(&keys.labels(binding.id), label)
                .len()
                .max(crate::format::wrap(binding.id, id).len())
                .max(crate::format::wrap(binding.description, description).len())
                .max(1)
        })
        .collect()
}

/// How many whole actions fit from `top` in `shown` display lines.
fn fits_from(top: usize, grown: &[usize], shown: usize) -> usize {
    let mut room: usize = shown;
    let mut count: usize = 0;
    for height in grown.iter().skip(top) {
        if *height > room {
            break;
        }
        room = room.saturating_sub(*height);
        count = count.saturating_add(1);
    }
    count
}

/// The action holding display line `line`: continuations belong to
/// their action, lines past the end to the last one.
fn action_at(line: usize, grown: &[usize]) -> usize {
    let mut start: usize = 0;
    for (at, height) in grown.iter().enumerate() {
        if line < start.saturating_add(*height) {
            return at;
        }
        start = start.saturating_add(*height);
    }
    grown.len().saturating_sub(1)
}

/// The frame's list for the selected action and the display-line
/// top: the stored top clamps to the current window range first, then
/// the window reveals the selected action's first line when the whole
/// action falls outside it, moving the smallest distance that shows
/// that line. The selected action's first line and the top land
/// independently, so a wheel step can rest the window inside a
/// wrapped action while the selection stays on its first line.
fn place(selected: usize, top_line: usize, grown: &[usize], shown: usize) -> List {
    let total: usize = grown.iter().sum();
    let end = total.saturating_sub(shown);
    let mut top = top_line.min(end);
    let at = selected.min(grown.len().saturating_sub(1));
    let selected_line: usize = grown.iter().take(at).sum();
    if shown > 0 {
        let last = selected_line
            .saturating_add(grown.get(at).copied().unwrap_or(0))
            .saturating_sub(1);
        if last < top {
            top = selected_line;
        } else if selected_line >= top.saturating_add(shown) {
            top = selected_line.saturating_add(1).saturating_sub(shown);
        }
    }
    let mut list = List::gutter();
    list.set_line_window(selected_line, top);
    list
}

/// One column's wrapped display lines, each padded to the column's
/// width, with how each line joins the one before.
fn column_joined(text: &str, wide: usize) -> Vec<(String, Join)> {
    let wide = wide.max(1);
    crate::format::wrap_joined(text, wide)
        .into_iter()
        .map(|(line, join)| {
            (
                format!(
                    "{line}{}",
                    " ".repeat(wide.saturating_sub(crate::format::width(&line)))
                ),
                join,
            )
        })
        .collect()
}

/// How display line `line` of one action joins the line before: the
/// first line breaks, a continuation wraps with the space where any
/// column dropped one, else wraps inside its word.
fn join_at(line: usize, columns: [&[(String, Join)]; 3]) -> Join {
    if line == 0 {
        return Join::Break;
    }
    let mut wrap = Join::Wrap;
    for column in columns {
        match column.get(line).map(|(_, join)| join) {
            Some(Join::WrapSpace) => return Join::WrapSpace,
            Some(Join::Wrap) => wrap = Join::Wrap,
            _ => {}
        }
    }
    wrap
}

/// The clash prompt's line: the captured key's label and the action
/// holding it.
fn clash_line(other: &str, stroke: &Stroke) -> String {
    let description = BINDINGS
        .iter()
        .find(|binding| binding.id == other)
        .map_or("?", |binding| binding.description);
    format!("{} is bound to {description}", stroke.label())
}

#[cfg(test)]
#[path = "rebind_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rebind_view_tests.rs"]
mod view_tests;
