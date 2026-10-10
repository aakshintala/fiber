//! The overlay frame every floating surface shares: the key map, the
//! quit question, the pickers and the completion panels (`docs/tui.md`,
//! "Look", "Overlays").
//!
//! The frame is a `surface` slab with a ▄ edge above and a ▀ edge below
//! and no border glyphs. Each row holding text opens with two blank
//! columns and ends with two blank columns, with no stripe, and one blank
//! row sits inside each edge. It is generic over its body, so each surface
//! builds its rows and the frame places, pads, bars and clips them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::format::{cut, width as cells};
use crate::mouse::{Target, TargetId};
use crate::theme::Role;

/// The narrowest overlay, in columns.
pub(crate) const MIN_W: u16 = 40;
/// The widest overlay, in columns.
pub(crate) const MAX_W: u16 = 150;
/// The blank columns opening and closing every text row.
const PAD: u16 = 2;

/// One content row. Column 0 is the first text column, past the frame's
/// two blank columns.
pub(crate) struct Row {
    /// The row's text, styled without a background; the frame tints it.
    pub(crate) spans: Vec<Span<'static>>,
    /// Right-aligned at the content's right end, such as a tag, a count
    /// or a cost; may be empty. It keeps priority over the left text when
    /// the row is cut.
    pub(crate) right: Vec<Span<'static>>,
    /// Click targets as `[start, end)` content columns.
    pub(crate) targets: Vec<(u16, u16, TargetId)>,
    /// Whether the focused-choice bar covers the row.
    pub(crate) barred: bool,
}

/// What the frame draws: a title, a body and a footer with one blank row
/// between each two present sections.
pub(crate) struct Overlay {
    /// The title text and its right end (a ✕ or a count), drawn dim.
    pub(crate) title: Option<(String, Option<Span<'static>>)>,
    /// The click target on the title's ✕, if any.
    pub(crate) close: Option<TargetId>,
    /// The body rows.
    pub(crate) body: Vec<Row>,
    /// The footer row.
    pub(crate) footer: Option<Row>,
    /// The content width the rows wrap at before the overlay shrinks to
    /// what they need: 71 for a question, 55 for the workspace picker, 96
    /// for the model picker and the completion panels.
    pub(crate) prefer: u16,
}

/// Where the frame sits in its area.
pub(crate) enum Place {
    /// Centred across and down the area.
    Centre,
    /// Above the input box: the slab's ▀ edge on the row above `bottom`.
    /// The completion panels take it (a later lane).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the completion panels take it in a later lane")
    )]
    Across {
        /// The input box's top-edge row.
        bottom: u16,
    },
    /// Under a view's top row: the slab's ▄ edge on `top`. The model
    /// picker takes it (a later lane).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the model picker takes it in a later lane")
    )]
    Under {
        /// The first row below the header.
        top: u16,
    },
    /// Docked at the bottom, full width.
    Dock,
}

/// The overlay's width for `content` columns of text in `area` columns:
/// the content plus the four blank columns, kept between 40 and 150 and
/// never wider than the screen. Below 40 columns the overlay is the area
/// wide.
pub(crate) fn width(content: u16, prefer: u16, area: u16) -> u16 {
    (content
        .min(prefer.min(area.saturating_sub(4)))
        .saturating_add(4))
    .clamp(MIN_W, MAX_W)
    .min(area)
}

/// The text columns inside an overlay `width` wide.
pub(crate) fn inner(width: u16) -> u16 {
    width.saturating_sub(4)
}

/// The overlay's full height in rows: two edges, two pad rows, the title
/// and its gap, the body, and the footer's gap and row. Later surfaces
/// budget their rows against it.
#[allow(dead_code, reason = "later surfaces budget their rows against it")]
pub(crate) fn height(overlay: &Overlay) -> u16 {
    let mut rows: u16 = 4;
    if overlay.title.is_some() {
        rows = rows.saturating_add(1);
    }
    if overlay.title.is_some() && (!overlay.body.is_empty() || overlay.footer.is_some()) {
        rows = rows.saturating_add(1);
    }
    rows = rows.saturating_add(u16::try_from(overlay.body.len()).unwrap_or(u16::MAX));
    if overlay.footer.is_some() {
        if !overlay.body.is_empty() {
            rows = rows.saturating_add(1);
        }
        rows = rows.saturating_add(1);
    }
    rows
}

/// Marks `row` as the focused choice. Later bodies bar their focused
/// row through it.
#[allow(dead_code, reason = "later bodies bar their focused row through it")]
pub(crate) fn bar(mut row: Row) -> Row {
    row.barred = true;
    row
}

