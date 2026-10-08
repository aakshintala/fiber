//! What the view paints over the conversation's rows once they are drawn
//! (`docs/tui.md`, "Selection and copy", "Links", "Search"): the search
//! bar, the selection's highlight, and the links' targets with their
//! underline.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use crate::app::{App, FindBar};
use crate::mouse::Target;

/// The selection's highlight.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const SELECTION: Style = Style::new().bg(Color::Indexed(24));

/// A match's highlight.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const MATCH: Style = Style::new().bg(Color::Indexed(58));

/// The current match's highlight.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const CURRENT: Style = Style::new().bg(Color::Indexed(178)).fg(Color::Black);

/// A link's underline: markdown links already draw underlined, and bare
/// URLs gain it here (`docs/tui.md`, "Links").
const LINK: Style = Style::new().add_modifier(Modifier::UNDERLINED);

/// Paints the search bar on the conversation's top-right row, then the
/// selection's cells, then pushes one click target per row each visible
/// link covers and underlines its cells. With no bar, no selection and no
/// link on screen this costs one check each (`docs/tui.md`,
/// "Performance").
pub(super) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    if let Some(bar) = app.find_bar() {
        draw_bar(&bar, area, buf);
    }
    for (rect, current) in app.find_marks(area) {
        let style = if current { CURRENT } else { MATCH };
        buf.set_style(rect.intersection(area), style);
    }
    for rect in app.selection_cells(area) {
        buf.set_style(rect.intersection(area), SELECTION);
    }
    for link in app.visible_links(area) {
        for rect in link.rects {
            let rect = rect.intersection(area);
            if rect.is_empty() {
                continue;
            }
            buf.set_style(rect, LINK);
            targets.push(Target { id: link.id, rect });
        }
    }
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

/// Paints the bar right-aligned on `area`'s first row.
fn draw_bar(bar: &FindBar, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    let (x, width) = bar_span(bar, area);
    buf.set_stringn(x, area.y, bar_text(bar), width, Style::default());
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
