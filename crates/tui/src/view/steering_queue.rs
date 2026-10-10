//! The queued steering messages above the input box (`docs/tui.md`,
//! "Steering"): indented two columns under the working line, with no
//! stripe.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::app::App;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};

/// The queue's heading above its rows (`docs/tui.md`, "Steering").
const HEADING: &str = "• Steering, joins the turn at the next step";
/// The queue's footer under its rows (`docs/tui.md`, "Steering").
const FOOTER: &str = "⌥↑ edit · ⌥↓ next · ⌥x drop · click a row to edit, ✕ to drop";
/// The queue's two-column indent (`docs/tui.md`, "Steering").
const INDENT: &str = "  ";
/// A droppable row's `✕` with its two-space gap (`docs/tui.md`,
/// "Steering").
const CROSS: &str = "  ✕";

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
        // The row cut at the width: the `✕` draws only while it fits, so
        // the drop target never covers text.
        let mut shown = format!("{INDENT}{} {}", row.mark, row.text);
        let crossed = row.droppable
            && format::width(&shown) + format::width(CROSS) <= usize::from(area.width);
        if crossed {
            shown.push_str(CROSS);
        }
        let shown = format::cut(&shown, usize::from(area.width));
        paint_row(buf, area, y, &shown, &row.text, row.selected);
        targets.push(Target {
            id: TargetId::Steering(at),
            rect: Rect::new(area.x, y, area.width, 1),
        });
        if crossed && shown.ends_with('✕') && area.width > 0 {
            // The `✕` ends the drawn row: the target sits on its cell.
            let x = area
                .x
                .saturating_add(u16::try_from(format::width(&shown)).unwrap_or(u16::MAX));
            targets.push(Target {
                id: TargetId::DropSteering(at),
                rect: Rect::new(x.saturating_sub(1), y, 1, 1),
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

/// Paints one queue row at `y`: the indent dim, the mark and its gap dim,
/// or in `attention` while the row is selected, the text dim, or in the
/// default foreground while selected, and the row's `✕` dim
/// (`docs/tui.md`, "Steering").
fn paint_row(buf: &mut Buffer, area: Rect, y: u16, shown: &str, text: &str, selected: bool) {
    let dim = style(Role::Muted);
    // The mark's cell with its gap, and the text's cells.
    let mark = area
        .x
        .saturating_add(u16::try_from(INDENT.len()).unwrap_or(u16::MAX));
    let body = mark.saturating_add(2);
    let end = body.saturating_add(u16::try_from(format::width(text)).unwrap_or(u16::MAX));
    let mut x = area.x;
    for ch in shown.chars() {
        let paint = if x < mark {
            dim
        } else if x < body {
            // The mark with its gap: attention while selected, else dim.
            if selected {
                style(Role::Attention)
            } else {
                dim
            }
        } else if x < end {
            if selected { Style::default() } else { dim }
        } else {
            dim
        };
        buf.set_stringn(x, y, ch.to_string(), 1, paint);
        x = x.saturating_add(u16::try_from(format::width(&ch.to_string())).unwrap_or(1));
    }
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
