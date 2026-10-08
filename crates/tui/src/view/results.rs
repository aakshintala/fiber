//! Drawing the search results as a swapped view (`docs/tui.md`, "Search",
//! "Swapped views"): the header, then every match one row each with the
//! lines around it, each row a click target jumping to its match.

use std::ops::Range;

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::to_u16;
use crate::app::Snippet;
use crate::app::results::ResultsView;
use crate::mouse::{Target, TargetId};
use crate::theme::Role;

/// The selected entry's style.
const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// A match's highlight.
const HIT: Style = Style::new().bg(Role::Match.color());

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
    // The match's bytes in the row: `at` counts the cut line's chars.
    let skipped: usize = line.chars().take(at.start).map(char::len_utf8).sum();
    let span: usize = line
        .chars()
        .skip(at.start)
        .take(at.end.saturating_sub(at.start))
        .map(char::len_utf8)
        .sum();
    let hit_start = text.len().saturating_add(skipped);
    let hit_end = hit_start.saturating_add(span);
    text.push_str(line);
    if !after.is_empty() {
        text.push(' ');
        text.push_str(after);
    }
    let cells = drawn(&text);
    // The graphemes the match covers; a match covering none (an empty
    // one) anchors at the first grapheme drawn after its start.
    let overlaps = |cell: &Drawn| cell.bytes.start < hit_end && cell.bytes.end > hit_start;
    let first = cells.iter().position(overlaps).unwrap_or_else(|| {
        cells
            .iter()
            .position(|cell| cell.bytes.end > hit_start)
            .unwrap_or(cells.len())
    });
    let last = cells
        .iter()
        .rposition(overlaps)
        .map_or(first, |index| index.saturating_add(1));
    // The column grapheme `index` is drawn from, or the row's drawn
    // width past its last: columns are contiguous, so the cells drawn
    // by graphemes `lo..hi` are exactly `edge(hi) - edge(lo)`.
    let edge = |index: usize| {
        cells.get(index).map_or_else(
            || {
                cells
                    .last()
                    .map_or(0, |cell| cell.col.saturating_add(cell.width))
            },
            |cell| cell.col,
        )
    };
    let extent = |lo: usize, hi: usize| edge(hi).saturating_sub(edge(lo));
    // The window holds whole graphemes around the match, grown from it
    // while their drawn cells fit the view, so the match is always
    // visible and a row that fits shows whole. A match wider than the
    // view clips at its edge, so what fits still shows.
    let target = usize::from(width);
    let (mut lo, mut hi) = (first, last);
    if extent(lo, hi) > target {
        hi = (lo..hi)
            .rev()
            .find(|&end| extent(lo, end) <= target)
            .unwrap_or(lo);
    } else {
        // Each pass adds at least one of the row's graphemes or ends the
        // growth, so the row's length bounds the passes.
        for _ in 0..cells.len() {
            let mut grew = false;
            if let Some(prev) = lo.checked_sub(1)
                && extent(prev, hi) <= target
            {
                lo = prev;
                grew = true;
            }
            if hi < cells.len() && extent(lo, hi.saturating_add(1)) <= target {
                hi = hi.saturating_add(1);
                grew = true;
            }
            if !grew {
                break;
            }
        }
    }
    // An empty window reads backwards (its last grapheme ends at or
    // before its first starts), which `get` answers with nothing.
    let shown = match (
        cells.get(lo),
        hi.checked_sub(1).and_then(|end| cells.get(end)),
    ) {
        (Some(from), Some(to)) => text
            .get(from.bytes.start..to.bytes.end)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    };
    let start = extent(lo, first.clamp(lo, hi));
    let end = extent(lo, last.clamp(lo, hi));
    (shown, to_u16(start)..to_u16(end))
}

/// One grapheme of a row as `Buffer::set_stringn` draws it: its bytes in
/// the row and the cells it covers, counted from the row's first cell.
struct Drawn {
    bytes: Range<usize>,
    col: usize,
    width: usize,
}

/// Where `row`'s graphemes land when [`render`] draws it with
/// `Buffer::set_stringn`, by that function's own rule: it skips a
/// grapheme holding a control character or drawing no cells, and draws
/// every other at the next column, advancing by that grapheme's
/// `cell_width`. Widths of whole strings never add up the same way (a
/// lam-alef pair is one cell as a string but two drawn), so no other
/// measure places a row's graphemes.
fn drawn(row: &str) -> Vec<Drawn> {
    let base = row.as_ptr().addr();
    let mut col = 0usize;
    let mut out = Vec::new();
    for grapheme in Span::raw(row).styled_graphemes(Style::default()) {
        let width = usize::from(grapheme.symbol.cell_width());
        if width == 0 {
            continue;
        }
        let start = grapheme.symbol.as_ptr().addr().saturating_sub(base);
        out.push(Drawn {
            bytes: start..start.saturating_add(grapheme.symbol.len()),
            col,
            width,
        });
        col = col.saturating_add(width);
    }
    out
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
