//! Selecting panel card text (`docs/tui.md`, "Selection and copy").
//!
//! Reads the existing card rows; their content stays owned by the card
//! builders and PR 3315. A cut row copies what it shows, without the
//! "…" the fit adds.

use ratatui::text::Line;

use super::Row;
use crate::app::{App, NotedSurface, SpanRow, SurfaceId};
use crate::text::to_u16;

/// A text row's copy text at `text` columns: its spans joined, with one
/// trailing cut marker dropped when the row fills its columns (a row
/// that fits keeps a trailing "…" it shows). Trailing pad stays in the
/// text; the copy drops trailing spaces on each row.
pub(super) fn row_text(line: &Line<'static>, text: usize) -> String {
    let mut out: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    if out.ends_with('…') && crate::format::width(&out) >= text {
        out.pop();
    }
    out
}

/// Notes the panel's shown text rows: `rows` from the draw, `skip`
/// scrolled off, the first shown on `first`, text at `x`, stopping
/// before `bottom`. Edge rows never note: stripes, half-block edges and
/// borders are decoration, not text.
pub(super) fn note(
    app: &App,
    rows: &[Row],
    skip: usize,
    x: u16,
    first: u16,
    bottom: u16,
    text: usize,
) {
    let mut noted = Vec::new();
    let mut y = first;
    for row in rows.iter().skip(skip) {
        if y >= bottom {
            break;
        }
        if !row.edge {
            let text = row_text(&row.line, text);
            noted.push(SpanRow {
                y,
                x,
                end: x.saturating_add(to_u16(crate::format::width(&text))),
                text,
                join: crate::rows::Join::Break,
            });
        }
        y = y.saturating_add(1);
    }
    app.note_surface(NotedSurface {
        surface: SurfaceId::Panel,
        rows: noted,
    });
}
