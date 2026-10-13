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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::look::{Depth, Var, depth};
use crate::theme::Role;

/// Whether stripes draw, read once at start.
static STRIPES: AtomicBool = AtomicBool::new(true);

/// Whether surfaces draw as colourless boxes, read once at start.
static BOXED: AtomicBool = AtomicBool::new(false);

/// Reads whether stripes draw and whether surfaces box into their
/// statics; `run` calls it once before the first frame.
pub(crate) fn init(var: Var<'_>) {
    STRIPES.store(stripes(var), Ordering::Relaxed);
    BOXED.store(depth(var) == Depth::NoColour, Ordering::Relaxed);
}

/// Whether surfaces draw as thin colourless boxes instead of tinted
/// slabs with half-block edges: `NO_COLOR` set and not empty, agreeing
/// with [`crate::look::depth`] (`docs/tui.md`, "Look").
pub(crate) fn boxed() -> bool {
    BOXED.load(Ordering::Relaxed)
}

/// Whether a surface `width` wide draws its box: colourless with room
/// for two sides and text. Narrower keeps the text, with no box
/// (`docs/tui.md`, "Look").
pub(crate) fn box_fit(width: u16) -> bool {
    boxed() && width >= 3
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

/// Which of a surface's edge rows draw: a card cut across pages draws no
/// edge at the cut (`docs/tui.md`, "Look", "History and paging").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Edges {
    pub(crate) top: bool,
    pub(crate) bottom: bool,
}

/// A state stripe on a slab's side: `▌` on the left, `▐` on the right
/// (`docs/tui.md`, "Look").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stripe {
    pub(crate) colour: Role,
    pub(crate) right: bool,
}

impl Edges {
    /// Both edge rows draw.
    pub(crate) const BOTH: Self = Self {
        top: true,
        bottom: true,
    };
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

/// A box's edge row `width` wide: ┌─┐ above (`top`) or └─┘ below,
/// dim, on the terminal's default colours (`docs/tui.md`, "Look").
/// The row keeps its place, so row counts never change.
pub(crate) fn box_edge_row(width: usize, top: bool) -> Line<'static> {
    let style = Style::new().add_modifier(Modifier::DIM);
    if width == 0 {
        return Line::default();
    }
    let (left, right) = if top { ('┌', '┐') } else { ('└', '┘') };
    if width == 1 {
        return Line::from(Span::styled(left.to_string(), style));
    }
    Line::from(Span::styled(
        format!("{left}{}{right}", "─".repeat(width.saturating_sub(2))),
        style,
    ))
}

/// An edge row for a surface `width` wide in `tint`: the box's edge
/// where a box fits, else half blocks in the tint (`docs/tui.md`,
/// "Look"). The prompt keeps [`edge_row`]'s half blocks, which the
/// paint pass blanks, so its rows stay edge-free.
pub(crate) fn card_edge_row(width: usize, tint: Role, top: bool) -> Line<'static> {
    if boxed() && width >= 3 {
        box_edge_row(width, top)
    } else {
        edge_row(width, tint, top)
    }
}

/// Draws a box's sides over `rect`'s rows: │ dim on its first and last
/// columns, where each sits inside `buf` (`docs/tui.md`, "Look").
/// Adds no corners and writes no text: the edge rows hold the corners,
/// and the text keeps its cells.
pub(crate) fn draw_sides(buf: &mut Buffer, rect: Rect) {
    if rect.height == 0 {
        return;
    }
    let area = buf.area;
    let left = rect.x;
    let right = rect.right().saturating_sub(1);
    if left >= right {
        return;
    }
    let side = Line::from(Span::styled("│", Style::new().add_modifier(Modifier::DIM)));
    for y in rect.top().max(area.top())..rect.bottom().min(area.bottom()) {
        if left >= area.left() && left < area.right() {
            buf.set_line(left, y, &side, 1);
        }
        if right >= area.left() && right < area.right() {
            buf.set_line(right, y, &side, 1);
        }
    }
}

