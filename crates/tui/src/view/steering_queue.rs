//! The queued steering messages above the input box (`docs/tui.md`,
//! "Steering"): indented two columns under the working line, with no
//! stripe.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};

/// The queue's heading above its rows (`docs/tui.md`, "Steering").
const HEADING: &str = "• Steering, joins the turn at the next step";
/// The queue's footer under its rows (`docs/tui.md`, "Steering").
const FOOTER: &str = "⌥↑ edit · ⌥↓ next · ⌥X drop · click a row to edit, ✕ to drop";
/// The queue's two-column indent (`docs/tui.md`, "Steering").
const INDENT: &str = "  ";
/// A droppable row's `✕` with its two-space gap (`docs/tui.md`,
/// "Steering").
const CROSS: &str = "  ✕";
/// The indent, the mark and its gap in cells: the row's text starts
/// past them (`docs/tui.md`, "Steering").
const PREFIX_CELLS: usize = 4;

/// Draws the steering queue on the rows above `bottom`: the heading, one
/// row each with the row's `✕` after its text, and the footer, all dim
/// and indented two columns. A selected row's mark is in `attention` and
/// its text in the default foreground. Each row is a target over its
/// whole row, and a drawn `✕` a target over its cell
/// (`docs/tui.md`, "Steering"). Moves `bottom` above them; nothing while
/// the queue is empty.
pub(super) fn draw(
    app: &App,
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let rows = app.steering_rows();
    if rows.is_empty() {
        return;
    }
    put_row(
        area,
        bottom,
        buf,
        &format!("{INDENT}{FOOTER}"),
        style(Role::Muted),
    );
    for (at, row) in rows.iter().enumerate().rev() {
        let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
            continue;
        };
        *bottom = y;
        // The `✕` draws only while it and the row fit, so the drop
        // target never covers text.
        let crossed = row.droppable
            && PREFIX_CELLS + format::width(&row.text) + format::width(CROSS)
                <= usize::from(area.width);
        let end = paint_row(buf, area, y, &row.text, row.selected, crossed);
        targets.push(Target {
            id: TargetId::Steering(at),
            rect: Rect::new(area.x, y, area.width, 1),
        });
        if crossed {
            // The `✕` ends the drawn row: the target sits on its cell.
            targets.push(Target {
                id: TargetId::DropSteering(at),
                rect: Rect::new(end.saturating_sub(1), y, 1, 1),
            });
        }
    }
    put_row(
        area,
        bottom,
        buf,
        &format!("{INDENT}{HEADING}"),
        style(Role::Muted),
    );
}

/// Paints one queue row at `y` from its spans — the indent dim, the mark
/// with its gap dim or in `attention` while selected, the text dim or in
/// the default foreground while selected, and the row's `✕` dim — through
/// the grapheme-aware line writer, so wide glyphs draw whole and combining
/// marks stay on their base (`docs/tui.md`, "Steering"). Returns the
/// column after the drawn row.
fn paint_row(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    text: &str,
    selected: bool,
    crossed: bool,
) -> u16 {
    let dim = style(Role::Muted);
    let mut spans = vec![
        Span::styled(INDENT, dim),
        Span::styled(
            if selected { "▸ " } else { "↳ " },
            if selected {
                style(Role::Attention)
            } else {
                dim
            },
        ),
        Span::styled(text, if selected { Style::default() } else { dim }),
    ];
    if crossed {
        spans.push(Span::styled(CROSS, dim));
    }
    let (end, _) = buf.set_line(area.x, y, &Line::from(spans), area.width);
    end
}

/// Puts one queue line on the row above `bottom`, cut at the width;
/// nothing once `bottom` reaches the top of `area`.
fn put_row(area: Rect, bottom: &mut u16, buf: &mut Buffer, text: &str, paint: Style) {
    let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    buf.set_stringn(
        area.x,
        y,
        format::cut(text, usize::from(area.width)),
        usize::from(area.width),
        paint,
    );
    *bottom = y;
}

#[cfg(test)]
#[path = "steering_queue_tests.rs"]
mod tests;
