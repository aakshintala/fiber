//! Conversation rows and their targets, including the normal conversation and item transcript
//! surfaces (`docs/tui.md`, "Turns", "Look", "Tool groups and the ledger", "Selection and copy", "Links").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use super::{marks, paragraph, scroll_bar, working_line};
use crate::app::App;
use crate::mouse::{Target, TargetId};
use crate::text::to_u16;
use crate::theme::{Role, style};

/// The overlay shown while scrolled up once new output arrives.
const NEW_BELOW: &str = " ↓ New messages below · End ";

/// Shown in the conversation's top-right corner after a copy, below
/// the search box when it is open.
const COPIED: &str = " Copied ";

/// Whether the line draws on a card tint: a turn, surface or handoff
/// card, whose sides draw as a thin box without colour, and which marks
/// the pinned row's tint (`docs/tui.md`, "Look").
fn card_tint(line: &Line<'_>) -> bool {
    line.style.bg == Some(Role::Turn.color())
        || line.style.bg == Some(Role::Surface.color())
        || line.style.bg == Some(Role::Handoff.color())
}

/// Draws one conversation row into its rect: the line with its own
/// background filling the column, and, without colour on a card tint,
/// the thin-box sides (`docs/tui.md`, "Look").
fn draw_row(line: Line<'_>, rect: Rect, buf: &mut Buffer) {
    let turn_tint = card_tint(&line);
    let fill = Style {
        bg: line.style.bg,
        ..Style::default()
    };
    paragraph(line).style(fill).render(rect, buf);
    if crate::surface::box_fit(rect.width) && turn_tint {
        crate::surface::draw_sides(buf, rect);
    }
}

/// The row of the " Copied " confirmation in `area`, when drawn: on the
/// row under the bar when the bar draws, else on the top row
/// (`docs/tui.md`, "Selection and copy").
fn copied_row(app: &App, area: Rect) -> Option<u16> {
    if !app.copied() {
        return None;
    }
    let offset = if app.find_bar().is_some() && !app.results_open() {
        // The bar draws in the conversation's area with the scroll
        // bar's column; this area drops that column on the main path.
        let room = Rect::new(area.x, area.y, area.width.saturating_add(1), area.height);
        match marks::bar_rect(room).map(|rect| rect.height) {
            Some(3) => 5,
            Some(_) => 1,
            None => 0,
        }
    } else {
        0
    };
    Some(area.y.saturating_add(offset))
}

/// Blanks the hint's reserved row with its card's tint, so no previous
/// frame's cells show past the hint (`docs/tui.md`, "Links").
fn blank_row(buf: &mut Buffer, area: Rect, y: u16, style: Style) {
    let width = usize::from(area.width);
    buf.set_stringn(area.x, y, " ".repeat(width), width, style);
}

