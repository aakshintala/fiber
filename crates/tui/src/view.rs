//! Drawing the terminal: the conversation's styled lines, the notices over
//! it, the quit hint, the approval badge, the steering queue, and the input
//! line or the approval panel in its place (`docs/tui.md`, "Turns",
//! "Steering", "Notices", "Approvals and questions").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
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

/// A notice's tint.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const NOTICE_TINT: Style = Style::new().bg(Color::Indexed(236));

/// One line, wrapped the way it draws.
fn paragraph(line: Line<'_>) -> Paragraph<'_> {
    Paragraph::new(line).wrap(Wrap { trim: false })
}

/// How many rows `line` takes at `width`.
pub(crate) fn rows(line: Line<'_>, width: u16) -> usize {
    paragraph(line).line_count(width).max(1)
}

/// Draws `app` into `area` of `buf`, from the bottom up: the input line
/// on the last row, or the approval panel in its place, then the steering
/// queue, the badge and the quit hint when shown, and the conversation in
/// the rows left, the notices floating over its top-right corner. A screen
/// too short for them all drops the hint first, then the badge. A panel taller than the screen keeps its top.
pub(crate) fn render(app: &App, area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width);
    let mut bottom = area.bottom();
    if let Some(panel) = app.panel() {
        let height: usize = panel
            .lines
            .iter()
            .map(|line| rows(Line::raw(line.as_str()), area.width))
            .sum();
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
        // The input line keeps the end of a draft wider than the screen.
        let input = format!("> {}", app.draft());
        let skip = input.chars().count().saturating_sub(width);
        let shown: String = input.chars().skip(skip).collect();
        put(&shown);
    }
    // The steering queue sits above the input box, its newest row lowest.
    for row in app.steering().iter().rev() {
        put(row);
    }
    if let Some(badge) = app.badge() {
        put(&badge);
    }
    if app.hint() {
        put(QUIT_HINT);
    }
    let rows = bottom.saturating_sub(area.y);
    let conversation = Rect::new(area.x, area.y, area.width, rows);
    conversation_rows(app, conversation, buf);
    notices(app, conversation, buf);
}

/// Floats the notices over the conversation's top-right corner, newest on
/// top, or the notice overlay over the whole conversation while it is open.
fn notices(app: &App, area: Rect, buf: &mut Buffer) {
    let mut y = area.y;
    if let Some(texts) = app.notice_overlay() {
        let rows: Vec<String> = texts
            .iter()
            .flat_map(|text| crate::format::wrap(text, usize::from(area.width)))
            .collect();
        for row in rows {
            if y >= area.bottom() {
                return;
            }
            let width = usize::from(area.width);
            let blank = " ".repeat(width);
            buf.set_stringn(area.x, y, blank, width, NOTICE_TINT);
            buf.set_stringn(area.x, y, &row, width, NOTICE_TINT);
            y = y.saturating_add(1);
        }
        return;
    }
    for notice in app.notices() {
        for row in notice.rows {
            if y >= area.bottom() {
                return;
            }
            let wide = to_u16(crate::format::width(&row)).min(area.width);
            let x = area.right().saturating_sub(wide);
            buf.set_stringn(x, y, &row, usize::from(wide), NOTICE_TINT);
            y = y.saturating_add(1);
        }
    }
}

/// Draws the conversation's visible rows, bottom-aligned while it is
/// shorter than its area.
fn conversation_rows(app: &App, area: Rect, buf: &mut Buffer) {
    let lines = app.lines();
    let heights: Vec<usize> = lines
        .iter()
        .map(|line| rows(line.clone(), area.width))
        .collect();
    let total: usize = heights.iter().sum();
    let height = usize::from(area.height);
    let bottom_top = total.saturating_sub(height);
    let top = app.top().map_or(bottom_top, |top| top.min(bottom_top));
    let end = top.saturating_add(height);
    let shown = total.min(end).saturating_sub(top);
    let mut y = area.y.saturating_add(to_u16(height.saturating_sub(shown)));
    let mut start = 0usize;
    // A line wholly above `top` or below `end` shows no rows.
    for (line, rows) in lines.into_iter().zip(heights) {
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

/// Where the conversation is scrolled to.
#[derive(Debug, Default)]
pub(crate) struct Scroll {
    /// The top wrapped row while scrolled up; `None` follows new output.
    pub(crate) top: Option<usize>,
    /// New output arrived while scrolled up.
    pub(crate) has_new: bool,
}

impl Scroll {
    /// New output while scrolled up shows the overlay; the view stays put.
    pub(crate) fn changed(&mut self) {
        if self.top.is_some() {
            self.has_new = true;
        }
    }

    /// PageUp: up by `step` from the top row, or from `bottom` when
    /// following.
    pub(crate) fn up(&mut self, step: usize, bottom: usize) {
        let top = self.top.unwrap_or(bottom);
        self.top = Some(top.saturating_sub(step));
    }

    /// PageDown: down by `step`, following again on reaching `bottom`.
    pub(crate) fn down(&mut self, step: usize, bottom: usize) {
        let Some(top) = self.top else {
            return;
        };
        let next = top.saturating_add(step);
        if next >= bottom {
            self.follow();
        } else {
            self.top = Some(next);
        }
    }

    /// End jumps to the bottom and resumes following.
    pub(crate) fn follow(&mut self) {
        self.top = None;
        self.has_new = false;
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