/// Choice rows for `choices`, each a key and its description: the keys in
/// one column as wide as the widest key, two spaces, then the description
/// dim, with a description that wraps hanging under its own start. Each
/// row starts with two columns of gutter; the focused choice has "› " in
/// its gutter, reads bold throughout, and its rows are barred.
pub(crate) fn choices(choices: &[(&str, &str)], focused: usize, inner: u16) -> Vec<Row> {
    let keys = choices.iter().map(|(key, _)| cells(key)).max().unwrap_or(0);
    let mut rows = Vec::new();
    for (at, (key, description)) in choices.iter().enumerate() {
        let barred = at == focused;
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let desc = if barred {
            bold
        } else {
            Style::new().add_modifier(Modifier::DIM)
        };
        let hang = 2 + keys + 2;
        let room = usize::from(inner).saturating_sub(hang).max(1);
        let mut wrapped = crate::format::wrap(description, room);
        if wrapped.is_empty() {
            wrapped.push(String::new());
        }
        for (line, text) in wrapped.iter().enumerate() {
            // Only the choice's first row carries "› ": wrapped rows
            // hang under their own start on blank gutters.
            let gutter = if barred && line == 0 { "› " } else { "  " };
            let mut spans = vec![Span::styled(gutter.to_owned(), bold)];
            if line == 0 {
                spans.push(Span::styled(
                    format!("{key}{}", " ".repeat(keys.saturating_sub(cells(key)))),
                    bold,
                ));
                spans.push(Span::styled("  ".to_owned(), bold));
            } else {
                spans.push(Span::styled(" ".repeat(keys.saturating_add(2)), bold));
            }
            spans.push(Span::styled(text.clone(), desc));
            rows.push(Row {
                spans,
                right: Vec::new(),
                targets: Vec::new(),
                barred,
            });
        }
    }
    rows
}

