//! The frame every configuration view draws in (`docs/tui.md`, "Swapped
//! views"): a top row with the view's name, its command and `Esc
//! returns`, the rows with one selected, the lines below them, an edit
//! field when one is open, and a footer naming the view's keys.
//! Attached, a view takes the conversation area; on home it takes the
//! whole screen.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::App;
use crate::keys::Key;
use crate::mouse::{Target, TargetId};
use crate::theme::Role;

/// The columns a list with a gutter keeps before each row's text.
pub(crate) const GUTTER: usize = 2;

/// The selected row's style: reversed, so it shows on every theme and
/// with no colour.
const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// A click target in a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// The top row: closes the view.
    Close,
    /// A row, by its index: selects it, or chooses it in a choice list.
    Row(usize),
    /// A cell, by its row and place in it: a button, or a chip in the
    /// model picker. A view without cells never sees one.
    Cell(usize, usize),
    /// A switch in a row: `at` 0 is this project, 1 everywhere.
    Switch { row: usize, at: usize },
    /// A rule row's ✕, by its index: revokes the rule
    /// (`docs/tui.md`, "Swapped views").
    Revoke(usize),
    /// A delegate or job row, by its job's serial: a press and a release
    /// on the same serial opens that item, never the row's position.
    /// Only the running list draws one.
    Item(u64),
}

/// How a cell draws: plain text, a provider heading, dimmed text such
/// as a row's roles, or one role's colour. The selected row still
/// reverses over it, so it shows on every theme and with no colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ink {
    /// Plain text.
    Plain,
    /// A heading: bold.
    Heading,
    /// Dimmed text: faint in the muted role's colour.
    Muted,
    /// One role's colour, such as a context category's.
    Role(Role),
    /// One role's colour in reverse video, such as a diff's differing words.
    Marked(Role),
}

impl Ink {
    /// The cell's style.
    pub(crate) fn style(self) -> Style {
        match self {
            Ink::Plain => Style::default(),
            Ink::Heading => Style::new().add_modifier(Modifier::BOLD),
            Ink::Muted => Style::new()
                .fg(Role::Muted.color())
                .add_modifier(Modifier::DIM),
            Ink::Role(role) => Style::new().fg(role.color()),
            Ink::Marked(role) => Style::new()
                .fg(role.color())
                .add_modifier(Modifier::REVERSED),
        }
    }
}

/// A list's selection and the first row shown.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct List {
    selected: usize,
    top: usize,
    /// Whether the selected row is marked by a "› " gutter instead of the
    /// selection bar.
    gutter: bool,
}

impl List {
    /// A list that marks its selected row with a "› " gutter: every row
    /// draws two columns in, and the selected one draws the mark in them
    /// (`docs/tui.md`, "Swapped views").
    pub(crate) fn gutter() -> Self {
        Self {
            gutter: true,
            ..Self::default()
        }
    }

    /// Whether the list marks its selection with a gutter.
    pub(crate) fn is_gutter(&self) -> bool {
        self.gutter
    }

    /// The list as a frame draws it with no row focused: no row is
    /// marked and the first row shown stays. Only for the frame; the
    /// view keeps its own list.
    pub(crate) fn blur(mut self) -> Self {
        self.selected = usize::MAX;
        self
    }

    /// Moves the selection by ↑ or ↓ onto the next row in `takes`
    /// (ascending) in that direction: the rows that take Enter or a
    /// switch. Any other key, and a list with none in `takes`, moves as
    /// [`List::key`] does. Whether it moved.
    pub(crate) fn key_over(
        &mut self,
        key: &Key,
        rows: usize,
        height: usize,
        takes: &[usize],
    ) -> bool {
        if takes.is_empty() {
            return self.key(key, rows, height);
        }
        let at = self.selected;
        let to = if *key == Key::Down {
            takes.iter().copied().find(|row| *row > at)
        } else if *key == Key::Up {
            takes.iter().rev().copied().find(|row| *row < at)
        } else {
            return self.key(key, rows, height);
        };
        let Some(to) = to else {
            return false;
        };
        self.select(to, rows, height);
        self.selected != at
    }

