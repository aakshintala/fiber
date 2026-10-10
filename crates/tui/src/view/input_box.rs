//! The input box: the draft's rows on their own tint with half-block edges
//! (`docs/tui.md`, "Look", "The input box").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::{put, to_u16};
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
    let fits = surface::edged(rows.len(), usize::from(area.height)) > rows.len();
    let mut bottom_edge = false;
    if fits && let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) {
        *bottom = y;
        bottom_edge = true;
    }
    let below = *bottom;
    let text = text_area(area);
    for row in rows.iter().rev() {
        put_row(buf, area, text, bottom, row);
    }
    let slab = Rect::new(area.x, *bottom, area.width, below.saturating_sub(*bottom));
    let mut top_edge = false;
    if fits && let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) {
        *bottom = y;
        top_edge = true;
    }
    surface::draw_slab(
        buf,
        slab,
        Role::Surface,
        Some(surface::Stripe {
            colour: Role::Accent,
            right: false,
        }),
        surface::Edges {
            top: top_edge,
            bottom: bottom_edge,
        },
    );
    token_targets(app, area, below, (top, rows.len()), targets);
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

/// The columns the draft's rows take: past the stripe and gap where the
/// box has them (`docs/tui.md`, "Look", "The input box").
fn text_area(area: Rect) -> Rect {
    let width = surface::inset(area.width);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width)),
        area.y,
        width,
        area.height,
    )
}

/// Puts one draft row on the row above `bottom` at the text columns,
/// moving `bottom` up to it; nothing once `bottom` reaches the top of
/// `area`.
fn put_row(buf: &mut Buffer, area: Rect, text: Rect, bottom: &mut u16, row: &str) {
    let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    buf.set_stringn(text.x, y, row, usize::from(text.width), Style::default());
    *bottom = y;
}

/// The input box's shown rows, the draft row they start at, and the
/// cursor's row in them and column: the draft wrapped past the stripe
/// and gap, at most [`App::input_height`] rows, scrolled so the cursor's
/// row shows.
pub(super) fn rows(app: &App, width: u16) -> (Vec<String>, usize, usize, u16) {
    let width = surface::inset(width);
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
/// sits in the same column as without edges, past the stripe and gap
/// (`docs/tui.md`, "Look").
pub(super) fn cursor_row(app: &App, width: u16, room: usize) -> (u16, u16) {
    let (rows, _, row, col) = rows(app, width);
    let edges = usize::from(surface::edged(rows.len(), room) > rows.len());
    (
        to_u16(rows.len().saturating_sub(row).saturating_add(edges)),
        col.saturating_add(width.saturating_sub(surface::inset(width))),
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
    let text = text_area(area);
    for span in app.input().token_spans(text.width) {
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
        let end = span.end.min(text.width);
        if span.start < end {
            targets.push(Target {
                id: TargetId::Token(span.number),
                rect: Rect::new(text.x.saturating_add(span.start), y, end - span.start, 1),
            });
        }
    }
}

#[cfg(test)]
#[path = "input_box_tests.rs"]
mod tests;
