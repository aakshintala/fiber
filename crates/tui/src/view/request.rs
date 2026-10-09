//! Drawing the request panel in the input box's place (`docs/tui.md`,
//! "Approvals and questions", "A question form"): its lines, its tint, and
//! a form's tabs and rows as click targets.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::{ALERT_TINT, APPROVAL_TINT, paragraph, rows, to_u16};
use crate::approvals::{IRREVERSIBLE, Panel};
use crate::mouse::{Target, TargetId};
use crate::surface;
use crate::theme::Role;

/// "irreversible" in an approval's header: bold, in the error colour, on
/// whatever tint asked (`docs/tui.md`, "An approval").
const LOUD: Style = Style::new()
    .fg(Role::Error.color())
    .add_modifier(Modifier::BOLD);

/// Panel line `line`, `text`: the header's "irreversible" loud.
fn styled(line: usize, text: &str) -> Line<'_> {
    match text.strip_suffix(IRREVERSIBLE) {
        Some(head) if line == 0 => Line::from(vec![
            Span::raw(head),
            Span::raw(" · "),
            Span::styled("irreversible", LOUD),
        ]),
        Some(_) | None => Line::raw(text),
    }
}

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
    /// Lays `panel` out in `area` above `bottom`: one surface with its
    /// text inset two columns, the stripe's and the gap's, where they fit
    /// (`docs/tui.md`, "Look"). A panel taller than its rows keeps its
    /// top, except that a form scrolls by the fewest rows that keep its
    /// cursor line's last row shown.
    fn new(panel: &Panel, area: Rect, bottom: u16) -> Self {
        let width = surface::inset(area.width);
        let rows: Vec<usize> = panel
            .lines
            .iter()
            .map(|line| rows(Line::raw(line.as_str()), width))
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
    /// them, scrolled off; `None` when none of its rows shows. The rect
    /// sits on the inset: past the stripe and the gap where they fit.
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
        let width = surface::inset(area.width);
        let x = area.x.saturating_add(area.width.saturating_sub(width));
        let rect = Rect::new(x, y, width, to_u16(count));
        Some((rect, first.saturating_sub(start)))
    }
}

/// The panel's text rows at the column's width: its lines wrapped at
/// the inset (`docs/tui.md`, "Look").
fn text_rows(panel: &Panel, width: u16) -> usize {
    panel
        .lines
        .iter()
        .map(|line| rows(Line::raw(line.as_str()), surface::inset(width)))
        .sum()
}

/// The panel's rows at the column's width: its text rows with the edge
/// rows where they fit (`docs/tui.md`, "Look").
pub(crate) fn height(panel: &Panel, width: u16, room: usize) -> usize {
    surface::edged(text_rows(panel, width), room)
}

/// Whether the panel's edges draw: its text rows with an edge row above
/// and below fit the body `room` high (`docs/tui.md`, "Look").
fn edged(panel: &Panel, width: u16, room: usize) -> bool {
    let rows = text_rows(panel, width);
    surface::edged(rows, room) > rows
}

/// Draws `panel` on the rows above `bottom` in `area`: one surface in
/// its tint with edges where they fit, the stripe on an approval's rows,
/// pushing a target per spot shown; returns the row above its top edge,
/// so the conversation never draws over it
/// (`docs/tui.md`, "Approvals and questions", "Look").
pub(super) fn draw(
    panel: &Panel,
    area: Rect,
    bottom: u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> u16 {
    let room = usize::from(area.height);
    // The edges draw only when the rows with an edge above and below
    // fit; the text then sits one row above `bottom`, leaving the bottom
    // edge its row.
    let fits = edged(panel, area.width, room);
    let bottom = bottom.saturating_sub(u16::from(fits));
    let layout = Layout::new(panel, area, bottom);
    let tint = if panel.alert {
        Role::Alert
    } else {
        Role::Approval
    };
    let rect = Rect::new(area.x, layout.top, area.width, to_u16(layout.shown));
    buf.set_style(
        rect,
        if panel.alert {
            ALERT_TINT
        } else {
            APPROVAL_TINT
        },
    );
    // The stripe marks an approval; a form's stripe cell keeps the tint
    // (`docs/tui.md`, "Look"). Too narrow for a stripe, there is none.
    if panel.cursor.is_none() && area.width >= 3 {
        surface::draw_stripe(
            buf,
            rect,
            if panel.alert {
                Role::Error
            } else {
                Role::Attention
            },
            tint,
            false,
        );
    }
    for (line, text) in panel.lines.iter().enumerate() {
        if let Some((rect, skip)) = layout.place(line, area) {
            paragraph(styled(line, text))
                .scroll((to_u16(skip), 0))
                .render(rect, buf);
        }
    }
    if fits {
        surface::draw_edges(buf, rect, tint);
    }
    for spot in &panel.spots {
        let Some((rect, _)) = layout.place(spot.line, area) else {
            continue;
        };
        // A spot over columns sits on a line that fits one row.
        let rect = match spot.cols {
            None => rect,
            Some((from, to)) => Rect::new(
                rect.x.saturating_add(from),
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
    layout.top.saturating_sub(u16::from(fits))
}

/// The text cursor's cell on the form's words row, while the panel draws
/// that row; the column stops at the inset's last one.
pub(super) fn caret(panel: &Panel, area: Rect, bottom: u16) -> Option<Position> {
    let (line, col) = panel.caret?;
    let room = usize::from(area.height);
    let bottom = bottom.saturating_sub(u16::from(edged(panel, area.width, room)));
    let (rect, _) = Layout::new(panel, area, bottom).place(line, area)?;
    let width = surface::inset(area.width);
    let col = col.min(width.saturating_sub(1));
    Some(Position::new(rect.x.saturating_add(col), rect.y))
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
