//! What the view paints over the conversation's rows once they are drawn
//! (`docs/tui.md`, "Selection and copy", "Links", "Search"): the search
//! bar, the selection's highlight, and the links' targets with their
//! underline.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::{App, FindBar};
use crate::look::{BAR_TEXT, TAB_BAR};
use crate::mouse::{Target, TargetId};
use crate::theme::Role;

/// The selection's highlight: the `selection` tint, with no modifier,
/// so the text keeps its own colour (`docs/tui.md`, "Search").
const SELECTION: Style = Style::new().bg(Role::Selection.color());

/// A match's highlight: the `match` tint with white text, with no
/// modifier (`docs/tui.md`, "Search"). White is the look's shared
/// white, so no new colour is named outside the theme and the look.
const MATCH: Style = Style::new().bg(Role::Match.color()).fg(TAB_BAR);

/// The current match's highlight: the brighter `match_current` tint
/// with black text, with no modifier (`docs/tui.md`, "Search").
const CURRENT: Style = Style::new().bg(Role::MatchCurrent.color()).fg(BAR_TEXT);

/// The selection's mark with no colour: reversed, so it shows
/// (`docs/tui.md`, "Look").
const SELECTION_NO_COLOR: Style = Style::new()
    .bg(Role::Selection.color())
    .add_modifier(Modifier::REVERSED);

/// A match's mark with no colour: underlined, so it shows
/// (`docs/tui.md`, "Look").
const MATCH_NO_COLOR: Style = Style::new()
    .bg(Role::Match.color())
    .add_modifier(Modifier::UNDERLINED);

/// The current match's mark with no colour: reversed and bold, so it
/// shows and apart from the selection (`docs/tui.md`, "Look").
const CURRENT_NO_COLOR: Style = Style::new()
    .bg(Role::MatchCurrent.color())
    .fg(Role::Text.color())
    .add_modifier(Modifier::REVERSED)
    .add_modifier(Modifier::BOLD);

/// The selection's highlight style, shared by every surface that paints
/// one (`docs/tui.md`, "Selection and copy").
pub(crate) fn selection_style() -> Style {
    if crate::surface::boxed() {
        SELECTION_NO_COLOR
    } else {
        SELECTION
    }
}

/// Paints `surface`'s selection over `area`: each cell with the
/// selection tint (`docs/tui.md`, "Selection and copy").
pub(crate) fn paint_selection(
    app: &App,
    buf: &mut Buffer,
    surface: crate::app::SurfaceId,
    area: Rect,
) {
    for rect in app.selection_cells(surface, area) {
        buf.set_style(rect.intersection(area), selection_style());
    }
}

/// The matched and current styles shared by conversation and results.
pub(super) fn hit_styles() -> (Style, Style) {
    if crate::surface::boxed() {
        (MATCH_NO_COLOR, CURRENT_NO_COLOR)
    } else {
        (MATCH, CURRENT)
    }
}

/// A link's look: the accent colour, underlined (`docs/tui.md`,
/// "Links"). Markdown links already draw it in the reply; bare URLs
/// gain it here.
const LINK: Style = Style::new()
    .fg(Role::Accent.color())
    .add_modifier(Modifier::UNDERLINED);

/// A focused link's look: the link look on the `surface_raised` tint
/// (`docs/tui.md`, "Links"). The view paints no generic focus style
/// over links; this is the focused look.
const FOCUSED: Style = Style::new()
    .fg(Role::Accent.color())
    .bg(Role::SurfaceRaised.color())
    .add_modifier(Modifier::UNDERLINED);

/// The hint under a focused link (`docs/tui.md`, "Links").
pub(crate) const HINT: &str = "Enter opens · y copies";

/// Paints the search bar on the conversation's top-right row, then the
/// selection's cells, then pushes one click target per row each visible
/// link covers and underlines its cells. With no bar, no selection and no
/// link on screen this costs one check each (`docs/tui.md`,
/// "Performance"). While the results view is open only the bar draws:
/// it covers the conversation (`docs/tui.md`, "Search").
///
/// `gap` is the hint row the conversation reserved under the focused
/// link, if any: the links shift with it, so hint text and click targets
/// agree. Views whose content does not move pass `None`, and their links
/// draw as before with no hint (`docs/tui.md`, "Links").
pub(super) fn draw(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    gap: Option<u16>,
) {
    // While the results view is open the bar does not draw: it would
    // cover the view's top rows. Its state stays, and it draws again
    // when the view closes (`docs/tui.md`, "Search").
    if app.results_open() {
        return;
    }
    if let Some(bar) = app.find_bar() {
        draw_bar(&bar, area, buf, targets);
    }
    // With no colour the marks keep their modifiers and the paint
    // pass strips the tints; in colour the tints carry the marks with
    // no modifier (`docs/tui.md`, "Look", "Search").
    let (matched, current_mark) = hit_styles();
    for (rect, current) in app.find_marks(area) {
        let style = if current { current_mark } else { matched };
        buf.set_style(rect.intersection(area), style);
    }
    paint_selection(app, buf, crate::app::SurfaceId::Conversation, area);
    let focused = app.focused();
    let links = if gap.is_some() {
        app.visible_links(area)
    } else {
        app.plain_links(area)
    };
    for link in links {
        // The focused link sits on the `surface_raised` tint; every
        // other link keeps the card it draws on (`docs/tui.md`,
        // "Links").
        let style = if focused.is_some_and(|id| id == link.id) {
            FOCUSED
        } else {
            LINK
        };
        for rect in link.rects {
            let rect = rect.intersection(area);
            if rect.is_empty() {
                continue;
            }
            buf.set_style(rect, style);
            targets.push(Target { id: link.id, rect });
        }
    }
    hint(app, area, buf, gap);
}

