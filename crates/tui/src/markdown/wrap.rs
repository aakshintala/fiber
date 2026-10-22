//! Cell layout for the markdown renderer: widths, wrapping and the link
//! columns of wrapped rows. Stateless, so nothing here reads the renderer's
//! state (`docs/tui.md`, "Look" and "Links").

use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::rows::Join;

/// One character and its style, before wrapping.
pub(super) type Cell = (char, Style);

/// `text` cut to `room` cells, ending in `…` when cut.
pub(super) fn truncate(text: &str, room: usize) -> String {
    if cells_width(text) <= room {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let width = char_width(ch);
        if used.saturating_add(width).saturating_add(1) > room {
            break;
        }
        out.push(ch);
        used = used.saturating_add(width);
    }
    if room > 0 {
        out.push('…');
    }
    out
}

/// The cells `ch` takes on screen.
pub(super) fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    Span::raw(&*ch.encode_utf8(&mut buf)).width()
}

/// The cells `text` takes on screen.
pub(super) fn cells_width(text: &str) -> usize {
    Span::raw(text).width()
}

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
    let mut rows: Vec<(Vec<Cell>, Join, Vec<usize>)> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut indices: Vec<usize> = Vec::new();
    let mut join = Join::Break;
    let mut used = 0usize;
    let mut at = 0usize;
    // Each pass takes at least the cell at `at`, so the loop ends.
    while let Some(&(ch, _)) = cells.get(at) {
        if ch == '\n' {
            rows.push((std::mem::take(&mut row), join, std::mem::take(&mut indices)));
            join = Join::Break;
            used = 0;
            at = at.saturating_add(1);
            continue;
        }
        let next = at.saturating_add(1);
        let end = if words && ch != ' ' {
            cells
                .iter()
                .skip(next)
                .position(|(ch, _)| *ch == ' ' || *ch == '\n')
                .map_or(cells.len(), |len| next.saturating_add(len))
        } else {
            next
        };
        let piece = cells.get(at..end).unwrap_or_default();
        let start = at;
        at = end;
        let width: usize = piece.iter().map(|(ch, _)| char_width(*ch)).sum();
        let room = if rows.is_empty() { first } else { rest };
        // A word that overflows goes to the next row when it fits there.
        if words && !row.is_empty() && used.saturating_add(width) > room && width <= rest {
            // Each pass pops one cell, so the loop ends.
            while row.last().is_some_and(|(ch, _)| *ch == ' ') {
                row.pop();
                indices.pop();
            }
            rows.push((std::mem::take(&mut row), join, std::mem::take(&mut indices)));
            join = Join::WrapSpace;
            used = 0;
        }
        // Spaces at the start of a wrapped row are dropped. Only a break
        // at a space, which joins with one, or a newline leaves a row
        // empty: a break inside a word puts the cell that overflowed on the
        // new row at once.
        if words && ch == ' ' && row.is_empty() && !rows.is_empty() {
            continue;
        }
        // One pass per cell of the piece.
        for (offset, &cell) in piece.iter().enumerate() {
            let room = if rows.is_empty() { first } else { rest };
            let width = char_width(cell.0);
            if used.saturating_add(width) > room && !row.is_empty() {
                rows.push((std::mem::take(&mut row), join, std::mem::take(&mut indices)));
                join = Join::Wrap;
                used = 0;
            }
            row.push(cell);
            indices.push(start.saturating_add(offset));
            used = used.saturating_add(width);
        }
    }
    if !row.is_empty() {
        rows.push((row, join, indices));
    }
    rows
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
                start = start.saturating_add(char_width(*ch));
            }
            let mut end = start;
            for (ch, _) in cells {
                end = end.saturating_add(char_width(*ch));
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
