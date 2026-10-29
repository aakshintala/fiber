//! Cell layout for the markdown renderer: widths, wrapping and the link
//! columns of wrapped rows. Stateless, so nothing here reads the renderer's
//! state (`docs/tui.md`, "Look" and "Links").

use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::rows::Join;

/// One character and its style, before wrapping.
pub(super) type Cell = (char, Style);

/// Wraps `cells` into lines `width` wide, the first after `first` and the
/// rest after `rest`, each with how it joins the line before. With `words`
/// a line breaks between words where it can, and spaces at a break are
/// dropped; without, it breaks at the width. A newline ends a line.
pub(super) fn wrap(
    cells: &[Cell],
    width: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
    words: bool,
) -> Vec<(Line<'static>, Join)> {
    let prefix = |spans: &[Span<'static>]| spans.iter().map(Span::width).sum::<usize>();
    let first_room = width.saturating_sub(prefix(first)).max(1);
    let rest_room = width.saturating_sub(prefix(rest)).max(1);
    wrap_joined_indices(cells, first_room, rest_room, words)
        .into_iter()
        .enumerate()
        .map(|(at, (row, join, _))| {
            let mut spans = if at == 0 {
                first.to_vec()
            } else {
                rest.to_vec()
            };
            spans.extend(spans_of(&row));
            (Line::from(spans), join)
        })
        .collect()
}

/// Wraps `cells` into rows: `first` cells wide for the first, `rest` for
/// each after. Each row comes with how it joins the row before: the first
/// and each after a newline break; one broken inside a word joins with
/// nothing; one broken where spaces were dropped joins with one space. Each
/// row also has the indices into `cells` its cells came from, so a link range
/// over `cells` maps onto the rows that hold it (`docs/tui.md`, "Links").
/// Dropped spaces and newlines are in no row.
pub(super) fn wrap_joined_indices(
    cells: &[Cell],
    first: usize,
    rest: usize,
    words: bool,
) -> Vec<(Vec<Cell>, Join, Vec<usize>)> {
    let widths: Vec<usize> = cells
        .iter()
        .map(|(ch, _)| crate::text::char_width(*ch))
        .collect();
    let mut rows = Vec::new();
    visit_wrap_ranges(cells, &widths, first, rest, words, |range, join| {
        let row = cells.get(range.clone()).unwrap_or_default().to_vec();
        rows.push((row, join, range.collect()));
    });
    rows
}

/// Visits the source ranges used by the drawing wrapper, without making
/// each row's cells or indices. Both drawing and counting use this walker.
pub(super) fn visit_wrap_ranges(
    cells: &[Cell],
    widths: &[usize],
    first: usize,
    rest: usize,
    words: bool,
    mut visit: impl FnMut(Range<usize>, Join),
) {
    let mut rows = 0usize;
    let mut start = 0usize;
    let mut end = 0usize;
    let mut join = Join::Break;
    let mut used = 0usize;
    let mut at = 0usize;
    while let Some(&(ch, _)) = cells.get(at) {
        if ch == '\n' {
            visit(start..end, join);
            rows = rows.saturating_add(1);
            at = at.saturating_add(1);
            start = at;
            end = at;
            join = Join::Break;
            used = 0;
            continue;
        }
        let piece_start = at;
        let next = at.saturating_add(1);
        let piece_end = if words && ch != ' ' {
            cells
                .iter()
                .skip(next)
                .position(|(ch, _)| *ch == ' ' || *ch == '\n')
                .map_or(cells.len(), |len| next.saturating_add(len))
        } else {
            next
        };
        at = piece_end;
        let piece_width = widths
            .get(piece_start..piece_end)
            .unwrap_or_default()
            .iter()
            .fold(0usize, |sum, width| sum.saturating_add(*width));
        let room = if rows == 0 { first } else { rest };
        if words && start < end && used.saturating_add(piece_width) > room && piece_width <= rest {
            while end > start && cells.get(end - 1).is_some_and(|(ch, _)| *ch == ' ') {
                end -= 1;
                used = used.saturating_sub(widths.get(end).copied().unwrap_or_default());
            }
            visit(start..end, join);
            rows = rows.saturating_add(1);
            start = piece_start;
            end = piece_start;
            join = Join::WrapSpace;
            used = 0;
        }
        if words && ch == ' ' && start == end && rows > 0 {
            start = at;
            end = at;
            continue;
        }
        for index in piece_start..piece_end {
            let room = if rows == 0 { first } else { rest };
            let cell_width = widths.get(index).copied().unwrap_or_default();
            if used.saturating_add(cell_width) > room && start < end {
                visit(start..end, join);
                rows = rows.saturating_add(1);
                start = index;
                join = Join::Wrap;
                used = 0;
            }
            end = index.saturating_add(1);
            used = used.saturating_add(cell_width);
        }
    }
    if start < end {
        visit(start..end, join);
    }
}

/// The links drawn on one wrapped row: each link range over the source
/// cells that this row's cells hold, from its first cell to its last
/// one's end, offset past the row's prefix `prefix` cells
/// (`docs/tui.md`, "Links").
pub(super) fn link_cols(
    row: &[Cell],
    indices: &[usize],
    prefix: usize,
    links: &[(Range<usize>, String)],
) -> Vec<(Range<u16>, String)> {
    let mut out = Vec::new();
    for (range, url) in links {
        let first = indices.iter().position(|source| range.contains(source));
        let last = indices.iter().rposition(|source| range.contains(source));
        if let (Some(first), Some(last)) = (first, last)
            && let Some(cells) = row.get(first..=last)
        {
            let mut start = prefix;
            for (ch, _) in row.get(..first).unwrap_or_default() {
                start = start.saturating_add(crate::text::char_width(*ch));
            }
            let mut end = start;
            for (ch, _) in cells {
                end = end.saturating_add(crate::text::char_width(*ch));
            }
            let start = u16::try_from(start).unwrap_or(u16::MAX);
            let end = u16::try_from(end).unwrap_or(u16::MAX);
            if start < end {
                out.push((start..end, url.clone()));
            }
        }
    }
    out
}

/// Cells as spans, one per run of the same style.
pub(super) fn spans_of(cells: &[Cell]) -> Vec<Span<'static>> {
    cells
        .chunk_by(|a, b| a.1 == b.1)
        .filter_map(|run| {
            let &(_, style) = run.first()?;
            let text: String = run.iter().map(|(ch, _)| *ch).collect();
            Some(Span::styled(text, style))
        })
        .collect()
}
