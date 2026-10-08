//! Drawing the search results as a swapped view (`docs/tui.md`, "Search",
//! "Swapped views"): the header, then every match one row each with the
//! lines around it, each row a click target jumping to its match.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::to_u16;
use crate::app::Snippet;
use crate::app::results::ResultsView;
use crate::mouse::{Target, TargetId};

/// The selected entry's style.
/// debt: a fixed style, not a theme role; upgrade when colour roles land
/// (see #685).
const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// A match's highlight.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const HIT: Style = Style::new().bg(Color::Indexed(58));

/// Draws `view` into `area`: the header on its first row, then one row
/// per entry from its top, the selected entry reversed and every match
/// marked. Pushes one jump target per drawn entry. Past the bottom, or on
/// an empty area, draws what fits.
pub(super) fn render(view: &ResultsView, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    if area.is_empty() {
        return;
    }
    buf.set_stringn(
        area.x,
        area.y,
        &view.header,
        usize::from(area.width),
        Style::default(),
    );
    let height = usize::from(area.height).saturating_sub(1);
    for (row, (at, entry)) in view
        .entries
        .iter()
        .enumerate()
        .skip(view.top)
        .take(height)
        .enumerate()
    {
        let y = area.y.saturating_add(1).saturating_add(to_u16(row));
        let (text, hit) = entry_row(entry, area.width);
        buf.set_stringn(area.x, y, &text, usize::from(area.width), Style::default());
        let rect = Rect::new(
            area.x.saturating_add(hit.start),
            y,
            hit.end.saturating_sub(hit.start),
            1,
        );
        // An empty range styles nothing: a match clipped past the edge
        // keeps no highlight.
        buf.set_style(rect.intersection(area), HIT);
        let rect = Rect::new(area.x, y, area.width, 1);
        if at == view.selected {
            buf.set_style(rect, SELECTED);
        }
        targets.push(Target {
            id: TargetId::FindResult(at),
            rect,
        });
    }
}

/// One entry's row text with its match's cell range: the non-empty lines
/// joined by one space, so a match at a page's edge shows only what the
/// scan kept. A row wider than the view shows the cells around the match,
/// so the match is always visible however long the lines around it are
/// (`docs/tui.md`, "Search").
fn entry_row(entry: &Snippet, width: u16) -> (String, Range<u16>) {
    let Snippet {
        before,
        line,
        at,
        after,
    } = entry;
    let mut text = String::new();
    if !before.is_empty() {
        text.push_str(before);
        text.push(' ');
    }
    let prefix = crate::format::width(&text);
    text.push_str(line);
    if !after.is_empty() {
        text.push(' ');
        text.push_str(after);
    }
    let total = crate::format::width(&text);
    let target = usize::from(width);
    // The match's cells from the row's start: the lines before it, then
    // the cut line's chars up to the match.
    let skipped: String = line.chars().take(at.start).collect();
    let start = prefix.saturating_add(crate::format::width(&skipped));
    let span = at.end.saturating_sub(at.start);
    let hit: String = line.chars().skip(at.start).take(span).collect();
    let hit_width = crate::format::width(&hit);
    if total <= target {
        let start = to_u16(start);
        let end = start.saturating_add(to_u16(hit_width));
        return (text, start.min(end)..end);
    }
    // The row is wider than the view: the window holds `target` cells
    // around the match, so the cells around it show the lines around
    // the match instead of hiding it behind its own context.
    if target == 0 {
        return (String::new(), 0..0);
    }
    let end = start.saturating_add(hit_width);
    let win = start
        .saturating_sub(target.saturating_sub(hit_width) / 2)
        .min(total.saturating_sub(target));
    let stop = win.saturating_add(target);
    windowed(&text, start, end, win, stop)
}

/// The cells `win..stop` of `text` with the match `start..end` relative
/// to them: graphemes straddling an edge are dropped, so the row never runs
/// past the view (`docs/tui.md`, "Search").
fn windowed(text: &str, start: usize, end: usize, win: usize, stop: usize) -> (String, Range<u16>) {
    let mut out = String::new();
    let mut cells = 0usize;
    let mut shown = 0u16..0u16;
    for grapheme in Span::raw(text).styled_graphemes(Style::default()) {
        // Use the whole-string width measure so traversal agrees with the bounds.
        let step = crate::format::width(grapheme.symbol);
        let next = cells.saturating_add(step);
        if cells >= win && next <= stop {
            if cells == start {
                shown.start = to_u16(crate::format::width(&out));
            }
            out.push_str(grapheme.symbol);
            if next == end {
                shown.end = to_u16(crate::format::width(&out));
            }
        }
        cells = next;
    }
    // A match wider than the window clips at its edge: what fits still
    // shows.
    if end > stop {
        shown.end = to_u16(crate::format::width(&out));
    }
    (out, shown)
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