/// Paints the focused link's hint on its reserved row, dim. The row is
/// the conversation's own gap under the link's line, so no other target
/// sits on it; the next frame without focus paints its rows with no gap
/// again, so the hint goes when focus leaves (`docs/tui.md`, "Links").
/// With no reserved row only the focused look paints.
fn hint(app: &App, area: Rect, buf: &mut Buffer, gap: Option<u16>) {
    let Some(id) = app.focused() else {
        return;
    };
    let Some(gap) = gap else {
        return;
    };
    let Some(link) = app
        .visible_links(area)
        .into_iter()
        .find(|link| link.id == id)
    else {
        return;
    };
    let x = link.rects.first().map_or(area.x, |rect| rect.x);
    let room = usize::from(area.right().saturating_sub(x));
    // One call: `set_stringn` with a dim-only style keeps each cell's
    // colours and adds dim, exactly what the per-cell loop did, so the
    // card under the hint shows through. The hint holds no wide
    // character, so its graphemes match the cut's chars one by one.
    buf.set_stringn(
        x,
        gap,
        crate::format::cut(HINT, room),
        room,
        Style::new().add_modifier(Modifier::DIM),
    );
}

/// The bar's cells: at most this wide (`docs/tui.md`, "Search").
const BAR_WIDTH: usize = 40;

/// The bar's content row, before its trailing blank: ` ⌕ `, the query,
/// the block, one gap, then the count right-aligned
/// (`docs/tui.md`, "Search").
const PREFIX: &str = " ⌕ ";

/// The content columns around the query and the count: the ` ⌕ `, the
/// block, the gap and the trailing blank.
const OVERHEAD: usize = 6;

/// The content row's text columns in `rect`: with no colour the box's
/// sides take one column each (`docs/tui.md`, "Look").
fn content_row(rect: Rect) -> (u16, u16, usize) {
    let y = rect.y.saturating_add(u16::from(rect.height == 3));
    if crate::surface::boxed() {
        (
            rect.x.saturating_add(1),
            y,
            usize::from(rect.width.saturating_sub(2)),
        )
    } else {
        (rect.x, y, usize::from(rect.width))
    }
}

/// The bar's count text: `type to search` on an empty query, else the
/// bar's count with its scan and completeness marks (`docs/tui.md`,
/// "Search").
fn bar_count(bar: &FindBar) -> &str {
    if bar.query.is_empty() {
        "type to search"
    } else {
        bar.count.as_str()
    }
}

/// The columns `text` draws: each grapheme's own cells summed, as
/// `Buffer::set_stringn` draws them. A whole string fuses some pairs
/// (`لا` measures 1) while the buffer draws a cell per grapheme, so a
/// whole-string measure puts the cursor before the block
/// (`docs/tui.md`, "Search").
fn drawn_width(text: &str) -> usize {
    Span::raw(text)
        .styled_graphemes(Style::default())
        .filter(|grapheme| !grapheme.symbol.contains(char::is_control))
        .map(|grapheme| crate::format::width(grapheme.symbol))
        .sum()
}

/// The query's end kept in `room` columns: an overlong query cuts at
/// the left of its kept end, so the block after it always shows. The
/// cut falls on a grapheme boundary, as the draw does
/// (`docs/tui.md`, "Search").
fn query_end(query: &str, room: usize) -> String {
    let span = Span::raw(query);
    let pieces: Vec<(&str, usize)> = span
        .styled_graphemes(Style::default())
        .map(|grapheme| (grapheme.symbol, crate::format::width(grapheme.symbol)))
        .collect();
    let mut taken = 0usize;
    let mut kept: Vec<&str> = Vec::new();
    for (symbol, cell) in pieces.iter().rev() {
        if taken.saturating_add(*cell) > room {
            break;
        }
        taken = taken.saturating_add(*cell);
        kept.push(*symbol);
    }
    kept.into_iter().rev().collect()
}

