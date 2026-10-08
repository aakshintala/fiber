//! Drawing the request panel in the input box's place (`docs/tui.md`,
//! "Approvals and questions", "A question form"): its lines, its tint, and
//! a form's tabs and rows as click targets.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::Widget;

use super::{ALERT_TINT, APPROVAL_TINT, paragraph, rows, to_u16};
use crate::approvals::Panel;
use crate::mouse::{Target, TargetId};

/// Where the panel's lines land in the rows above `bottom`.
struct Layout {
    /// Each line's wrapped rows.
    rows: Vec<usize>,
    /// The panel's rows scrolled off its top.
    skip: usize,
    /// The panel's top row on screen.
    top: u16,
    /// The rows it shows.
    shown: usize,
}

impl Layout {
    /// Lays `panel` out in `area` above `bottom`. A panel taller than its
    /// rows keeps its top, except that a form scrolls by the fewest rows
    /// that keep its cursor line's last row shown.
    fn new(panel: &Panel, area: Rect, bottom: u16) -> Self {
        let rows: Vec<usize> = panel
            .lines
            .iter()
            .map(|line| rows(Line::raw(line.as_str()), area.width))
            .collect();
        let space = usize::from(bottom.saturating_sub(area.y));
        let total: usize = rows.iter().sum();
        let shown = total.min(space);
        let skip = panel.cursor.map_or(0, |cursor| {
            let end: usize = rows.iter().take(cursor.saturating_add(1)).sum();
            end.saturating_sub(space)
        });
        Self {
            rows,
            skip,
            top: bottom.saturating_sub(to_u16(shown)),
            shown,
        }
    }

    /// Line `line`'s rows on screen and how many of its own rows sit above
    /// them, scrolled off; `None` when none of its rows shows.
    fn place(&self, line: usize, area: Rect) -> Option<(Rect, usize)> {
        let start: usize = self.rows.iter().take(line).sum();
        let next = start.saturating_add(*self.rows.get(line)?);
        let end = self.skip.saturating_add(self.shown);
        let first = start.max(self.skip);
        let count = next
            .min(end)
            .checked_sub(first)
            .filter(|count| *count > 0)?;
        let y = self
            .top
            .saturating_add(to_u16(first.saturating_sub(self.skip)));
        let rect = Rect::new(area.x, y, area.width, to_u16(count));
        Some((rect, first.saturating_sub(start)))
    }
}

/// Draws `panel` on the rows above `bottom` in `area`, in its tint, pushing
/// a target per spot shown; returns its top row.
pub(super) fn draw(
    panel: &Panel,
    area: Rect,
    bottom: u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> u16 {
    let layout = Layout::new(panel, area, bottom);
    let rect = Rect::new(area.x, layout.top, area.width, to_u16(layout.shown));
    buf.set_style(
        rect,
        if panel.alert {
            ALERT_TINT
        } else {
            APPROVAL_TINT
        },
    );
    for (line, text) in panel.lines.iter().enumerate() {
        if let Some((rect, skip)) = layout.place(line, area) {
            paragraph(Line::raw(text.as_str()))
                .scroll((to_u16(skip), 0))
                .render(rect, buf);
        }
    }
    for spot in &panel.spots {
        let Some((rect, _)) = layout.place(spot.line, area) else {
            continue;
        };
        // A spot over columns sits on a line that fits one row.
        let rect = match spot.cols {
            None => rect,
            Some((from, to)) => Rect::new(
                area.x.saturating_add(from),
                rect.y,
                to.saturating_sub(from),
                1,
            ),
        };
        targets.push(Target {
            id: TargetId::Form(spot.spot),
            rect,
        });
    }
    layout.top
}

/// The text cursor's cell on the form's words row, while the panel draws
/// that row; the column stops at the last one.
pub(super) fn caret(panel: &Panel, area: Rect, bottom: u16) -> Option<Position> {
    let (line, col) = panel.caret?;
    let (rect, _) = Layout::new(panel, area, bottom).place(line, area)?;
    let col = col.min(area.width.saturating_sub(1));
    Some(Position::new(area.x.saturating_add(col), rect.y))
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
