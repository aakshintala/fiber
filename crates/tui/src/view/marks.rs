//! What the view paints over the conversation's rows once they are drawn
//! (`docs/tui.md`, "Selection and copy", "Links", "Search"): the search
//! bar, the selection's highlight, and the links' targets with their
//! underline.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};

use crate::app::{App, FindBar};
use crate::mouse::{Target, TargetId};
use crate::theme::Role;

/// The selection's highlight: reversed, so it shows with no colour.
const SELECTION: Style = Style::new()
    .bg(Role::Selection.color())
    .add_modifier(Modifier::REVERSED);

/// A match's highlight: underlined, so it shows with no colour.
const MATCH: Style = Style::new()
    .bg(Role::Match.color())
    .add_modifier(Modifier::UNDERLINED);

/// The current match's highlight: the brighter match tint, in the full
/// text colour; reversed and bold, so it shows with no colour and apart
/// from the selection.
const CURRENT: Style = Style::new()
    .bg(Role::MatchCurrent.color())
    .fg(Role::Text.color())
    .add_modifier(Modifier::REVERSED)
    .add_modifier(Modifier::BOLD);

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
    if let Some(bar) = app.find_bar() {
        draw_bar(&bar, area, buf, targets);
    }
    if app.results_open() {
        return;
    }
    for (rect, current) in app.find_marks(area) {
        let style = if current { CURRENT } else { MATCH };
        buf.set_style(rect.intersection(area), style);
    }
    for rect in app.selection_cells(area) {
        buf.set_style(rect.intersection(area), SELECTION);
    }
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

/// The bar's text: `find: <query>`, and ` · <count>` once a scan has run.
fn bar_text(bar: &FindBar) -> String {
    if bar.count.is_empty() {
        format!("find: {}", bar.query)
    } else {
        format!("find: {} · {}", bar.query, bar.count)
    }
}

/// The bar's cells on `area`'s first row: its left edge and its width
/// (`docs/tui.md`, "Search": the bar floats over the conversation's
/// top-right corner).
fn bar_span(bar: &FindBar, area: Rect) -> (u16, usize) {
    let text = bar_text(bar);
    let width = crate::format::width(&text)
        .min(BAR_WIDTH)
        .min(usize::from(area.width));
    let x = area
        .right()
        .saturating_sub(u16::try_from(width).unwrap_or(u16::MAX));
    (x, width)
}

/// Paints the bar right-aligned on `area`'s first row, pushing a target
/// over its match count: a click opens the results (`docs/tui.md`,
/// "Search"). A count the bar's width cuts off is no target.
fn draw_bar(bar: &FindBar, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    if area.is_empty() {
        return;
    }
    let (x, width) = bar_span(bar, area);
    buf.set_stringn(x, area.y, bar_text(bar), width, Style::default());
    // The count starts past `find: <query> · `: past the bar's edge it
    // is cut off and no target.
    let start = crate::format::width(&format!("find: {} · ", bar.query));
    let width = width.saturating_sub(start);
    if width == 0 {
        return;
    }
    targets.push(Target {
        id: TargetId::FindCount,
        rect: Rect::new(
            x.saturating_add(u16::try_from(start).unwrap_or(u16::MAX)),
            area.y,
            u16::try_from(width).unwrap_or(u16::MAX),
            1,
        ),
    });
}

/// The bar's cursor at its query's end, while the bar is open: `None` on
/// an empty area.
pub(super) fn bar_cursor(bar: &FindBar, area: Rect) -> Option<Position> {
    if area.is_empty() {
        return None;
    }
    let (x, width) = bar_span(bar, area);
    let before = crate::format::width(&format!("find: {}", bar.query)).min(width);
    let x = x.saturating_add(u16::try_from(before).unwrap_or(u16::MAX));
    Some(Position::new(x, area.y))
}

#[cfg(test)]
#[path = "marks_tests.rs"]
mod tests;
