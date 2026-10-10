//! The input box: the draft's rows on their own tint with half-block edges
//! (`docs/tui.md`, "Look", "The input box").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::{put, to_u16};
use crate::app::App;
use crate::completion_rows::Rows;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};
use crate::surface;

/// The hint at the box's right end while a queued message is edited
/// (`docs/tui.md`, "Steering").
const EDITING_HINT: &str = "editing a queued message · enter amends · ⌥x drops · esc stops";

/// Draws the input box on the rows above `bottom`: its rows on the surface
/// tint with edges where they fit, token targets over them, and the Ctrl+R
/// panel above its top edge (`docs/tui.md`, "Look"). The `/` and `@`
/// panels draw over the conversation instead, so they reserve nothing.
/// Moves `bottom` above it all and returns the box's top-edge row, so the
/// `/` and `@` overlay sits above it.
pub(super) fn draw(
    app: &App,
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> u16 {
    let (rows, top, cursor_row, cursor_col) = rows(app, area.width);
    let fits = surface::edged(rows.len(), usize::from(area.height)) > rows.len();
    let mut bottom_edge = false;
    if fits && let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) {
        *bottom = y;
        bottom_edge = true;
    }
    let below = *bottom;
    let text = text_area(area);
    for (at, row) in rows.iter().enumerate().rev() {
        // The first draft row opens with the prompt in `info`
        // (`docs/tui.md`, "The input box").
        put_row(buf, area, text, bottom, row, top == 0 && at == 0);
    }
    let slab = Rect::new(area.x, *bottom, area.width, below.saturating_sub(*bottom));
    let mut top_edge = false;
    if fits && let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) {
        *bottom = y;
        top_edge = true;
    }
    // Above the box's top edge: the box's own top row, whatever drew it.
    let box_top = *bottom;
    // While a queued message is edited the stripe is in `attention`
    // (`docs/tui.md`, "Steering").
    let stripe = if app.steering_selected().is_some() {
        Role::Attention
    } else {
        Role::Accent
    };
    surface::draw_slab(
        buf,
        slab,
        Role::Surface,
        Some(surface::Stripe {
            colour: stripe,
            right: false,
        }),
        surface::Edges {
            top: top_edge,
            bottom: bottom_edge,
        },
    );
    if app.steering_selected().is_some() {
        draw_editing_hint(area, below, &rows, (cursor_row, cursor_col), buf);
    }
    if cursor_shown(app) {
        let (x, y) = caret_cell(area, below, &rows, cursor_row, cursor_col);
        // The cursor is a drawn dim `█` (`docs/tui.md`, "The input box").
        // The terminal's own cursor stays where `super::cursor` puts it.
        // The row always sits inside the area: the caret's row shows.
        buf.set_stringn(x, y, "█", 1, style(Role::Muted));
    }
    token_targets(app, area, below, (top, rows.len()), targets);
    // Only the Ctrl+R panel draws here, in its reserved rows; the `/`
    // and `@` panels draw over the conversation after it.
    if let Some(completions) = app.completions()
        && matches!(completions.rows, Rows::Search(_))
    {
        let lines = completions.lines();
        for (at, line) in lines.iter().enumerate().rev() {
            let style = if completions.selected == Some(at) {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            put(buf, area, bottom, line, style);
        }
    }
    box_top
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
/// `area`. The prompt opens the first draft row in `info`, and the rest
/// of every row is the default foreground (`docs/tui.md`, "The input
/// box").
fn put_row(buf: &mut Buffer, area: Rect, text: Rect, bottom: &mut u16, row: &str, prompt: bool) {
    let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    if prompt {
        // The prompt is the row's first two cells (`Draft::rows`).
        let cut: String = row.chars().take(2).collect();
        let rest: String = row.chars().skip(2).collect();
        let (end, _) = buf.set_stringn(text.x, y, &cut, usize::from(text.width), style(Role::Info));
        let room = text.width.saturating_sub(end.saturating_sub(text.x));
        buf.set_stringn(end, y, &rest, usize::from(room), Style::default());
    } else {
        buf.set_stringn(text.x, y, row, usize::from(text.width), Style::default());
    }
    *bottom = y;
}

/// Whether the input box draws its cursor: the native cursor shows on
/// the box exactly then (`super::cursor`).
fn cursor_shown(app: &App) -> bool {
    app.chrome().floor_line().is_none()
        && !app.config_view_open()
        && !app.model_picker_open()
        && !app.keys_screen_open()
        && !app.session_view_open()
        && app.focused().is_none()
        && app.panel().is_none()
        && !app.offer_open()
        && app.find_bar().is_none()
}

/// The caret's cell in `area`: the cursor's column past the stripe and
/// gap, on the cursor's shown row above `below` (`docs/tui.md",
/// "The input box").
fn caret_cell(area: Rect, below: u16, rows: &[String], row: usize, col: u16) -> (u16, u16) {
    let text = text_area(area);
    (
        text.x.saturating_add(col),
        below
            .saturating_sub(to_u16(rows.len()))
            .saturating_add(to_u16(row)),
    )
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

/// Draws the editing hint at the box's right end on its last text row,
/// right-aligned and dim: only over cells past the draft's text and never
/// over the caret, so a long draft clips it at the right edge instead of
/// covering text (`docs/tui.md`, "Steering").
fn draw_editing_hint(
    area: Rect,
    below: u16,
    rows: &[String],
    (cursor_row, cursor_col): (usize, u16),
    buf: &mut Buffer,
) {
    let Some(last) = rows.last() else {
        return;
    };
    let Some(y) = below.checked_sub(1).filter(|y| *y >= area.y) else {
        return;
    };
    let text = text_area(area);
    let end = text.x.saturating_add(to_u16(format::width(last)));
    let caret = caret_cell(area, below, rows, cursor_row, cursor_col);
    // The hint is a fixed literal of one-cell characters, drawn
    // right-aligned only over free cells: past the draft's text, never
    // over the caret (`docs/tui.md`, "Steering"). Cells past the box
    // clip in the buffer write.
    let mut x = area
        .right()
        .saturating_sub(to_u16(format::width(EDITING_HINT)));
    for ch in EDITING_HINT.chars() {
        let wide = to_u16(format::width(&ch.to_string()));
        if x >= end && (x, y) != caret {
            buf.set_stringn(x, y, ch.to_string(), usize::from(wide), style(Role::Muted));
        }
        x = x.saturating_add(wide.max(1));
    }
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
