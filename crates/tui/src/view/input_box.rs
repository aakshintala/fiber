//! The input box: the draft's rows on their own tint with half-block edges
//! (`docs/tui.md`, "Look", "The input box").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::{SURFACE_TINT, put, to_u16};
use crate::app::App;
use crate::mouse::{Target, TargetId};
use crate::surface;
use crate::theme::Role;

/// Draws the input box on the rows above `bottom`: its rows on the surface
/// tint with edges where they fit, token targets over them, and the
/// completion panel above its top edge (`docs/tui.md`, "Look"). Moves
/// `bottom` above it all.
pub(super) fn draw(
    app: &App,
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let (rows, top, _, _) = rows(app, area.width);
    let edges = surface::edged(rows.len(), usize::from(area.height)) > rows.len();
    if edges {
        edge(buf, area, bottom, false);
    }
    let below = *bottom;
    for row in rows.iter().rev() {
        put(buf, area, bottom, row, Style::default());
    }
    if *bottom < below {
        buf.set_style(
            Rect::new(area.x, *bottom, area.width, below.saturating_sub(*bottom)),
            SURFACE_TINT,
        );
    }
    token_targets(app, area, below, (top, rows.len()), targets);
    if edges {
        edge(buf, area, bottom, true);
    }
    if let Some(completions) = app.completions() {
        for (at, line) in completions.lines.iter().enumerate().rev() {
            let style = if completions.selected == Some(at) {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            put(buf, area, bottom, line, style);
        }
    }
}

/// One edge row above (`top`) or below the box, moving `bottom` past it
/// where it fits.
fn edge(buf: &mut Buffer, area: Rect, bottom: &mut u16, top: bool) {
    let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    buf.set_line(
        area.x,
        y,
        &surface::edge_row(usize::from(area.width), Role::Surface, top),
        area.width,
    );
    *bottom = y;
}

/// The input box's shown rows, the draft row they start at, and the
/// cursor's row in them and column: at most [`App::input_height`] rows,
/// scrolled so the cursor's row shows.
pub(super) fn rows(app: &App, width: u16) -> (Vec<String>, usize, usize, u16) {
    let draft = app.input();
    let (row, col) = draft.cursor(width);
    let height = app.input_height();
    let top = row.saturating_add(1).saturating_sub(height);
    let rows = draft
        .rows(width)
        .into_iter()
        .skip(top)
        .take(height)
        .collect();
    (rows, top, row.saturating_sub(top), col)
}

/// The cursor's rows above the column's bottom and its column: the shown
/// rows below the cursor's with the bottom edge below them, so the cursor
/// sits in the same column as without edges (`docs/tui.md`, "Look").
pub(super) fn cursor_row(app: &App, width: u16, room: usize) -> (u16, u16) {
    let (rows, _, row, col) = rows(app, width);
    let edges = usize::from(surface::edged(rows.len(), room) > rows.len());
    (
        to_u16(rows.len().saturating_sub(row).saturating_add(edges)),
        col,
    )
}

/// Pushes a target over the cells of each paste token's label the input
/// box shows: the box's `shown` rows, from draft row `top`, end on the row
/// above `below`.
fn token_targets(
    app: &App,
    area: Rect,
    below: u16,
    (top, shown): (usize, usize),
    targets: &mut Vec<Target>,
) {
    for span in app.input().token_spans(area.width) {
        let Some(up) = span
            .row
            .checked_sub(top)
            .and_then(|at| shown.checked_sub(at))
            .filter(|up| *up > 0)
        else {
            continue;
        };
        let Some(y) = below.checked_sub(to_u16(up)).filter(|y| *y >= area.y) else {
            continue;
        };
        let end = span.end.min(area.width);
        if span.start < end {
            targets.push(Target {
                id: TargetId::Token(span.number),
                rect: Rect::new(area.x.saturating_add(span.start), y, end - span.start, 1),
            });
        }
    }
}

#[cfg(test)]
#[path = "input_box_tests.rs"]
mod tests;
