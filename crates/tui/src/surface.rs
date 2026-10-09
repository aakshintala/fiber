//! Surfaces: the tinted cards, boxes and bubbles the conversation sits on,
//! their half-block edges and state stripes (`docs/tui.md`, "Look").
//!
//! A stripe is one unbroken bar where the terminal draws half blocks that
//! way (Ghostty, WezTerm and kitty, outside a multiplexer); elsewhere the
//! stripe's cell keeps its tint, so text keeps its columns and row counts
//! never depend on the terminal.

use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::look::Var;
use crate::theme::Role;

/// Whether stripes draw, read once at start.
static STRIPES: AtomicBool = AtomicBool::new(true);

/// Reads whether stripes draw into [`STRIPES`]; `run` calls it once before
/// the first frame.
pub(crate) fn init(var: Var<'_>) {
    STRIPES.store(stripes(var), Ordering::Relaxed);
}

/// Whether stripes draw on the terminal `var` describes (`docs/tui.md`,
/// "Look"): never inside a multiplexer, which may not draw half blocks
/// unbroken; otherwise in Ghostty, WezTerm and kitty.
pub(crate) fn stripes(var: Var<'_>) -> bool {
    if var("TMUX").is_some()
        || var("STY").is_some()
        || var("TERM").unwrap_or_default().starts_with("tmux")
        || var("TERM").unwrap_or_default().starts_with("screen")
    {
        return false;
    }
    var("TERM_PROGRAM").is_some_and(|program| program == "ghostty" || program == "WezTerm")
        || var("TERM").is_some_and(|term| term == "xterm-ghostty" || term == "xterm-kitty")
}

/// The text width of a surface `width` wide with a stripe on its left: the
/// stripe and one gap column (`docs/tui.md`, "Look"). Too narrow for both,
/// there is no stripe and the text keeps the width.
pub(crate) fn inset(width: u16) -> u16 {
    if width >= 3 {
        width.saturating_sub(2)
    } else {
        width
    }
}

/// The rows a bottom surface of `rows` rows takes in a body `room` high:
/// its rows with an edge row above and below, which draw only when both
/// fit (`docs/tui.md`, "Look").
pub(crate) fn edged(rows: usize, room: usize) -> usize {
    if rows.saturating_add(2) <= room {
        rows.saturating_add(2)
    } else {
        rows
    }
}

/// An edge row `width` wide in `tint`: half blocks above (`top`) or below
/// the surface, with no borders (`docs/tui.md`, "Look"). With no colour
/// the paint pass blanks it, and the row stays.
pub(crate) fn edge_row(width: usize, tint: Role, top: bool) -> Line<'static> {
    let edge = if top { "▄" } else { "▀" };
    Line::from(Span::styled(
        edge.repeat(width),
        Style::new().fg(tint.color()),
    ))
}

/// The stripe's cell in `colour` on `tint`, on the `right` of its surface:
/// `▌` on the left, `▐` on the right (`docs/tui.md`, "Look").
pub(crate) fn stripe_cell(colour: Role, tint: Role, right: bool) -> Span<'static> {
    stripe(STRIPES.load(Ordering::Relaxed), colour, tint, right)
}

/// The stripe's cell when stripes draw (`on`): the half block in the
/// state's colour on the tint, else a tinted blank, so text keeps its
/// columns where no stripe draws (`docs/tui.md`, "Look").
fn stripe(on: bool, colour: Role, tint: Role, right: bool) -> Span<'static> {
    if on {
        Span::styled(
            if right { "▐" } else { "▌" },
            Style::new().fg(colour.color()).bg(tint.color()),
        )
    } else {
        Span::styled(" ", Style::new().bg(tint.color()))
    }
}

/// Draws the edge rows above and below `rect` in `tint`, where they sit
/// inside `buf` (`docs/tui.md`, "Look").
pub(crate) fn draw_edges(buf: &mut Buffer, rect: Rect, tint: Role) {
    if rect.width == 0 {
        return;
    }
    let area = buf.area;
    if rect.y > area.y {
        buf.set_line(
            rect.x,
            rect.y.saturating_sub(1),
            &edge_row(usize::from(rect.width), tint, true),
            rect.width,
        );
    }
    if rect.y.saturating_add(rect.height) < area.bottom() {
        buf.set_line(
            rect.x,
            rect.y.saturating_add(rect.height),
            &edge_row(usize::from(rect.width), tint, false),
            rect.width,
        );
    }
}

/// Draws the stripe over `rect`'s rows in `colour`: its left column, or
/// its right one (`right`), on `tint` (`docs/tui.md`, "Look").
pub(crate) fn draw_stripe(buf: &mut Buffer, rect: Rect, colour: Role, tint: Role, right: bool) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let x = if right {
        rect.right().saturating_sub(1)
    } else {
        rect.x
    };
    let stripe = stripe_cell(colour, tint, right);
    let line = Line::from(stripe);
    for y in rect.top()..rect.bottom() {
        buf.set_line(x, y, &line, 1);
    }
}

#[cfg(test)]
#[path = "surface_tests.rs"]
mod tests;