    /// Puts the selection on a row in `takes`: the one it is on, else the
    /// next, else the last. With none in `takes` it stays.
    pub(crate) fn rest_on(&mut self, takes: &[usize], rows: usize, height: usize) {
        let at = takes
            .iter()
            .copied()
            .find(|row| *row >= self.selected)
            .or_else(|| takes.last().copied())
            .unwrap_or(self.selected);
        self.select(at, rows, height);
    }

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
    /// The slash command that opened the view, or what did for a view
    /// with no slash command: always non-empty, drawn dim on the top
    /// row after the title (`docs/tui.md`, "Swapped views").
    pub(crate) command: &'static str,
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

/// The top row: `" " + name` bold, `"  " + command` dim and `Esc
/// returns` dim right-aligned, all on the `surface` tint across the
/// full width (`docs/tui.md`, "Swapped views"). The whole row is one
/// click target closing the view, pushed before any row's own target,
/// so a row's target still wins on its cells. With no colour the row
/// is plain: bold and dim stay, with no tint and no box.
pub(crate) fn draw_top_row(
    buf: &mut Buffer,
    area: Rect,
    name: &str,
    command: &str,
    close: TargetId,
    targets: &mut Vec<Target>,
) {
    targets.push(Target {
        id: close,
        rect: Rect::new(area.x, area.y, area.width, 1),
    });
    // The tint runs the whole row, padding included; each part paints
    // its own cells over it.
    let tinted = !crate::surface::boxed();
    let paint = |style: Style| {
        if tinted {
            style.bg(Role::Surface.color())
        } else {
            style
        }
    };
    buf.set_style(
        Rect::new(area.x, area.y, area.width, 1),
        paint(Style::new()),
    );
    let layout = crate::offer::title_layout(name, command, usize::from(area.width));
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut parts = vec![(layout.name, bold)];
    if let Some(command) = layout.command {
        parts.push((command, dim));
    }
    parts.push((" ".repeat(layout.pad), Style::new()));
    if layout.esc {
        parts.push(("Esc returns".to_owned(), dim));
    }
    let mut x = area.x;
    for (text, style) in parts {
        let (end, _) = buf.set_stringn(
            x,
            area.y,
            text.as_str(),
            usize::from(area.right().saturating_sub(x)),
            paint(style),
        );
        x = end;
    }
}

/// Draws the open swapped view into `area`, pushing its click targets:
/// the model picker while it is open, else the open configuration view.
/// The picker keeps the view's top row with its title and command, and
/// draws in the overlay frame under it. Returns the framed slab for the
/// model picker and the `/login` and `/logout` views, `None` for every
/// other view, which draws full-area with no frame of its own.
pub(crate) fn draw(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Option<Rect> {
    if area.is_empty() {
        return None;
    }
    if let Some(view) = app.model_picker_view() {
        draw_top_row(
            buf,
            area,
            "Models",
            "/model",
            TargetId::View(Spot::Close),
            targets,
        );
        let below = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
        let slab = if below.is_empty() {
            Rect::default()
        } else {
            super::view::model_picker::draw(&view, below, buf, targets)
        };
        if !slab.is_empty() {
            app.note_surface(crate::app::NotedSurface {
                surface: crate::app::SurfaceId::Swapped,
                rows: super::view::model_picker::note_selection(&view, below, slab),
            });
            for rect in app.selection_cells(crate::app::SurfaceId::Swapped, slab) {
                buf.set_style(
                    rect.intersection(slab),
                    crate::view::marks::selection_style(),
                );
            }
        }
        return Some(slab);
    }
    let height = usize::from(area.height);
    if let Some(frame) = app.keys_frame(height) {
        render(&frame, area, buf, targets);
        select_frame(app, &frame, area, buf);
        None
    } else if let Some(login) = app.login_view() {
        // The login overlay reads as its own surface over the
        // conversation's rows (`docs/tui.md`, "Selection and copy").
        Some(login.draw_noted(app, area, buf, targets))
    } else if let Some(logout) = app.logout_view() {
        Some(logout.draw(area, buf, targets))
    } else if let Some(frame) = app.config_view_screen() {
        render(&frame, area, buf, targets);
        select_frame(app, &frame, area, buf);
        None
    } else if let Some(frame) = app.session_view_screen(area.width) {
        render(&frame, area, buf, targets);
        select_frame(app, &frame, area, buf);
        None
    } else {
        None
    }
}

/// Notes `frame`'s text rows for the selection and paints the
/// selection over them: each shown row's cells, the focus gutter left
/// out, with the lines below and the field's text
/// (`docs/tui.md`, "Selection and copy"). The title row and the
/// key-hint footer never copy.
pub(crate) fn select_frame(app: &App, frame: &Frame, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    let shown = rows_height(frame, usize::from(area.height));
    let mut noted = Vec::new();
    let mut y = area.y.saturating_add(1);
    for cells in frame.rows.iter().skip(frame.list.top()).take(shown) {
        let gutter = frame.list.is_gutter();
        let x = if gutter {
            area.x.saturating_add(crate::text::to_u16(GUTTER))
        } else {
            area.x
        };
        let mut text = String::new();
        for (cell, _, _) in cells {
            text.push_str(cell);
        }
        let cut = crate::format::cut(&text, usize::from(area.right().saturating_sub(x)));
        noted.push(crate::app::SpanRow {
            y,
            x,
            end: area.right(),
            text: cut,
            join: crate::rows::Join::Break,
        });
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
        noted.push(crate::app::SpanRow {
            y,
            x: area.x,
            end: area.right(),
            text: crate::format::cut(line, usize::from(area.width)),
            join: crate::rows::Join::Break,
        });
        y = y.saturating_add(1);
    }
    if let Some((text, caret)) = &frame.field
        && y < footer
    {
        // The field's `> ` prompt marks the edit; the copy reads the
        // text after it, scrolled as drawn, as the input box reads past
        // its prompt.
        let room = usize::from(area.width).saturating_sub(3);
        let skip = caret.saturating_sub(room);
        let shown: String = text
            .chars()
            .skip(skip)
            .take(room.saturating_add(1))
            .collect();
        noted.push(crate::app::SpanRow {
            y,
            x: area.x.saturating_add(2),
            end: area.right(),
            text: shown,
            join: crate::rows::Join::Break,
        });
    }
    app.note_surface(crate::app::NotedSurface {
        surface: crate::app::SurfaceId::Swapped,
        rows: noted,
    });
    for rect in app.selection_cells(crate::app::SurfaceId::Swapped, area) {
        buf.set_style(
            rect.intersection(area),
            crate::view::marks::selection_style(),
        );
    }
}

/// Draws `frame` into `area`: the top row, then what fits top down,
/// the footer on the last row.
pub(crate) fn render(frame: &Frame, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    draw_top_row(
        buf,
        area,
        &frame.title,
        frame.command,
        TargetId::View(Spot::Close),
        targets,
    );
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
        let gutter = frame.list.is_gutter();
        let mut x = area.x;
        if gutter {
            if at == frame.list.selected() {
                buf.set_stringn(x, y, "› ", GUTTER, Style::default());
            }
            x = x
                .saturating_add(crate::text::to_u16(GUTTER))
                .min(area.right());
        }
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
        if !gutter && at == frame.list.selected() {
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
        buf.set_stringn(area.x, y, line, width, Ink::Muted.style());
        y = y.saturating_add(1);
    }
    if let Some((text, caret)) = &frame.field
        && y < footer
    {
        field(text, *caret, Rect::new(area.x, y, area.width, 1), buf);
    }
    if !frame.footer.is_empty() && footer > area.y {
        buf.set_stringn(area.x, footer, &frame.footer, width, Ink::Muted.style());
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

/// A compact count as the swapped views say it: exact under 1,000,
/// whole thousands above, whole millions past that, rounded half up
/// (`docs/tui.md`, "Swapped views").
pub(crate) fn compact(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let thousands = n.saturating_add(500) / 1000;
    if thousands < 1000 {
        return format!("{thousands}k");
    }
    format!("{}M", thousands.saturating_add(500) / 1000)
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