/// The bar's query and count in `room` content columns: the query's
/// kept end, then the count's kept start in what the ` ⌕ `, the query,
/// the block and the gap leave (`docs/tui.md`, "Search").
fn bar_pieces(bar: &FindBar, room: usize) -> (String, String) {
    let query = query_end(&bar.query, room.saturating_sub(OVERHEAD));
    let count_room = room
        .saturating_sub(OVERHEAD)
        .saturating_sub(drawn_width(&query));
    let count = crate::format::cut(bar_count(bar), count_room);
    (query, count)
}

/// The bar's content rows in `area`: the slab's three rows (blank, text,
/// blank) with room for both edge rows, else the bare content row;
/// `None` on an empty area or under the width floor (`docs/tui.md`,
/// "Search", "Look"). The bar is `BAR_WIDTH` columns wide, or
/// `area.width - 1` when narrower, ending one column left of the scroll
/// bar's column, whose column `area` keeps. With no colour the box's
/// two sides take two columns, so the floor is one higher. The slab's
/// content rows and the bare row share this one helper, so `draw_bar`,
/// `bar_cursor` and the "Copied" row cannot disagree.
pub(super) fn bar_rect(area: Rect) -> Option<Rect> {
    if area.is_empty() {
        return None;
    }
    let floor = if crate::surface::boxed() { 7 } else { 6 };
    if area.width < floor {
        return None;
    }
    let width =
        u16::try_from(BAR_WIDTH.min(usize::from(area.width).saturating_sub(1))).unwrap_or(u16::MAX);
    let x = area.right().saturating_sub(1).saturating_sub(width);
    if area.height >= 5 {
        Some(Rect::new(x, area.y.saturating_add(1), width, 3))
    } else {
        Some(Rect::new(x, area.y, width, 1))
    }
}

/// Paints the bar right-aligned in `area` as a `surface` slab with
/// half-block edges, pushing a target over its match count: a click
/// opens the results (`docs/tui.md`, "Search"). With fewer than five
/// rows the content row draws alone, with no edges. A count cut to
/// nothing is no target. The slab's blank rows and edges hold no
/// target, so a click there falls through to the target under it.
fn draw_bar(bar: &FindBar, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let Some(rect) = bar_rect(area) else {
        return;
    };
    let slab = rect.height == 3;
    let edges = crate::surface::Edges {
        top: slab,
        bottom: slab,
    };
    // Every content row starts blank; the text overwrites its own row,
    // and the tint stays either way.
    for row in rect.rows() {
        buf.set_stringn(
            rect.x,
            row.y,
            " ".repeat(usize::from(rect.width)),
            usize::from(rect.width),
            Style::new(),
        );
    }
    if crate::surface::boxed() {
        crate::surface::draw_box(buf, area, rect, edges);
    } else {
        crate::surface::draw_slab(buf, rect, Role::Surface, None, edges);
    }
    let (x, y, room) = content_row(rect);
    let (query, count) = bar_pieces(bar, room);
    let count_width = crate::format::width(&count);
    let pad = room
        .saturating_sub(
            crate::format::width(PREFIX)
                .saturating_add(drawn_width(&query))
                .saturating_add(1),
        )
        .saturating_sub(count_width.saturating_add(1));
    // The row's runs, recording the count's first cell: its cells are
    // the click target. The trailing blank is already blank.
    let dim = Style::new().add_modifier(Modifier::DIM);
    let has_count = !count.is_empty();
    let pieces = [
        (
            PREFIX.to_owned(),
            Style::new().fg(Role::Attention.color()),
            false,
        ),
        (query, Style::new().add_modifier(Modifier::BOLD), false),
        ("█".to_owned(), dim, false),
        (" ".repeat(pad), Style::new(), false),
        (count, dim, true),
    ];
    let mut x = x;
    let mut left = room;
    let mut count_x = x;
    for (text, style, is_count) in pieces {
        if is_count {
            count_x = x;
        }
        let (end, _) = buf.set_stringn(x, y, &text, left, style);
        left = left.saturating_sub(usize::from(end.saturating_sub(x)));
        x = end;
    }
    if has_count {
        targets.push(Target {
            id: TargetId::FindCount,
            rect: Rect::new(
                count_x,
                y,
                u16::try_from(count_width).unwrap_or(u16::MAX),
                1,
            ),
        });
    }
}

/// The bar's cursor on its block cell, while the bar draws: `None` where
/// the bar draws nothing.
pub(super) fn bar_cursor(bar: &FindBar, area: Rect) -> Option<Position> {
    let rect = bar_rect(area)?;
    let (x, y, room) = content_row(rect);
    let (query, _) = bar_pieces(bar, room);
    let x = x
        .saturating_add(u16::try_from(crate::format::width(PREFIX)).unwrap_or(u16::MAX))
        .saturating_add(u16::try_from(drawn_width(&query)).unwrap_or(u16::MAX));
    Some(Position::new(x, y))
}

#[cfg(test)]
#[path = "marks_tests.rs"]
mod tests;
