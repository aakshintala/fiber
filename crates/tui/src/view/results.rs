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
    let target = usize::from(width);
    // Widths never add up (a ZWJ sequence, a lam-alef pair, a wide char
    // and its neighbours all measure differently together than apart),
    // so every width below is the width of the whole slice being drawn,
    // measured by `crate::format::width` (the string-level width ratatui
    // draws a Span/Line with): traversal and bounds share one measure
    // and cannot disagree.
    if crate::format::width(&text) <= target {
        let start = to_u16(crate::format::width(
            text.get(..hit_start).unwrap_or_default(),
        ));
        let end = to_u16(crate::format::width(
            text.get(..hit_end).unwrap_or_default(),
        ));
        return (text, start.min(end)..end);
    }
    // The row is wider than the view: the window holds `target` cells
    // around the match, so the cells around it show the lines around
    // the match instead of hiding it behind its own context.
    if target == 0 {
        return (String::new(), 0..0);
    }
    windowed(&text, hit_start, hit_end, target)
}

/// The grapheme-aligned slice of `row` around the match's bytes
/// `hit_start..hit_end`: whole graphemes grown from the hit while the
/// width of the whole candidate slice fits `target`, so the match stays
/// visible however long the lines around it are. A grapheme straddling
/// the edge is dropped, so the row never runs past the view; a match
/// wider than the view clips at its edge, so what fits still shows
/// (`docs/tui.md`, "Search").
fn windowed(row: &str, hit_start: usize, hit_end: usize, target: usize) -> (String, Range<u16>) {
    // The row's grapheme edges in bytes; every slice below starts and
    // ends on one, so no width is ever a sum of per-piece widths.
    let mut edges = vec![0usize];
    for grapheme in Span::raw(row).styled_graphemes(Style::default()) {
        let last = edges.last().copied().unwrap_or_default();
        edges.push(last.saturating_add(grapheme.symbol.len()));
    }
    let graphemes = edges.len().saturating_sub(1);
    // The `at` helper reads an edge that is always there: every index
    // below comes from the edges themselves.
    let at = |index: usize| edges.get(index).copied().unwrap_or_default();
    // `row` between two grapheme edges, the only slicing this window
    // does.
    let between = |from: usize, to: usize| row.get(at(from)..at(to)).unwrap_or_default();
    // The graphemes overlapping the hit; an empty hit anchors at the
    // grapheme holding its bytes.
    let mut lo = graphemes;
    let mut hi = graphemes;
    for (index, pair) in edges.windows(2).enumerate() {
        let first = pair.first().copied().unwrap_or_default();
        let second = pair.get(1).copied().unwrap_or_default();
        if first < hit_end && second > hit_start {
            if lo == graphemes {
                lo = index;
            }
            hi = index.saturating_add(1);
        }
    }
    if lo == graphemes {
        lo = (0..graphemes)
            .find(|&index| at(index.saturating_add(1)) > hit_start)
            .unwrap_or(graphemes);
        hi = lo;
    }
    if lo < hi && crate::format::width(between(lo, hi)) > target {
        let mut end = lo;
        while end < hi && crate::format::width(between(lo, end.saturating_add(1))) <= target {
            end = end.saturating_add(1);
        }
        let shown = between(lo, end).to_owned();
        let end = to_u16(crate::format::width(&shown));
        return (shown, 0..end);
    }
    while lo > 0 || hi < graphemes {
        let mut grew = false;
        if lo > 0 && crate::format::width(between(lo.saturating_sub(1), hi)) <= target {
            lo = lo.saturating_sub(1);
            grew = true;
        }
        if hi < graphemes && crate::format::width(between(lo, hi.saturating_add(1))) <= target {
            hi = hi.saturating_add(1);
            grew = true;
        }
        if !grew {
            break;
        }
    }
    let shown = between(lo, hi).to_owned();
    let from = at(lo);
    let start = to_u16(crate::format::width(
        shown
            .get(..hit_start.clamp(from, at(hi)).saturating_sub(from))
            .unwrap_or_default(),
    ));
    let end = to_u16(crate::format::width(
        shown
            .get(..hit_end.clamp(from, at(hi)).saturating_sub(from))
            .unwrap_or_default(),
    ));
    (shown, start.min(end)..end)
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
