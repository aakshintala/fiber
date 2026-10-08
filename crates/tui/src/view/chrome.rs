//! Drawing the session screen's chrome: the floor line, the header row
//! at the top of the conversation column, and the rail's and the panel's
//! regions with the grip on each draggable edge (`docs/tui.md`, "Layout",
//! "Shedding"). The cards inside the rail and the panel draw on top.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::app::App;
use crate::layout::Layout;
use crate::markdown::{Role, style};

/// The rail's and the panel's background.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const REGION_TINT: Style = Style::new().bg(Color::Indexed(235));

/// The draggable edge's grip, drawn on three rows at mid-height.
const GRIP: &str = "⋮";

/// Draws `text` centred in `area`, cut at its width.
pub(super) fn floor(text: &str, area: Rect, buf: &mut Buffer) {
    let width = u16::try_from(crate::format::width(text)).unwrap_or(u16::MAX);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area.y.saturating_add(area.height / 2);
    buf.set_stringn(x, y, text, usize::from(area.width), Style::default());
}

/// Draws the chrome of `layout`: the regions' tints and grips and the
/// header. Returns the rect the conversation draws in: the column below
/// its header row.
pub(super) fn draw(app: &App, layout: &Layout, buf: &mut Buffer) -> Rect {
    if let Some(rail) = layout.rail {
        buf.set_style(rail, REGION_TINT);
        grip(buf, rail.right().saturating_sub(1), rail);
    }
    if let Some(at) = layout.grip {
        grip(buf, at.x, at);
    }
    if let Some(panel) = layout.panel {
        buf.set_style(panel, REGION_TINT);
        grip(buf, panel.x, panel);
    }
    let column = layout.column;
    if column.height > 0 {
        buf.set_stringn(
            column.x,
            column.y,
            app.header(),
            usize::from(column.width),
            Style::default(),
        );
    }
    body(layout)
}

/// The rect the conversation draws in: the column below its header row.
pub(crate) fn body(layout: &Layout) -> Rect {
    let column = layout.column;
    Rect {
        y: column.y.saturating_add(1),
        height: column.height.saturating_sub(1),
        ..column
    }
}

/// `⋮` dim in column `x` on the three rows at `region`'s mid-height.
fn grip(buf: &mut Buffer, x: u16, region: Rect) {
    let mid = region.y.saturating_add(region.height / 2);
    for y in mid.saturating_sub(1)..=mid.saturating_add(1) {
        if y >= region.y && y < region.bottom() {
            buf.set_string(x, y, GRIP, style(Role::Dim));
        }
    }
}

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod tests;
