//! Drawing the session screen's chrome: the floor line and the rail's and
//! the panel's regions with the grip on each draggable edge (`docs/tui.md`,
//! "Layout", "Shedding"). The cards inside the rail and the panel draw on
//! top.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::layout::Layout;
use crate::markdown::{Role, style};

/// The draggable edge's grip, drawn on three rows at mid-height.
const GRIP: &str = "⋮";

/// Draws `text` centred in `area`, cut at its width.
pub(super) fn floor(text: &str, area: Rect, buf: &mut Buffer) {
    let width = u16::try_from(crate::format::width(text)).unwrap_or(u16::MAX);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area.y.saturating_add(area.height / 2);
    buf.set_stringn(x, y, text, usize::from(area.width), Style::default());
}

use ratatui::style::Style;

/// Draws the chrome of `layout`: the rail's and the panel's regions in
/// the `panel` tint, then the regions' grips. The conversation draws in
/// the column's full rect; its rows inset past the gutter where they
/// draw.
pub(super) fn draw(layout: &Layout, buf: &mut Buffer) {
    if let Some(rail) = layout.rail {
        crate::surface::draw_slab(
            buf,
            rail,
            Role::Panel,
            None,
            crate::surface::Edges {
                top: false,
                bottom: false,
            },
        );
        grip(buf, rail.right().saturating_sub(1), rail);
    }
    if let Some(at) = layout.grip {
        grip(buf, at.x, at);
    }
    if let Some(panel) = layout.panel {
        crate::surface::draw_slab(
            buf,
            panel,
            Role::Panel,
            None,
            crate::surface::Edges {
                top: false,
                bottom: false,
            },
        );
        grip(buf, panel.x, panel);
    }
}

/// `⋮` dim in column `x` on the three rows at `region`'s mid-height.
fn grip(buf: &mut Buffer, x: u16, region: Rect) {
    let mid = region.y.saturating_add(region.height / 2);
    for y in mid.saturating_sub(1)..=mid.saturating_add(1) {
        if y >= region.y && y < region.bottom() {
            buf.set_string(x, y, GRIP, style(Role::Muted));
        }
    }
}

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod tests;
