//! Drawing the terminal: the conversation as plain text, the notice, the
//! quit hint and the input line (`docs/tui.md`, "Turns").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::app::{App, QUIT_HINT};

/// The overlay shown while scrolled up once new output arrives.
const NEW_BELOW: &str = "↓ New messages below";

/// One plain line, wrapped the way it draws.
fn paragraph(line: &str) -> Paragraph<'_> {
    Paragraph::new(line).wrap(Wrap { trim: false })
}

/// How many rows `line` takes at `width`.
pub(crate) fn rows(line: &str, width: u16) -> usize {
    paragraph(line).line_count(width).max(1)
}

/// Draws `app` into `area` of `buf`, from the bottom up: the input line
/// on the last row, then the quit hint and the notice when shown, and the
/// conversation in the rows left. A screen too short for them all drops
/// the notice first, then the hint.
pub(crate) fn render(app: &App, area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width);
    let mut bottom = area.bottom();
    let mut put = |text: &str| {
        if let Some(row) = bottom.checked_sub(1).filter(|row| *row >= area.y) {
            buf.set_stringn(area.x, row, text, width, Style::default());
            bottom = row;
        }
    };
    // The input line keeps the end of a draft wider than the screen.
    let input = format!("> {}", app.draft());
    let skip = input.chars().count().saturating_sub(width);
    let shown: String = input.chars().skip(skip).collect();
    put(&shown);
    if app.hint() {
        put(QUIT_HINT);
    }
    if let Some(notice) = app.notice() {
        put(notice);
    }
    let rows = bottom.saturating_sub(area.y);
    conversation_rows(app, Rect::new(area.x, area.y, area.width, rows), buf);
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
