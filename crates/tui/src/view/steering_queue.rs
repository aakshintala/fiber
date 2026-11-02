//! The queued steering messages above the input box, drawn as a panel
//! card (`docs/tui.md`, "Steering", "The panel"): a `surface` slab with
//! ▄ and ▀ edges, an `attention` stripe down its left edge, the title
//! row, one entry per message with its `✕` right-aligned, and the key
//! hint.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::panel::fit_spans;
use crate::app::App;
use crate::ext::{Input, RawSpan, SlotDraw, SteeringQueue};
use crate::format;
use crate::mouse::{Target, TargetId};
use crate::surface;
use crate::theme::{Role, style};

/// The card's title (`docs/tui.md`, "Steering").
const TITLE: &str = "Steering";
/// The mark before a selected entry, with its gap (`docs/tui.md`,
/// "Steering").
const SELECTED_MARK: &str = "▸ ";
/// The mark before any other entry, with its gap (`docs/tui.md`,
/// "Steering").
const MARK: &str = "↳ ";
/// The mark's cells; both marks are one glyph with a gap after it.
const MARK_CELLS: usize = 2;
/// An entry's continuation indent: under its text, not its mark
/// (`docs/tui.md`, "Steering").
const CONTINUATION: &str = "  ";

/// One card text row: its line, the entry it selects (the title and the
/// hint select nothing), and whether its `✕` drops that entry.
struct CardRow {
    line: Line<'static>,
    entry: Option<usize>,
    cross: bool,
}

/// The steering queue's renderer input: its messages oldest first.
fn input(app: &App, flat: bool) -> SteeringQueue<'_> {
    SteeringQueue {
        messages: app.steering_messages(),
        flat,
    }
}

/// The card's text columns at `width`: two columns of padding on the
/// left (the stripe and its gap) and one on the right (`docs/tui.md`,
/// "The panel"). Without colour the box takes the sides, so the text
/// keeps one column clear of each (`docs/tui.md`, "Look").
fn text_width(width: u16) -> usize {
    let room = if surface::box_fit(width) { 4 } else { 3 };
    usize::from(width).saturating_sub(room)
}

/// The title row: "Steering" bold in `attention`, with "N queued ·
/// joins at the next step" dim, right-aligned (`docs/tui.md`,
/// "Steering"). The name is cut from the left when the two cannot fit,
/// as a panel card's title is (`docs/tui.md`, "The panel").
fn title_row(count: usize, text: usize) -> Line<'static> {
    let figure = format!("{count} queued · joins at the next step");
    let right_w = format::width(&figure);
    let room = text.saturating_sub(right_w.saturating_add(1));
    let shown = crate::text::cut_left(TITLE, room.max(1));
    let used = format::width(&shown);
    let pad = text.saturating_sub(used + right_w).max(1);
    Line::from(vec![
        Span::styled(shown, style(Role::Attention).add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(pad)),
        Span::styled(figure, style(Role::Muted)),
    ])
}

