//! The key map overlay's state: its tab, search query, focus and scroll
//! (`docs/tui.md`, "Bindings").

use crate::bindings::{BINDINGS, Binding};
use crate::format::{width, wrap};
use crate::keys::Key;
use crate::keyset::Keyset;

/// The key map's tab, query, focus and scroll: which bindings show, which
/// of them is focused, and which is first shown. Tab 0 is All.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct KeyMap {
    tab: usize,
    query: String,
    focus: usize,
    top: usize,
}

/// The key map's tabs after All: each `Binding.area` in table order.
pub(crate) fn areas() -> Vec<&'static str> {
    let mut areas = Vec::new();
    for binding in BINDINGS {
        if areas.last() != Some(&binding.area) {
            areas.push(binding.area);
        }
    }
    areas
}

impl KeyMap {
    /// The bindings the tab and query show, in table order: the query
    /// matches the action, the effective keys drawn and the other paths,
    /// ignoring case.
    pub(crate) fn visible(&self, keys: &Keyset) -> Vec<&'static Binding> {
        let areas = areas();
        let query = self.query.to_lowercase();
        BINDINGS
            .iter()
            .filter(|binding| {
                self.tab == 0 || areas.get(self.tab.saturating_sub(1)) == Some(&binding.area)
            })
            .filter(|binding| {
                query.is_empty()
                    || binding.description.to_lowercase().contains(&query)
                    || keys.shown(binding).to_lowercase().contains(&query)
                    || binding.other_paths.to_lowercase().contains(&query)
            })
            .collect()
    }

    /// The selected tab: 0 is All.
    pub(crate) fn tab(&self) -> usize {
        self.tab
    }

    /// The typed query.
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The focused binding's index among the visible ones.
    pub(crate) fn focus(&self) -> usize {
        self.focus
    }

    /// The first binding shown.
    pub(crate) fn top(&self) -> usize {
        self.top
    }

    /// Moves the tab, stopping at both ends; the focus and scroll return
    /// to the top.
    pub(crate) fn move_tab(&mut self, delta: isize) {
        let tabs = areas().len();
        self.tab = self.tab.saturating_add_signed(delta).min(tabs);
        self.focus = 0;
        self.top = 0;
    }

    /// Types a character: the focus and scroll return to the top.
    pub(crate) fn push(&mut self, c: char) {
        self.query.push(c);
        self.focus = 0;
        self.top = 0;
    }

    /// Deletes the query's last character: the focus and scroll return to
    /// the top.
    pub(crate) fn pop(&mut self) {
        self.query.pop();
        self.focus = 0;
        self.top = 0;
    }

    /// Moves the focus by binding: ↑ and ↓ by one, PageUp and PageDown by
    /// the bindings that fit less one, at least one. The window keeps the
    /// focused binding whole: `fits(top)` is the bindings that fit whole
    /// from `top`. Any other key changes nothing.
    pub(crate) fn key(&mut self, key: &Key, keys: &Keyset, fits: &dyn Fn(usize) -> usize) {
        let total = self.visible(keys).len();
        match key {
            Key::Up => self.focus = self.focus.saturating_sub(1),
            Key::Down => {
                self.focus = self.focus.saturating_add(1).min(total.saturating_sub(1));
            }
            Key::PageUp => {
                let page = fits(self.top).saturating_sub(1).max(1);
                self.focus = self.focus.saturating_sub(page);
            }
            Key::PageDown => {
                let page = fits(self.top).saturating_sub(1).max(1);
                self.focus = self.focus.saturating_add(page).min(total.saturating_sub(1));
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
        self.settle(total, fits);
    }

    /// Keeps the focused binding whole on screen: the scroll is the first
    /// binding shown, moved down until the focus sits in the window that
    /// fits whole from it. The key map's draw re-runs it from the drawn
    /// layout, so a resize after the last key keeps the focus whole too.
    pub(crate) fn settle(&mut self, total: usize, fits: &dyn Fn(usize) -> usize) {
        if total == 0 {
            self.focus = 0;
            self.top = 0;
            return;
        }
        self.focus = self.focus.min(total.saturating_sub(1));
        if self.focus < self.top {
            self.top = self.focus;
            return;
        }
        while self.top < self.focus {
            let shown = fits(self.top).max(1);
            if self.focus < self.top.saturating_add(shown) {
                break;
            }
            self.top = self.top.saturating_add(1);
        }
    }
}

/// What a key does while the key map is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyPress {
    /// Esc: the map closes.
    Close,
    /// Ctrl+C, ⌥R and ⌥1–9: they reach past the map to their handlers.
    Through,
    /// Anything else: the map takes it.
    Handled,
}