/// Draws the conversation's visible rows, bottom-aligned while it is
/// shorter than its area. Only the pages that draw a visible row are read;
/// a page not loaded draws blank rows. Pushes a target over each shown line
/// that opens something, and over "↓ New messages below" when drawn.
/// Returns the pinned header for the caller to draw after the marks pass,
/// so covered-row selection, find and link looks never land on the pin
/// (`docs/tui.md`, "Pinned call"), with the painted animated items' keys
/// for the frame's single mark: the caller marks them once, so the
/// schedule advances only painted items.
// A line target covers one row: the pad splits longer lines into one-row
// pieces keeping the target on the first, so the clipped height equals the
// cell height whenever the guard keeps the target, and a fully clipped
// target is already skipped.
#[cfg_attr(false, mutants::skip)]
fn conversation_rows(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> (Option<crate::window::Pin>, Vec<crate::ext::anim::AnimKey>) {
    let height = usize::from(area.height);
    // The focused link's hint reserves the row under its line, laid out
    // with the targets: rows on and below it draw one lower, so hint
    // text and click targets agree (`docs/tui.md`, "Links").
    let placed = app.hint_layout(area);
    let top = placed.top;
    let gap = placed.gap;
    let mut y = placed.y0;
    let end = top.saturating_add(height);
    let crate::window::Shown {
        first,
        lines,
        turns,
        spins,
        anims,
        pin,
    } = app.shown(top, height);
    // Where this frame's turn targets splice in: after the targets
    // drawn before the conversation, before its line targets.
    let base = targets.len();
    let mut start = first;
    let overlay = app.has_new() && area.height > 0;
    let last = area.bottom().saturating_sub(u16::from(overlay));
    // The card tint above the hint's row: the row the hint reserves
    // ends its card, so the blank row keeps it.
    let mut under = Style::default();
    // Each line's cells on screen: its y and how many of its rows show,
    // laid out with the gap in the loop below. The turns loop after it
    // reads them.
    let mut layout: Vec<(u16, u16)> = Vec::with_capacity(lines.len());
    // The pinned header covers the top row's own line: that line keeps
    // no hit, so the pin is the click and focus target on row 0 and the
    // covered row steps or reveals out from under it (`docs/tui.md`,
    // "Pinned call").
    let mut pin = if !app.item_open() && app.top().is_some() && area.height > 0 {
        pin
    } else {
        None
    };
    // On the top row the " Copied " confirmation wins over the pin,
    // which yields (`docs/tui.md`, "Pinned call").
    if copied_row(app, area) == Some(area.y) {
        pin = None;
    }
    let pinned = pin.is_some();
    // A line wholly above `top` or below `end` shows no rows.
    for (at, (line, rows, open)) in lines.into_iter().enumerate() {
        let next = start.saturating_add(rows);
        let skip = top.saturating_sub(start);
        let count = next.min(end).saturating_sub(start.max(top));
        if gap.is_some_and(|gap| gap == y) {
            blank_row(buf, area, y, under);
            y = y.saturating_add(1);
        }
        if y >= area.bottom() {
            break;
        }
        layout.push((y, to_u16(count)));
        let room = area.bottom().saturating_sub(y);
        let take = count.min(usize::from(room));
        let rect = Rect::new(area.x, y, area.width, to_u16(take));
        // A short row and the wrapped rows of a long one fill the column
        // with the row's own background, so a card's surface tint reaches
        // the column's edge (`docs/tui.md`, "Look").
        let turn_tint = card_tint(&line);
        let fill = Style {
            bg: line.style.bg,
            ..Style::default()
        };
        paragraph(line)
            .style(fill)
            .scroll((to_u16(skip), 0))
            .render(rect, buf);
        under = fill;
        // Without colour a turn's card is a thin box: its edge rows
        // already draw ┌─┐ and └─┘, and its body rows take │ on the
        // card's columns (`docs/tui.md`, "Look"). The markers still
        // name the surface until the paint pass, so the pads the card
        // keeps are decorated without moving text; the prompt bubble
        // stays edge-free, and a cut card draws no corner it withheld.
        if crate::surface::box_fit(area.width) && turn_tint {
            crate::surface::draw_sides(buf, rect);
        }
        // A marked line spins its cell on the working line's tick; the
        // mark is the builders' promise that the line moves.
        if let Some(col) = spins
            .iter()
            .find_map(|(marked, col)| (*marked == at).then_some(*col))
        {
            working_line::spin(
                app,
                buf,
                area,
                working_line::Drawn {
                    y,
                    col,
                    first_row_shown: skip == 0,
                    rows,
                    count,
                },
            );
        }
        if let Some(open) = open {
            let height = rect.height.min(last.saturating_sub(rect.y));
            // A code block's copy target is its `copy` cells, not the line.
            let cells = app.copy_cells(open).map_or(rect, |cols| Rect {
                x: area.x.saturating_add(cols.start),
                width: cols.end.saturating_sub(cols.start),
                ..rect
            });
            if height > 0 && !(pinned && y == area.y) {
                targets.push(Target {
                    id: TargetId::Line(open),
                    rect: Rect { height, ..cells },
                });
            }
        }
        y = y.saturating_add(to_u16(count));
        start = next;
    }
    // The animated items painted this frame: rows with take greater
    // than 0 expose their tags for the frame's single mark, so the
    // schedule advances only painted items.
    let mut painted = Vec::new();
    for (at, (_, count)) in layout.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        painted.extend(
            anims
                .iter()
                .filter(|(marked, _)| *marked == at)
                .map(|(_, key)| key.clone()),
        );
    }
    // Turns are focus stops over the rows their page cards draw; they
    // are not click targets, so the line targets drawn before them win
    // clicks: their targets splice in front, in draw order.
    let mut turned: Vec<Target> = Vec::new();
    for (at, range) in turns {
        let mut rect: Option<Rect> = None;
        for (line, (line_y, count)) in layout.iter().enumerate() {
            if range.contains(&line) {
                rect = Some(match rect {
                    Some(before) => Rect {
                        height: before.height.saturating_add(*count),
                        ..before
                    },
                    None => Rect::new(area.x, *line_y, area.width, *count),
                });
            }
        }
        if let Some(rect) = rect {
            let height = rect.height.min(last.saturating_sub(rect.y));
            if height > 0 && !(pinned && y == area.y) {
                turned.push(Target {
                    id: TargetId::Turn(at),
                    rect: Rect { height, ..rect },
                });
            }
        }
    }
    targets.splice(base..base, turned);
    // The hint's row past the last line, when the scroll freed it
    // there: blank it with the card tint, as in the loop.
    if gap.is_some_and(|gap| gap == y) {
        blank_row(buf, area, y, under);
    }
    if overlay {
        // The pill over the conversation's bottom row: its ends and its
        // label reset the cells they cover, keeping the background under
        // them, so no modifier from the row below survives
        // (`docs/tui.md`, "Turns"). The row is never blanked.
        let row = area.bottom().saturating_sub(1);
        let mut cells: Vec<(String, bool)> = Vec::with_capacity(30);
        cells.push(("▐".to_owned(), false));
        cells.extend(
            Line::raw(NEW_BELOW)
                .styled_graphemes(Style::default())
                .map(|grapheme| (grapheme.symbol.to_owned(), true)),
        );
        cells.push(("▌".to_owned(), false));
        let start = area
            .x
            .saturating_add(area.width.saturating_sub(to_u16(cells.len())) / 2);
        let end = start.saturating_add(to_u16(cells.len())).min(area.right());
        for (x, (symbol, is_label)) in (start..end).zip(cells) {
            let cell = &mut buf[(x, row)];
            let bg = cell.bg;
            cell.reset();
            cell.bg = bg;
            cell.set_symbol(&symbol);
            if is_label {
                cell.set_fg(Role::Attention.color());
                cell.set_bg(Role::Rule.color());
            } else {
                cell.set_fg(Role::Rule.color());
            }
        }
        targets.push(Target {
            id: TargetId::NewBelow,
            rect: Rect::new(start, row, end.saturating_sub(start), 1),
        });
    }
    if let Some(y) = copied_row(app, area) {
        // " Copied " in `info` on the `surface` tint
        // (`docs/tui.md`, "Selection and copy").
        // The row under a five-row bar in a five-row area is the next
        // region's: omit the confirmation rather than cover it, and clip
        // its width to the area (`docs/tui.md`, "Selection and copy").
        if y < area.bottom() {
            let width = to_u16(crate::format::width(COPIED)).min(area.width);
            let x = area.x.saturating_add(area.width.saturating_sub(width));
            buf.set_style(
                Rect::new(x, y, width, 1),
                Style::new().bg(Role::Surface.color()),
            );
            buf.set_stringn(x, y, COPIED, usize::from(width), style(Role::Info));
        }
    }
    (pin, painted)
}