/// One hint key: the key bold and its label dim, after the dim " · "
/// separator past the first one, as a foot hint reads (`docs/tui.md`,
/// "Steering").
fn hint_key(spans: &mut Vec<Span<'static>>, key: &str, label: &str) {
    if !spans.is_empty() {
        spans.push(Span::styled(
            " · ",
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    spans.push(Span::styled(
        key.to_owned(),
        Style::new().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(" {label}"),
        Style::new().add_modifier(Modifier::DIM),
    ));
}

/// The key hint: each key bold and the rest dim (`docs/tui.md`,
/// "Steering"). A row too long for the card is cut with "…", as a
/// panel card's rows are (`docs/tui.md`, "The panel").
fn hint_row(text: usize) -> Line<'static> {
    let mut spans = Vec::new();
    hint_key(&mut spans, "⌥↑", "edit");
    hint_key(&mut spans, "⌥↓", "next");
    hint_key(&mut spans, "⌥X", "drop");
    spans.push(Span::styled(
        " · ",
        Style::new().add_modifier(Modifier::DIM),
    ));
    spans.push(Span::styled(
        "click a row to edit, ✕ to drop".to_owned(),
        Style::new().add_modifier(Modifier::DIM),
    ));
    Line::from(fit_spans(spans, text))
}

/// One entry's rows at `text` columns: the first with the mark in
/// `attention`, the text in the text colour (bold while selected) and
/// a dim `✕` right-aligned, the rest indented under the text
/// (`docs/tui.md`, "Steering"). The `✕` draws only while it, its gap,
/// the mark and one text cell fit; narrower, the entry keeps its text
/// with no `✕`.
fn entry_rows(
    selected: bool,
    droppable: bool,
    text: &str,
    at: usize,
    columns: usize,
) -> Vec<CardRow> {
    let cross = droppable && columns > MARK_CELLS + 1 + 1;
    // MARK_CELLS (2) with the cross's gap and cross (2): 4. A literal
    // keeps no `+` for a mutant to flip to `*` (`MARK_CELLS` is 2, so
    // both read 4 with no consumer-visible difference).
    let width = if cross {
        columns.saturating_sub(4)
    } else {
        columns.saturating_sub(MARK_CELLS)
    };
    let chunks = if width == 0 {
        vec![String::new()]
    } else {
        format::wrap(text, width)
    };
    let text_style = if selected {
        style(Role::Text).add_modifier(Modifier::BOLD)
    } else {
        style(Role::Text)
    };
    chunks
        .into_iter()
        .enumerate()
        .map(|(row, chunk)| {
            if row == 0 {
                let used = format::width(&chunk);
                let mut spans = vec![
                    Span::styled(
                        if selected { SELECTED_MARK } else { MARK }.to_owned(),
                        style(Role::Attention),
                    ),
                    Span::styled(chunk, text_style),
                ];
                if cross {
                    spans.push(Span::raw(
                        " ".repeat(columns.saturating_sub(MARK_CELLS + used + 1)),
                    ));
                    spans.push(Span::styled("✕", style(Role::Muted)));
                }
                CardRow {
                    line: Line::from(spans),
                    entry: Some(at),
                    cross,
                }
            } else {
                CardRow {
                    line: Line::from(vec![
                        Span::raw(CONTINUATION.to_owned()),
                        Span::styled(chunk, text_style),
                    ]),
                    entry: Some(at),
                    cross: false,
                }
            }
        })
        .collect()
}

/// The card's text rows at `text` columns, oldest entry first: the
/// title, one wrapped entry each, and the key hint (`docs/tui.md`,
/// "Steering").
fn card_lines(app: &App, text: usize) -> Vec<CardRow> {
    let rows = app.steering_rows();
    let mut out = vec![CardRow {
        line: title_row(rows.len(), text),
        entry: None,
        cross: false,
    }];
    for (at, row) in rows.iter().enumerate() {
        out.extend(entry_rows(row.selected, row.droppable, &row.text, at, text));
    }
    out.push(CardRow {
        line: hint_row(text),
        entry: None,
        cross: false,
    });
    out
}

/// The built-in queue as spans: the card's text rows. The paint path
/// below draws the same rows from the laid rows directly, so its
/// buffers never depend on this conversion.
fn builtin_spans(app: &App, width: u16) -> Vec<Vec<RawSpan>> {
    card_lines(app, text_width(width))
        .into_iter()
        .map(|row| crate::ext::spans::from_ratatui(&row.line.spans))
        .collect()
}

/// How many rows the steering card reserves: a replacement's line
/// count, or the built-in card's text rows with its ▄ and ▀ edges. A
/// zero-line replacement hides the whole card, and nothing draws or
/// runs on an empty queue.
pub(crate) fn count(app: &App, width: u16) -> usize {
    let bound = super::notice_rows::frame_bound(app);
    let folded = input(app, bound.flat);
    if folded.messages.is_empty() {
        return 0;
    }
    let builtin = &|_: &Input<'_>, width: u16| builtin_spans(app, width);
    match bound.slot_lines(
        SlotDraw::Count,
        "steering_queue",
        &Input::SteeringQueue(folded),
        width,
        builtin,
    ) {
        Some(lines) => lines.len(),
        // A miss draws the built-in card: its text rows with an edge
        // row above and below, where the room fits both
        // (`docs/tui.md`, "Look").
        None => {
            let rows = card_lines(app, text_width(width)).len();
            let room = usize::from(app.screen_size().1);
            surface::edged(rows, room)
        }
    }
}

/// Draws the steering card on the rows above `bottom`: the `surface`
/// slab with its edges and its `attention` stripe, the title, one
/// wrapped entry each, and the key hint. Each entry is a target over
/// its rows, and a drawn `✕` a target over its cell
/// (`docs/tui.md`, "Steering"). Moves `bottom` above them; nothing
/// while the queue is empty. A replacement draws its own lines with no
/// per-message target instead: keys still drop and pick rows.
pub(super) fn draw(
    app: &App,
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let bound = super::notice_rows::frame_bound(app);
    let folded = input(app, bound.flat);
    if folded.messages.is_empty() {
        return;
    }
    let builtin = &|_: &Input<'_>, width: u16| builtin_spans(app, width);
    let Some(fitted) = bound.slot_lines(
        SlotDraw::Paint,
        "steering_queue",
        &Input::SteeringQueue(folded),
        area.width,
        builtin,
    ) else {
        paint_builtin(app, area, bottom, buf, targets);
        return;
    };
    if fitted.is_empty() {
        // An empty replacement hides the whole card: no rows and no
        // targets.
        return;
    }
    paint_replacement(area, bottom, buf, &fitted);
}

/// Paints a replacement's lines bottom-up, newest row lowest, with no
/// target: keys still drop and pick rows.
fn paint_replacement(
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    fitted: &[crate::ext::spans::FittedLine],
) {
    let count = fitted
        .len()
        .min(usize::from(u16::MAX))
        .min(bottom.saturating_sub(area.y) as usize);
    if count == 0 {
        return;
    }
    *bottom = bottom.saturating_sub(u16::try_from(count).unwrap_or(u16::MAX));
    for (dy, line) in fitted.iter().take(count).enumerate() {
        let row = bottom.saturating_add(u16::try_from(dy).unwrap_or(u16::MAX));
        buf.set_line(area.x, row, &line.line, area.width);
    }
}

/// Paints the built-in card: its edges where they fit, its text rows on
/// the `surface` tint with the `attention` stripe, each entry a target
/// over its rows with a drawn `✕` a target over its cell. Every row the
/// card draws is reserved from `bottom` first, so its bottom edge never
/// covers the input box's top edge below it. Rows past the top shed
/// from the top; shed rows leave no target.
fn paint_builtin(
    app: &App,
    area: Rect,
    bottom: &mut u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let text = text_width(area.width);
    let lines = card_lines(app, text);
    let room = bottom.saturating_sub(area.y) as usize;
    // The card always holds its title and hint, so `lines` is never
    // empty when the queue is non-empty; only no room hides it.
    if room == 0 {
        return;
    }
    let edges = lines.len().saturating_add(2) <= room;
    let kept = lines
        .len()
        .min(room.saturating_sub(if edges { 2 } else { 0 }));
    if kept == 0 {
        return;
    }
    let skip = lines.len() - kept;
    let kept16 = u16::try_from(kept).unwrap_or(u16::MAX);
    // The bottom edge takes the row below the text.
    if edges {
        *bottom = bottom.saturating_sub(1);
    }
    *bottom = bottom.saturating_sub(kept16);
    let slab = Rect::new(area.x, *bottom, area.width, kept16);
    let card_edges = surface::Edges {
        top: edges,
        bottom: edges,
    };
    // Without colour the card is thin: │ on the slab's columns, dim,
    // with no tint and no stripe: the stripe's column is the slab's
    // left side (`docs/tui.md`, "Look").
    if surface::box_fit(area.width) {
        surface::draw_box(buf, area, slab, card_edges);
    } else {
        surface::draw_slab(
            buf,
            slab,
            Role::Surface,
            Some(surface::Stripe {
                colour: Role::Attention,
                right: false,
            }),
            card_edges,
        );
    }
    let x = area.x.saturating_add(2);
    let width = u16::try_from(text).unwrap_or(u16::MAX);
    // Each drawn entry's rows and its `✕` cell, when its first row drew.
    let mut drawn: Vec<(usize, u16, u16, Option<u16>)> = Vec::new();
    for (dy, row) in lines.iter().skip(skip).enumerate() {
        let row_y = slab.y.saturating_add(u16::try_from(dy).unwrap_or(u16::MAX));
        buf.set_line(x, row_y, &row.line, width);
        if let Some(at) = row.entry {
            match drawn.iter_mut().find(|span| span.0 == at) {
                Some(span) => span.2 = span.2.saturating_add(1),
                None => drawn.push((
                    at,
                    row_y,
                    1,
                    row.cross.then(|| x.saturating_add(width.saturating_sub(1))),
                )),
            }
        }
    }
    // Newest entry first, each with its `✕` after it, as the rows drew
    // them.
    for (at, first, height, cross) in drawn.into_iter().rev() {
        targets.push(Target {
            id: TargetId::Steering(at),
            rect: Rect::new(area.x, first, area.width, height),
        });
        if let Some(x) = cross {
            targets.push(Target {
                id: TargetId::DropSteering(at),
                rect: Rect::new(x, first, 1, 1),
            });
        }
    }
    // The top edge takes the row above the text; the slab's helpers
    // already drew it on the reserved row.
    if edges {
        *bottom = bottom.saturating_sub(1);
    }
}

#[cfg(test)]
#[path = "steering_queue_tests.rs"]
mod tests;