/// A legend footer: each key bold and its label dim, joined by " · ".
pub(crate) fn legend(items: &[(&str, &str)]) -> Row {
    let mut spans = Vec::new();
    for (at, (key, label)) in items.iter().enumerate() {
        if at != 0 {
            spans.push(Span::styled(
                " · ".to_owned(),
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" ".to_owned()));
        spans.push(Span::styled(
            (*label).to_owned(),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// One dim footer row.
pub(crate) fn hint(text: &str) -> Row {
    Row {
        spans: vec![Span::styled(
            text.to_owned(),
            Style::new().add_modifier(Modifier::DIM),
        )],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The width of `spans` in columns.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| cells(&span.content)).sum()
}

/// `spans` cut to at most `max` columns, keeping each kept cell's style.
fn cut_spans(spans: &[Span<'static>], max: usize) -> Vec<Span<'static>> {
    let mut kept = Vec::new();
    let mut room = max;
    for span in spans {
        if room == 0 {
            break;
        }
        let text = cut(&span.content, room);
        room = room.saturating_sub(cells(&text));
        if !text.is_empty() {
            kept.push(Span::styled(text, span.style));
        }
    }
    kept
}

/// One drawn content line: its spans across the inner columns, its
/// targets, and whether the bar covers it.
struct Line {
    spans: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    targets: Vec<(u16, u16, TargetId)>,
    barred: bool,
}

/// The frame's stack of rows, top to bottom.
enum Slot {
    EdgeTop,
    PadTop,
    Title,
    TitleGap,
    Body(usize),
    FootGap,
    Footer,
    PadBottom,
    EdgeBottom,
}

/// Draws `overlay` in `area` of `buf` at `place`: the slab tint and both
/// edge rows, the padded title, body and footer, and no cell outside the
/// slab. Every position is relative to `area`, never to the buffer's
/// origin. Returns the slab rect clipped to `area`, which may be empty.
pub(crate) fn draw(
    buf: &mut Buffer,
    area: Rect,
    overlay: &Overlay,
    place: Place,
    targets: &mut Vec<Target>,
) -> Rect {
    if area.is_empty() {
        return Rect::new(area.x, area.y, 0, 0);
    }
    // The content's width is the widest row's, the left text and its
    // right end a column apart when both show.
    let row_width = |spans: &[Span<'static>], right: &[Span<'static>]| {
        let mut wide = spans_width(spans);
        if !right.is_empty() {
            wide = wide.saturating_add(spans_width(right).saturating_add(1));
        }
        u16::try_from(wide).unwrap_or(u16::MAX)
    };
    let mut content: u16 = 0;
    if let Some((title, right)) = overlay.title.as_ref() {
        let mut wide = u16::try_from(cells(title)).unwrap_or(u16::MAX);
        if let Some(right) = right {
            wide = wide.saturating_add(
                u16::try_from(cells(&right.content))
                    .unwrap_or(u16::MAX)
                    .saturating_add(1),
            );
        }
        content = content.max(wide);
    }
    for row in &overlay.body {
        content = content.max(row_width(&row.spans, &row.right));
    }
    if let Some(footer) = overlay.footer.as_ref() {
        content = content.max(row_width(&footer.spans, &footer.right));
    }
    let slab_w = match place {
        Place::Dock => area.width,
        Place::Centre | Place::Across { .. } | Place::Under { .. } => {
            width(content, overlay.prefer, area.width)
        }
    };
    let inner_w = inner(slab_w);
    // The full stack, then chrome gives way until it fits: body rows from
    // the bottom, the footer and its gap, the title's gap, the pad rows
    // with the bottom one first, and the top edge before the bottom one.
    // The title stays while the area holds a row, so heights 1 and 2 keep
    // it with no edges and the ▀ edge.
    let mut stack: Vec<Slot> = vec![Slot::EdgeTop, Slot::PadTop];
    if overlay.title.is_some() {
        stack.push(Slot::Title);
    }
    if overlay.title.is_some() && (!overlay.body.is_empty() || overlay.footer.is_some()) {
        stack.push(Slot::TitleGap);
    }
    for at in 0..overlay.body.len() {
        stack.push(Slot::Body(at));
    }
    if overlay.footer.is_some() {
        if !overlay.body.is_empty() {
            stack.push(Slot::FootGap);
        }
        stack.push(Slot::Footer);
    }
    stack.push(Slot::PadBottom);
    stack.push(Slot::EdgeBottom);
    let stack_height = |stack: &[Slot]| u16::try_from(stack.len()).unwrap_or(u16::MAX);
    // Body rows from the bottom first, then each chrome kind once
    // in a fixed order while the stack is too tall, so the give-way
    // ends after a fixed number of steps by construction.
    let keep = usize::from(overlay.title.is_none());
    let bodies = stack
        .iter()
        .filter(|slot| matches!(slot, Slot::Body(_)))
        .count();
    let drop = bodies.saturating_sub(keep).min(usize::from(
        stack_height(&stack).saturating_sub(area.height),
    ));
    let keep_bodies = bodies - drop;
    let mut seen = 0;
    stack.retain(|slot| {
        if matches!(slot, Slot::Body(_)) {
            seen += 1;
            seen <= keep_bodies
        } else {
            true
        }
    });
    if stack_height(&stack) > area.height {
        stack.retain(|slot| !matches!(slot, Slot::Footer | Slot::FootGap));
    }
    if stack_height(&stack) > area.height {
        stack.retain(|slot| !matches!(slot, Slot::TitleGap));
    }
    if stack_height(&stack) > area.height {
        stack.retain(|slot| !matches!(slot, Slot::PadBottom));
    }
    if stack_height(&stack) > area.height {
        stack.retain(|slot| !matches!(slot, Slot::PadTop));
    }
    if stack_height(&stack) > area.height {
        stack.retain(|slot| !matches!(slot, Slot::EdgeTop));
    }
    if stack_height(&stack) > area.height {
        stack.retain(|slot| !matches!(slot, Slot::EdgeBottom));
    }
    let slab_h = stack_height(&stack);
    if slab_h == 0 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let (slab_x, slab_y) = match place {
        Place::Centre => (
            area.x.saturating_add(area.width.saturating_sub(slab_w) / 2),
            area.y
                .saturating_add(area.height.saturating_sub(slab_h) / 2),
        ),
        Place::Dock => (area.x, area.bottom().saturating_sub(slab_h)),
        Place::Across { bottom } => (
            area.x.saturating_add(area.width.saturating_sub(slab_w) / 2),
            bottom.min(area.bottom()).saturating_sub(slab_h).max(area.y),
        ),
        Place::Under { top } => (
            area.x.saturating_add(area.width.saturating_sub(slab_w) / 2),
            top.max(area.y)
                .min(area.bottom().saturating_sub(slab_h.min(area.height))),
        ),
    };
    let slab = Rect::new(slab_x, slab_y, slab_w, slab_h);
    // The tint covers the slab's text rows, with only the edges the
    // height stack kept: a trimmed edge row would draw above or below
    // the slab, outside the area.
    let edged =
        |slot: Option<&Slot>| u16::from(matches!(slot, Some(Slot::EdgeTop | Slot::EdgeBottom)));
    let text_top = slab_y.saturating_add(edged(stack.first()));
    let text_bottom = slab.bottom().saturating_sub(edged(stack.last()));
    if text_bottom > text_top {
        let text = Rect::new(slab_x, text_top, slab_w, text_bottom - text_top);
        let edges = crate::surface::Edges {
            top: matches!(stack.first(), Some(Slot::EdgeTop)),
            bottom: matches!(stack.last(), Some(Slot::EdgeBottom)),
        };
        crate::surface::draw_slab(buf, text, Role::Surface, None, edges);
        // The tint keeps the symbols underneath: blank the interior so
        // the screen behind cannot bleed through the pad and gap rows.
        // The content rows repaint over it below.
        let blank = text.intersection(area).intersection(buf.area);
        for y in blank.top()..blank.bottom() {
            for x in blank.left()..blank.right() {
                buf[(x, y)].set_symbol(" ");
            }
        }
    }
    let mut y = slab_y;
    for slot in &stack {
        let row_y = y;
        y = y.saturating_add(1);
        let line = match slot {
            Slot::Title => overlay
                .title
                .as_ref()
                .map(|(title, right)| title_line(title, right.as_ref(), inner_w)),
            Slot::Body(at) => overlay.body.get(*at).map(|row| Line {
                spans: row.spans.clone(),
                right: row.right.clone(),
                targets: row.targets.clone(),
                barred: row.barred,
            }),
            Slot::Footer => overlay.footer.as_ref().map(|row| Line {
                spans: row.spans.clone(),
                right: row.right.clone(),
                targets: row.targets.clone(),
                barred: row.barred,
            }),
            Slot::EdgeTop
            | Slot::EdgeBottom
            | Slot::PadTop
            | Slot::PadBottom
            | Slot::TitleGap
            | Slot::FootGap => None,
        };
        let Some(line) = line else { continue };
        draw_line(buf, &slab, area, row_y, &line, inner_w);
        if let Some(close) = overlay.close
            && matches!(slot, Slot::Title)
        {
            let cross = Rect::new(slab.right().saturating_sub(3), row_y, 1, 1);
            let cross = cross.intersection(slab).intersection(area);
            if !cross.is_empty() {
                targets.push(Target {
                    id: close,
                    rect: cross,
                });
            }
        }
        for (start, end, id) in &line.targets {
            let from = (*start).min(inner_w);
            let to = (*end).min(inner_w);
            if from >= to {
                continue;
            }
            let rect = Rect::new(
                slab_x.saturating_add(PAD).saturating_add(from),
                row_y,
                to - from,
                1,
            );
            let rect = rect.intersection(slab).intersection(area);
            if !rect.is_empty() {
                targets.push(Target { id: *id, rect });
            }
        }
    }
    slab.intersection(area)
}

/// The title's content line: bold in `accent` with its right end dim.
fn title_line(title: &str, right: Option<&Span<'static>>, inner_w: u16) -> Line {
    Line {
        spans: vec![Span::styled(
            cut(title, usize::from(inner_w)),
            Style::new()
                .fg(Role::Accent.color())
                .add_modifier(Modifier::BOLD),
        )],
        right: right
            .map(|span| {
                vec![Span::styled(
                    span.content.clone(),
                    span.style.patch(Style::new().add_modifier(Modifier::DIM)),
                )]
            })
            .unwrap_or_default(),
        targets: Vec::new(),
        barred: false,
    }
}

/// Draws one content `line` on `row_y`: its text on the bar or the surface
/// tint, padded with two blank columns on each side.
fn draw_line(buf: &mut Buffer, slab: &Rect, area: Rect, row_y: u16, line: &Line, inner_w: u16) {
    let tint = if line.barred {
        Role::Accent
    } else {
        Role::Surface
    };
    // The frame tints the row, but a span naming its own background
    // keeps it: the key map's chosen tab draws dark on white. The bar
    // covers the whole row in `accent` with black text whatever the row
    // names.
    let paint = |mut span: Span<'static>| {
        if line.barred {
            span.style = span.style.patch(
                Style::new()
                    .bg(tint.color())
                    .fg(crate::look::BAR_TEXT)
                    .add_modifier(Modifier::BOLD),
            );
        } else if span.style.bg.is_none() {
            span.style = span.style.patch(Style::new().bg(tint.color()));
        }
        span
    };
    let room = usize::from(inner_w);
    let right_w = spans_width(&line.right);
    // Cutting text that already fits keeps it, so one branch cuts
    // both ends whether or not they overflow: at an exact fit it
    // keeps both, agreeing with the old fit branch.
    let right = cut_spans(&line.right, room.min(right_w));
    let left = cut_spans(&line.spans, room.saturating_sub(spans_width(&right)));
    let mut cells: Vec<Span<'static>> = vec![paint(Span::raw("  ".to_owned()))];
    for span in left {
        cells.push(paint(span));
    }
    let gap = room
        .saturating_sub(spans_width(&cells).saturating_sub(usize::from(PAD)))
        .saturating_sub(spans_width(&right));
    for _ in 0..gap {
        cells.push(paint(Span::raw(" ".to_owned())));
    }
    for span in right {
        cells.push(paint(span));
    }
    let trailing = usize::from(slab.width).saturating_sub(spans_width(&cells));
    for _ in 0..trailing {
        cells.push(paint(Span::raw(" ".to_owned())));
    }
    let line = ratatui::text::Line::from(cells);
    let at = slab.x.max(area.x);
    let keep = slab.right().min(area.right()).saturating_sub(at);
    buf.set_line(at, row_y, &line, keep);
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