/// Draws the conversation or item transcript through their shared row model.
pub(super) fn draw(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Vec<crate::ext::anim::AnimKey> {
    if app.item_open() {
        let (_, painted) = conversation_rows(app, area, buf, targets);
        marks::draw(app, area, buf, targets, app.hint_layout(area).gap);
        return painted;
    }

    let inset = crate::layout::past_gutter(area, app.chrome().gutter());
    let (rows, bar) = scroll_bar::split(inset);
    let (pin, painted) = conversation_rows(app, rows, buf, targets);
    scroll_bar::draw(app, bar, buf);
    marks::draw(app, inset, buf, targets, app.hint_layout(inset).gap);
    if let Some(pin) = pin {
        let rect = Rect::new(rows.x, rows.y, rows.width, 1);
        draw_row(pin.line, rect, buf);
        targets.push(Target {
            id: TargetId::Line(pin.target),
            rect,
        });
    }
    painted
}

#[cfg(test)]
pub(super) fn test_conversation_rows(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> (Option<crate::window::Pin>, Vec<crate::ext::anim::AnimKey>) {
    conversation_rows(app, area, buf, targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_card_tint_draws_the_thin_box_sides() {
        // The pin draws through `draw_row`, and the row loop shares its tint
        // check: a turn, surface or handoff background draws the thin-box
        // sides without colour, so flipping any arm of the check drops a side
        // here (`docs/tui.md`, "Look").
        crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
        for (name, role) in [
            ("turn", Role::Turn),
            ("surface", Role::Surface),
            ("handoff", Role::Handoff),
        ] {
            let line = ratatui::text::Line::styled("tinted", Style::new().bg(role.color()));
            let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
            draw_row(line, Rect::new(0, 0, 20, 1), &mut buf);
            assert_eq!(buf[(0, 0)].symbol(), "│", "{name} keeps its left side");
            assert_eq!(buf[(19, 0)].symbol(), "│", "{name} keeps its right side");
        }
        crate::surface::init(&|_| None);
    }
}
