//! The frame every configuration view draws in (`docs/tui.md`, "Swapped
//! views"): a header with the view's title and a ✕, the rows with one
//! selected, the lines below them, an edit field when one is open, and a
//! footer naming the view's keys. Attached, a view takes the conversation
//! area; on home it takes the whole screen.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::App;
use crate::keys::Key;
use crate::mouse::{Target, TargetId};
use crate::theme::Role;

/// The selected row's style: reversed, so it shows on every theme and
/// with no colour.
const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// The footer's style.
const FOOTER: Style = Style::new().fg(Role::Muted.color());

/// A click target in a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// The header's ✕: closes the view.
    Close,
    /// A row, by its index: selects it, or chooses it in a choice list.
    Row(usize),
    /// A cell, by its row and place in it: a button, or a chip in the
    /// model picker. A view without cells never sees one.
    Cell(usize, usize),
    /// A rule row's ✕, by its index: revokes the rule
    /// (`docs/tui.md`, "Swapped views").
    Revoke(usize),
}

/// How a cell draws: plain text, a provider heading, or dimmed text
/// such as a row's roles. The selected row still reverses over it, so
/// it shows on every theme and with no colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ink {
    /// Plain text.
    Plain,
    /// A heading: bold.
    Heading,
    /// Dimmed text: the muted role's colour.
    Muted,
}

impl Ink {
    /// The cell's style.
    fn style(self) -> Style {
        match self {
            Ink::Plain => Style::default(),
            Ink::Heading => Style::new().add_modifier(Modifier::BOLD),
            Ink::Muted => Style::new().fg(Role::Muted.color()),
        }
    }
}

/// A list's selection and the first row shown.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct List {
    selected: usize,
    top: usize,
}

impl List {
    /// Moves the selection by `key` over `rows` rows shown `height` at a
    /// time: ↑ and ↓ by one, PageUp and PageDown by the height less one,
    /// clamped at both ends, scrolling to keep it shown. Whether it moved;
    /// any other key, and an empty list or area, leaves it.
    pub(crate) fn key(&mut self, key: &Key, rows: usize, height: usize) -> bool {
        let page = height.saturating_sub(1).max(1);
        let to = if *key == Key::Up {
            self.selected.saturating_sub(1)
        } else if *key == Key::Down {
            self.selected.saturating_add(1)
        } else if *key == Key::PageUp {
            self.selected.saturating_sub(page)
        } else if *key == Key::PageDown {
            self.selected.saturating_add(page)
        } else {
            return false;
        };
        let before = self.selected;
        self.select(to, rows, height);
        self.selected != before
    }

    /// Selects row `at`, clamped to the last of `rows`, and scrolls so it
    /// shows in `height` rows. An empty list or area selects nothing.
    pub(crate) fn select(&mut self, at: usize, rows: usize, height: usize) {
        if rows == 0 || height == 0 {
            return;
        }
        self.selected = at.min(rows - 1);
        self.top = self.top.min(self.selected);
        if self.selected >= self.top + height {
            self.top = self.selected + 1 - height;
        }
    }

    /// The selected row's index.
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// The first row shown.
    pub(crate) fn top(&self) -> usize {
        self.top
    }
}

/// One frame of a view, as the app builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    /// The header's title.
    pub(crate) title: String,
    /// The rows, each in cells drawn left to right, some with a click
    /// target of their own, each with how it draws.
    pub(crate) rows: Vec<Vec<(String, Option<Spot>, Ink)>>,
    /// The selection and the first row shown.
    pub(crate) list: List,
    /// Lines below the rows: what a write said, when a key applies.
    pub(crate) below: Vec<String>,
    /// The edit field while it is open: its text and the caret's place in
    /// it, in characters.
    pub(crate) field: Option<(String, usize)>,
    /// The view's keys.
    pub(crate) footer: String,
}

/// The rows a view of `height` shows for `frame`: the height less the
/// header, the lines below, the field and the footer.
pub(crate) fn rows_height(frame: &Frame, height: usize) -> usize {
    let fixed = 2 + frame.below.len() + usize::from(frame.field.is_some());
    height.saturating_sub(fixed)
}

/// Draws the open swapped view into `area`, pushing its click targets:
/// the model picker while it is open, else the open configuration view.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let height = usize::from(area.height);
    if let Some(frame) = app
        .model_picker_frame(height)
        .or_else(|| app.config_view_screen())
    {
        render(&frame, area, buf, targets);
    }
}

/// Draws `frame` into `area`: what fits, top down, the footer on the last
/// row.
pub(crate) fn render(frame: &Frame, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    buf.set_stringn(area.x, area.y, &frame.title, width, Style::default());
    let cross = Rect::new(area.right().saturating_sub(1), area.y, 1, 1);
    buf.set_string(cross.x, cross.y, "✕", Style::default());
    targets.push(Target {
        id: TargetId::View(Spot::Close),
        rect: cross,
    });
    let shown = rows_height(frame, usize::from(area.height));
    let mut y = area.y.saturating_add(1);
    for (at, cells) in frame
        .rows
        .iter()
        .enumerate()
        .skip(frame.list.top())
        .take(shown)
    {
        let line = Rect::new(area.x, y, area.width, 1);
        targets.push(Target {
            id: TargetId::View(Spot::Row(at)),
            rect: line,
        });
        let mut x = area.x;
        for (text, spot, ink) in cells {
            let (end, _) = buf.set_stringn(
                x,
                y,
                text,
                usize::from(area.right().saturating_sub(x)),
                ink.style(),
            );
            if let Some(spot) = spot {
                targets.push(Target {
                    id: TargetId::View(*spot),
                    rect: Rect::new(x, y, end.saturating_sub(x), 1),
                });
            }
            x = end;
        }
        if at == frame.list.selected() {
            buf.set_style(line, SELECTED);
        }
        y = y.saturating_add(1);
    }
    let footer = area.bottom().saturating_sub(1);
    let mut y = footer
        .saturating_sub(u16::try_from(frame.below.len()).unwrap_or(u16::MAX))
        .saturating_sub(u16::from(frame.field.is_some()))
        .max(area.y.saturating_add(1));
    for line in &frame.below {
        if y >= footer {
            break;
        }
        buf.set_stringn(area.x, y, line, width, Style::default());
        y = y.saturating_add(1);
    }
    if let Some((text, caret)) = &frame.field
        && y < footer
    {
        field(text, *caret, Rect::new(area.x, y, area.width, 1), buf);
    }
    if footer > area.y {
        buf.set_stringn(area.x, footer, &frame.footer, width, FOOTER);
    }
}

/// Draws the edit field on `line`: `> ` then the text, scrolled so the
/// caret shows, the caret's cell reversed.
fn field(text: &str, caret: usize, line: Rect, buf: &mut Buffer) {
    let room = usize::from(line.width).saturating_sub(3);
    let skip = caret.saturating_sub(room);
    let shown: String = text
        .chars()
        .skip(skip)
        .take(room.saturating_add(1))
        .collect();
    buf.set_stringn(
        line.x,
        line.y,
        format!("> {shown}"),
        usize::from(line.width),
        Style::default(),
    );
    let column = u16::try_from(caret - skip + 2).unwrap_or(u16::MAX);
    if column < line.width {
        buf.set_style(
            Rect::new(line.x.saturating_add(column), line.y, 1, 1),
            SELECTED,
        );
    }
}

/// A token count as the views say it: grouped by thousands.
pub(crate) fn about(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
#[path = "swapped_tests.rs"]
mod tests;
