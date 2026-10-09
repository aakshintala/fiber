//! The queued steering messages above the input box, each with the
//! steering stripe (`docs/tui.md`, "Steering", "Look").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use crate::app::App;
use crate::mouse::{Target, TargetId};
use crate::surface;
use crate::theme::Role;

/// Draws the steering queue on the rows above `bottom`, its newest row
/// lowest: one row each with the steering stripe, a ✕ dropping a droppable
/// row on its last column, each row a target over its whole row
/// (`docs/tui.md`, "Steering"). Moves `bottom` above them.
pub(super) fn draw(
    app: &App,
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let drops = app.steering_drops();
    for (at, row) in app.steering().iter().enumerate().rev() {
        let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
            continue;
        };
        *bottom = y;
        if area.width >= 3 {
            // The stripe, then a blank, then the text the queue cut at the
            // inset width (`docs/tui.md`, "Look").
            buf.set_line(
                area.x,
                y,
                &Line::from(surface::stripe_cell(Role::Accent, Role::Background, false)),
                1,
            );
            buf.set_stringn(
                area.x.saturating_add(2),
                y,
                row,
                usize::from(area.width.saturating_sub(2)),
                Style::default(),
            );
        } else {
            buf.set_stringn(area.x, y, row, usize::from(area.width), Style::default());
        }
        targets.push(Target {
            id: TargetId::Steering(at),
            rect: Rect::new(area.x, y, area.width, 1),
        });
        if drops.get(at) == Some(&true) && area.width > 0 {
            let close = Rect::new(area.right().saturating_sub(1), y, 1, 1);
            buf.set_string(close.x, close.y, "✕", Style::default());
            targets.push(Target {
                id: TargetId::DropSteering(at),
                rect: close,
            });
        }
    }
}

#[cfg(test)]
#[path = "steering_queue_tests.rs"]
mod tests;
