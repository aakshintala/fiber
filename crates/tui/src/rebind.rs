//! The `/keys` rebinding screen (`docs/tui.md`, "Bindings"): every action
//! with its keys, id and description; Enter then a key rebinds, a key
//! another action holds offers swap or cancel, `r` resets, and Delete
//! unbinds. The screen's own keys are fixed, never resolved through
//! bindings, so a person's bindings never move its controls.

use crate::bindings::BINDINGS;
use crate::keys::Key;
use crate::keyset::Keyset;
use crate::keyset::edit::Refused;
use crate::stroke::{Code, Mods, Stroke};
use crate::swapped::{Frame, Ink, List, Spot, rows_height};

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

/// The rebinding screen: the swapped list's selection, what it waits for,
/// and the message line below the rows.
#[derive(Debug, Default)]
pub(crate) struct KeysScreen {
    list: List,
    mode: Mode,
    message: Option<String>,
}

impl KeysScreen {
    /// Handles one stroke over `keys`: the fixed controls in every mode.
    /// `height` is the view's rows, for paging.
    pub(crate) fn press(&mut self, stroke: &Stroke, keys: &Keyset, height: usize) -> Outcome {
        if *stroke == ctrl_c() {
            return Outcome::Pass;
        }
        self.message = None;
        let shown = rows_height(&self.frame(keys, height), height);
        match std::mem::replace(&mut self.mode, Mode::Browse) {
            Mode::Browse => self.browse(stroke, keys, shown),
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
    fn browse(&mut self, stroke: &Stroke, keys: &Keyset, shown: usize) -> Outcome {
        self.mode = Mode::Browse;
        if let Some(key) = nav(stroke) {
            self.list.key(&key, BINDINGS.len(), shown);
            return Outcome::Nothing;
        }
        let selected = self.list.selected().min(BINDINGS.len().saturating_sub(1));
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

    /// Handles a click: the ✕ closes in every mode, a row selects while
    /// browsing. `height` is the view's rows.
    pub(crate) fn click(&mut self, spot: Spot, height: usize) -> Outcome {
        match spot {
            Spot::Close => Outcome::Close,
            Spot::Row(at) => {
                if matches!(self.mode, Mode::Browse) {
                    self.list.select(at, BINDINGS.len(), height);
                }
                Outcome::Nothing
            }
            Spot::Cell(_, _) | Spot::Switch { .. } | Spot::Revoke(_) => Outcome::Nothing,
        }
    }

    /// A save's failure: its message below the rows, back to browsing.
    pub(crate) fn failed(&mut self, message: String) {
        self.mode = Mode::Browse;
        self.message = Some(format!("Not saved: {message}"));
    }

    /// The frame at `height`: the title, one row per action, the message
    /// or the clash line below them, and the footer naming the mode's
    /// keys.
    pub(crate) fn frame(&self, keys: &Keyset, _height: usize) -> Frame {
        let labels: Vec<String> = BINDINGS
            .iter()
            .map(|binding| keys.labels(binding.id))
            .collect();
        let wide = labels
            .iter()
            .map(|label| crate::format::width(label))
            .max()
            .unwrap_or(0);
        let rows = BINDINGS
            .iter()
            .zip(labels)
            .map(|(binding, label)| {
                let fill = wide.saturating_sub(crate::format::width(&label));
                vec![
                    (format!("{label}{}", " ".repeat(fill)), None, Ink::Plain),
                    (format!(" {} ", binding.id), None, Ink::Muted),
                    (binding.description.to_owned(), None, Ink::Plain),
                ]
            })
            .collect();
        let mut below = Vec::new();
        if let Some(message) = &self.message {
            below.push(message.clone());
        } else if let Mode::Clash { other, stroke, .. } = &self.mode {
            below.push(clash_line(other, stroke));
        }
        Frame {
            title: "Keys".to_owned(),
            rows,
            list: self.list,
            below,
            field: None,
            footer: self.footer(),
        }
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
