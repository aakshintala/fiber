//! The drag's visuals: the edge column's tint under the pointer or while
//! dragged, and the pill naming the live share and columns
//! (`docs/tui.md`, "Layout").

use ratatui::buffer::Buffer;

use super::HOVER_TINT;
use crate::app::App;
use crate::layout::{Edge, edge_rect};
use crate::markdown::{Role, style};

/// Tints the edge under `pointer` and the edge being dragged, and draws
/// the pill naming the live share and columns while a drag runs. The tint
/// sets the background only, so the grip's `⋮` stays.
pub(super) fn draw(app: &App, buf: &mut Buffer, pointer: Option<(u16, u16)>) {
    let Some(layout) = app.chrome().layout() else {
        return;
    };
    let dragged = app.dragging().map(|(edge, _)| edge);
    for edge in [Edge::Rail, Edge::Panel, Edge::Grip] {
        let Some(rect) = edge_rect(&layout, edge) else {
            continue;
        };
        if pointer.is_some_and(|(col, _)| col == rect.x) || dragged == Some(edge) {
            buf.set_style(rect, HOVER_TINT);
        }
    }
    let Some((edge, share)) = app.dragging() else {
        return;
    };
    let Some(rect) = edge_rect(&layout, edge) else {
        return;
    };
    // A grip below the floor has no share yet, so it draws no pill.
    let Some((name, columns)) = (match edge {
        Edge::Rail => layout.rail.map(|rail| ("rail", rail.width)),
        Edge::Panel => layout.panel.map(|panel| ("panel", panel.width)),
        Edge::Grip => None,
    }) else {
        return;
    };
    let pill = crate::format::cut(
        &pill_text(name, share, columns),
        usize::from(layout.column.width.saturating_sub(2)),
    );
    let wide = u16::try_from(crate::format::width(&pill)).unwrap_or(u16::MAX);
    // The rail pill starts one column in; the panel pill ends one short
    // of the column's right edge, so a cut pill keeps its end.
    let x = match edge {
        Edge::Rail => layout.column.x.saturating_add(1),
        Edge::Panel => layout.column.right().saturating_sub(1).saturating_sub(wide),
        Edge::Grip => return,
    };
    let y = rect.y.saturating_add(rect.height / 2);
    buf.set_stringn(
        x,
        y,
        &pill,
        usize::from(wide),
        style(Role::Muted).bg(Role::Surface.color()),
    );
}

/// The pill's text: the edge's name, its live share and its columns.
/// The share prints with `f64`'s `Display`, so 18.0 reads `18`.
fn pill_text(name: &str, share: f64, columns: u16) -> String {
    format!(" {name} {share}% · {columns} cols ")
}

#[cfg(test)]
#[path = "drag_tests.rs"]
mod tests;