/// Draws a thin box for `rect` in `area` of `buf`: ┌─┐ on the row above
/// it (`edges.top`) and └─┘ on the row below it (`edges.bottom`), where
/// each sits inside both `area` and `buf` across the columns where the
/// rect and the buffer overlap, and │ on the rect's own columns
/// (`docs/tui.md`, "Look"). The rect keeps its rows: the edges take
/// the rows the slab's edge rows already reserved. A side whose column
/// sits outside the buffer draws nothing, so clipped text is never
/// overwritten.
pub(crate) fn draw_box(buf: &mut Buffer, area: Rect, rect: Rect, edges: Edges) {
    if edges.top && rect.y > area.y {
        box_edge(buf, area, rect, true);
    }
    // The rect's columns are the box's: the layout reserved them, so a
    // visible side never covers text.
    draw_sides(buf, rect);
    if edges.bottom {
        let y = rect.bottom();
        if y >= area.y && y < area.bottom() {
            box_edge(buf, area, rect, false);
        }
    }
}

/// Draws one box edge for `rect` in `area` of `buf`: above it (`top`)
/// or below it, where that row sits inside both, across the columns
/// where the rect and the buffer overlap (`docs/tui.md`, "Look").
fn box_edge(buf: &mut Buffer, area: Rect, rect: Rect, top: bool) {
    let space = buf.area.intersection(area);
    let y = if top {
        rect.y.saturating_sub(1)
    } else {
        rect.bottom()
    };
    if y < space.top() || y >= space.bottom() {
        return;
    }
    let left = rect.x.max(space.left());
    let right = rect.right().min(space.right());
    if left >= right {
        return;
    }
    let width = right - left;
    buf.set_line(left, y, &box_edge_row(usize::from(width), top), width);
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

/// Draws a tinted slab: `tint` over `rect`, an optional stripe on its
/// side, and the edge rows asked for (`docs/tui.md`, "Look"). The tint
/// covers the rect where it sits inside the buffer and leaves symbols
/// and foregrounds; each edge row sets only its foreground, so its
/// background stays the colour around the slab. The stripe draws where
/// the text insets past it, clipped to the buffer at the slab's own
/// column; the edges clip on their own rows and columns, whether or not
/// any of the slab's rows shows.
pub(crate) fn draw_slab(
    buf: &mut Buffer,
    rect: Rect,
    tint: Role,
    stripe: Option<Stripe>,
    edges: Edges,
) {
    if rect.width == 0 {
        return;
    }
    buf.set_style(rect, Style::new().bg(tint.color()));
    if let Some(stripe) = stripe
        && inset(rect.width) < rect.width
    {
        draw_stripe(buf, rect, stripe.colour, tint, stripe.right);
    }
    if edges.top {
        draw_edge(buf, rect, tint, true);
    }
    if edges.bottom {
        draw_edge(buf, rect, tint, false);
    }
}

/// Draws one edge row for `rect` in `tint`: above it (`top`) or below
/// it, where that row sits inside `buf`, across the columns where the
/// rect and the buffer overlap (`docs/tui.md`, "Look").
fn draw_edge(buf: &mut Buffer, rect: Rect, tint: Role, top: bool) {
    let area = buf.area;
    let y = if top {
        if rect.y <= area.y {
            return;
        }
        rect.y.saturating_sub(1)
    } else {
        rect.bottom()
    };
    if y < area.y || y >= area.bottom() {
        return;
    }
    let left = rect.x.max(area.left());
    let right = rect.right().min(area.right());
    if left >= right {
        return;
    }
    let width = right - left;
    buf.set_line(left, y, &edge_row(usize::from(width), tint, top), width);
}

/// Draws the edge rows above and below `rect` in `tint`, where they sit
/// inside `buf` (`docs/tui.md`, "Look").
pub(crate) fn draw_edges(buf: &mut Buffer, rect: Rect, tint: Role) {
    draw_edge(buf, rect, tint, true);
    draw_edge(buf, rect, tint, false);
}

/// Draws the stripe over `rect`'s rows in `colour`: its left column, or
/// its right one (`right`), on `tint`, where that column and each row
/// sit inside `buf` (`docs/tui.md`, "Look").
pub(crate) fn draw_stripe(buf: &mut Buffer, rect: Rect, colour: Role, tint: Role, right: bool) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let area = buf.area;
    let x = if right {
        rect.right().saturating_sub(1)
    } else {
        rect.x
    };
    if x < area.left() || x >= area.right() {
        return;
    }
    let stripe = stripe_cell(colour, tint, right);
    let line = Line::from(stripe);
    for y in rect.top().max(area.top())..rect.bottom().min(area.bottom()) {
        buf.set_line(x, y, &line, 1);
    }
}

#[cfg(test)]
#[path = "surface_tests.rs"]
mod tests;