impl KeyMap {
    /// Handles one key: typed characters and Backspace edit the query, ↑
    /// ↓ PageUp PageDown move the focus by what fits whole (`fits(top)`
    /// counts the bindings that fit whole from `top` in the overlay as
    /// drawn), Esc closes, and Enter does nothing.
    pub(crate) fn press(
        &mut self,
        key: &Key,
        keys: &Keyset,
        inner: u16,
        height: usize,
    ) -> KeyPress {
        match key {
            Key::Esc => KeyPress::Close,
            Key::CtrlC | Key::AltR | Key::AltDigit(_) => KeyPress::Through,
            Key::Char(ch) => {
                self.push(*ch);
                KeyPress::Handled
            }
            Key::Backspace => {
                self.pop();
                KeyPress::Handled
            }
            Key::Up | Key::Down | Key::PageUp | Key::PageDown => {
                let visible = self.visible(keys);
                let cols = columns(&visible, keys, inner);
                let grown = heights(&visible, keys, &cols);
                let body = chrome(height).body;
                let fits = |top: usize| fits_from(top, &grown, body);
                self.key(key, keys, &fits);
                KeyPress::Handled
            }
            Key::Enter
            | Key::End
            | Key::AltA
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlO
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::CtrlF => KeyPress::Handled,
        }
    }
}
/// condition and joiner, the condition moving after the action.
pub(crate) fn keys_text(binding: &Binding, shown: &str) -> String {
    if binding.when.is_empty() {
        return shown.to_owned();
    }
    let comma = format!(", {}", binding.when);
    let space = format!(" {}", binding.when);
    shown
        .strip_suffix(&comma)
        .or_else(|| shown.strip_suffix(&space))
        .unwrap_or(shown)
        .to_owned()
}

/// A binding's three column texts: the area, the action with any
/// condition in parentheses after it, and the keys with other paths
/// after " · ".
pub(crate) fn row_text(binding: &Binding, shown: &str) -> (String, String, String) {
    let action = if binding.when.is_empty() {
        binding.description.to_owned()
    } else {
        format!("{} ({})", binding.description, binding.when)
    };
    let keys = if binding.other_paths.is_empty() {
        keys_text(binding, shown)
    } else {
        format!("{} · {}", keys_text(binding, shown), binding.other_paths)
    };
    (binding.area.to_owned(), action, keys)
}

/// The columns for `visible` at `inner` columns wide: the area column
/// keeps its widest text while it takes at most half the room, so the
/// areas read whole; the action and keys columns share what is left in
/// proportion, one column each at least.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Columns {
    /// The area column.
    pub(crate) area: u16,
    /// The action column.
    pub(crate) action: u16,
    /// The keys column.
    pub(crate) keys: u16,
}

