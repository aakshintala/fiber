//! Drawing the terminal: the conversation as plain text, the notice, the
//! quit hint, the approval badge, and the input box or the approval panel
//! in its place (`docs/tui.md`, "Turns", "The input box", "Approvals and
//! questions").

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::app::{App, QUIT_HINT};

/// The overlay shown while scrolled up once new output arrives.
const NEW_BELOW: &str = "↓ New messages below";

/// The approval panel's tint when a standing rule or review asked.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
pub(crate) const APPROVAL_TINT: Style = Style::new().bg(Color::Indexed(17));

/// The approval panel's tint when the reviewer escalated.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
pub(crate) const ALERT_TINT: Style = Style::new().bg(Color::Indexed(52));

/// One plain line, wrapped the way it draws.
fn paragraph(line: &str) -> Paragraph<'_> {
    Paragraph::new(line).wrap(Wrap { trim: false })
}

/// How many rows `line` takes at `width`.
pub(crate) fn rows(line: &str, width: u16) -> usize {
    paragraph(line).line_count(width).max(1)
}

/// Draws `app` into `area` of `buf`, from the bottom up: the input box
/// on the last rows, or the approval panel in its place, then the badge, the
/// quit hint and the notice when shown, and the conversation in the rows
/// left. A screen too short for them all drops the notice first, then the
/// hint, then the badge. A panel taller than the screen keeps its top.
pub(crate) fn render(app: &App, area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width);
    let mut bottom = area.bottom();
    if let Some(panel) = app.panel() {
        let height: usize = panel.lines.iter().map(|line| rows(line, area.width)).sum();
        let top = bottom.saturating_sub(to_u16(height)).max(area.y);
        let rect = Rect::new(area.x, top, area.width, bottom.saturating_sub(top));
        let tint = if panel.alert {
            ALERT_TINT
        } else {
            APPROVAL_TINT
        };
        buf.set_style(rect, tint);
        Paragraph::new(panel.lines.join("\n"))
            .wrap(Wrap { trim: false })
            .render(rect, buf);
        bottom = top;
    }
    let mut put = |text: &str| {
        if let Some(row) = bottom.checked_sub(1).filter(|row| *row >= area.y) {
            buf.set_stringn(area.x, row, text, width, Style::default());
            bottom = row;
        }
    };
    if app.panel().is_none() {
        let (rows, _, _) = input_box(app, area.width);
        for row in rows.iter().rev() {
            put(row);
        }
    }
    if let Some(badge) = app.badge() {
        put(&badge);
    }
    if app.hint() {
        put(QUIT_HINT);
    }
    if let Some(notice) = app.notice() {
        put(notice);
    }
    let rows = bottom.saturating_sub(area.y);
    conversation_rows(app, Rect::new(area.x, area.y, area.width, rows), buf);
}

/// The input box's shown rows, and the cursor's row in them and column:
/// at most [`App::input_height`] rows, scrolled so the cursor's row shows.
fn input_box(app: &App, width: u16) -> (Vec<String>, usize, u16) {
    let draft = app.input();
    let (row, col) = draft.cursor(width);
    let height = app.input_height();
    let top = row.saturating_add(1).saturating_sub(height);
    let rows = draft
        .rows(width)
        .into_iter()
        .skip(top)
        .take(height)
        .collect();
    (rows, row.saturating_sub(top), col)
}

/// Where the terminal cursor shows: at the draft's cursor while the input
/// box has focus, `None` while the approval panel is open or the cursor's
/// row is off a screen too short for it.
pub(crate) fn cursor(app: &App, area: Rect) -> Option<Position> {
    if app.panel().is_some() {
        return None;
    }
    let (rows, row, col) = input_box(app, area.width);
    let below = to_u16(rows.len().saturating_sub(row));
    let y = area.bottom().checked_sub(below).filter(|y| *y >= area.y)?;
    let x = area.x.saturating_add(col.min(area.width.saturating_sub(1)));
    Some(Position::new(x, y))
}

/// Draws the conversation's visible rows, bottom-aligned while it is
/// shorter than its area.
fn conversation_rows(app: &App, area: Rect, buf: &mut Buffer) {
    let lines = app.lines();
    let heights: Vec<usize> = lines.iter().map(|line| rows(line, area.width)).collect();
    let total: usize = heights.iter().sum();
    let height = usize::from(area.height);
    let bottom_top = total.saturating_sub(height);
    let top = app.top().map_or(bottom_top, |top| top.min(bottom_top));
    let end = top.saturating_add(height);
    let shown = total.min(end).saturating_sub(top);
    let mut y = area.y.saturating_add(to_u16(height.saturating_sub(shown)));
    let mut start = 0usize;
    // A line wholly above `top` or below `end` shows no rows.
    for (line, rows) in lines.iter().zip(heights) {
        let next = start.saturating_add(rows);
        let skip = top.saturating_sub(start);
        let count = next.min(end).saturating_sub(start.max(top));
        let rect = Rect::new(area.x, y, area.width, to_u16(count));
        paragraph(line).scroll((to_u16(skip), 0)).render(rect, buf);
        y = y.saturating_add(to_u16(count));
        start = next;
    }
    if app.has_new() && area.height > 0 {
        let row = area.bottom().saturating_sub(1);
        let blank = " ".repeat(usize::from(area.width));
        buf.set_stringn(
            area.x,
            row,
            blank,
            usize::from(area.width),
            Style::default(),
        );
        let width = to_u16(NEW_BELOW.chars().count());
        let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
        buf.set_stringn(x, row, NEW_BELOW, usize::from(area.width), Style::default());
    }
}

/// A row count as a screen coordinate; a screen is never taller than
/// `u16::MAX`.
fn to_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// The screen as text, one row per line, trailing spaces trimmed.
pub(crate) fn text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