/// The columns for `visible` at `inner` columns wide.
pub(crate) fn columns(visible: &[&Binding], keys: &Keyset, inner: u16) -> Columns {
    let mut widest = (1u16, 1u16, 1u16);
    for binding in visible {
        let (area, action, paths) = row_text(binding, &keys.shown(binding));
        widest.0 = widest
            .0
            .max(u16::try_from(width(&area)).unwrap_or(u16::MAX));
        widest.1 = widest
            .1
            .max(u16::try_from(width(&action)).unwrap_or(u16::MAX));
        widest.2 = widest
            .2
            .max(u16::try_from(width(&paths)).unwrap_or(u16::MAX));
    }
    let room = inner.saturating_sub(6);
    if widest.0.saturating_add(widest.1).saturating_add(widest.2) <= room {
        return Columns {
            area: widest.0,
            action: widest.1,
            keys: widest.2,
        };
    }
    let area = widest.0.min(room / 2).max(1);
    let rest = room.saturating_sub(area);
    let total = widest.1.saturating_add(widest.2).max(1);
    let share = |wide: u16| {
        u16::try_from(u32::from(wide) * u32::from(rest) / u32::from(total)).unwrap_or(u16::MAX)
    };
    let (mut action, mut keys) = (share(widest.1).max(1), share(widest.2).max(1));
    while action.saturating_add(keys) < rest && (action < widest.1 || keys < widest.2) {
        if action < widest.1 {
            action = action.saturating_add(1);
        } else {
            keys = keys.saturating_add(1);
        }
    }
    while action.saturating_add(keys) > rest && (action > 1 || keys > 1) {
        if keys > 1 {
            keys = keys.saturating_sub(1);
        } else {
            action = action.saturating_sub(1);
        }
    }
    Columns { area, action, keys }
}

/// How many display rows `binding` takes in `cols`: each column wraps
/// inside its own width.
pub(crate) fn binding_rows(binding: &Binding, shown: &str, cols: &Columns) -> usize {
    let (area, action, paths) = row_text(binding, shown);
    wrap(&area, usize::from(cols.area).max(1))
        .len()
        .max(1)
        .max(wrap(&action, usize::from(cols.action).max(1)).len())
        .max(wrap(&paths, usize::from(cols.keys).max(1)).len())
}

/// How many display rows each of `visible` takes in `cols`.
pub(crate) fn heights(visible: &[&Binding], keys: &Keyset, cols: &Columns) -> Vec<usize> {
    visible
        .iter()
        .map(|binding| binding_rows(binding, &keys.shown(binding), cols))
        .collect()
}

/// The key map's chrome in an area `area_h` rows high: which parts stay
/// so the body holds at least one row. The two dim lines give way first,
/// then the blank row before the tabs, then the footer with its blank
/// row; the edges, pad rows, title, tabs and search line stay while the
/// area holds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Chrome {
    /// The two dim lines under the title draw.
    pub(crate) dims: bool,
    /// The blank row before the tabs draws.
    pub(crate) blank: bool,
    /// The footer and its blank row draw.
    pub(crate) footer: bool,
    /// The body rows left for bindings.
    pub(crate) body: usize,
}

/// The chrome and binding rows in an area `area_h` rows high.
pub(crate) fn chrome(area_h: usize) -> Chrome {
    let mut dims = true;
    let mut blank = true;
    let mut footer = true;
    // Chrome gives way until the body holds at least one binding row:
    // the two dim lines, then the blank row before the tabs, then the
    // footer with its blank row.
    loop {
        let frame = if footer { 8 } else { 6 };
        let mut need: usize = 2;
        if dims {
            need = need.saturating_add(2);
        }
        if blank {
            need = need.saturating_add(1);
        }
        if area_h.saturating_sub(frame) >= need.saturating_add(1) {
            break;
        }
        if dims {
            dims = false;
        } else if blank {
            blank = false;
        } else if footer {
            footer = false;
        } else {
            break;
        }
    }
    let frame = if footer { 8 } else { 6 };
    let mut body = area_h.saturating_sub(frame);
    if dims {
        body = body.saturating_sub(2);
    }
    if blank {
        body = body.saturating_sub(1);
    }
    Chrome {
        dims,
        blank,
        footer,
        body: body.saturating_sub(2),
    }
}

/// How many bindings fit whole from `top` in `body` rows: the last row
/// is the "↑ N more · ↓ M more" line while bindings hide above.
pub(crate) fn fits_from(top: usize, heights: &[usize], body: usize) -> usize {
    let mut room = body.saturating_sub(usize::from(top > 0));
    let mut shown: usize = 0;
    for height in heights.iter().skip(top) {
        if *height > room {
            break;
        }
        room = room.saturating_sub(*height);
        shown = shown.saturating_add(1);
    }
    shown
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
