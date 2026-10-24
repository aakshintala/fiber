//! A call view's rows (`docs/tui.md`, "Swapped views"): the frame's
//! body rows from the view's snapshot and held pages, the model's-view
//! markers placed from `kept`, the copy footer frame, and the scoped
//! search highlight. Every builder here is pure over its inputs: the
//! view in `call_view` owns the state and passes a [`RowsInput`].

use std::collections::BTreeMap;

use super::call_view_pages::{Page, Seg};
use crate::diff::gutter_prefix;
use crate::swapped::{Frame, Ink, List};
use crate::theme::Role;
use crate::turn::opened;

/// The marker where the model's view stops.
const END_MARK: &str = "── end of the model's view ──";
/// The marker where the model's view resumes.
const RESUME_MARK: &str = "── the model's view resumes ──";

/// Where one drawn body row comes from: a prefix or closing row by its
/// index, an output line by its page and line, or a marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum Source {
    Prefix(usize),
    Page {
        offset: u64,
        index: usize,
    },
    Closing(usize),
    Marker {
        offset: u64,
        index: usize,
        resume: bool,
    },
}

/// One drawn body row: its cells and where it comes from.
pub(in crate::app) struct Row {
    pub(in crate::app) cells: Vec<(String, Ink)>,
    pub(in crate::app) source: Source,
}

/// What [`build_rows`] draws: the uncapped or prefix and closing rows,
/// the paging inputs, and the held pages with their extents.
pub(in crate::app) struct RowsInput<'a> {
    pub(in crate::app) prefix: &'a [opened::Body],
    pub(in crate::app) plain: &'a [opened::Body],
    pub(in crate::app) closing: &'a [opened::Body],
    pub(in crate::app) numbering: Option<u64>,
    pub(in crate::app) paged: bool,
    pub(in crate::app) kept: Option<&'a Vec<[u64; 2]>>,
    pub(in crate::app) size: u64,
    pub(in crate::app) complete: bool,
    pub(in crate::app) pages: &'a BTreeMap<u64, Page>,
    pub(in crate::app) segs: &'a BTreeMap<u64, Seg>,
}

/// The view's body rows: prefix rows, held output lines with marker
/// rows at their anchors, then closing rows. Rows from a result
/// with no artifact are its uncapped rows whole.
pub(in crate::app) fn build_rows(input: &RowsInput<'_>) -> Vec<Row> {
    if !input.paged {
        return plain_rows(input);
    }
    let mut out = Vec::new();
    for (index, body) in input.prefix.iter().enumerate() {
        out.push(Row {
            cells: body_cells(body, 0),
            source: Source::Prefix(index),
        });
    }
    let mut anchors = markers(input);
    anchors.sort_by_key(|(at, _)| *at);
    for offset in input.pages.keys().copied().collect::<Vec<_>>() {
        let Some(page) = input.pages.get(&offset) else {
            continue;
        };
        let lines = page_lines(&page.text);
        for (index, text) in lines.iter().enumerate() {
            while anchors
                .first()
                .is_some_and(|(at, _)| *at == (offset, index))
            {
                let (at, marker) = anchors.remove(0);
                out.push(marker_row(at.0, at.1, marker));
            }
            let number = input.numbering.map(|start| {
                start
                    .saturating_add(line_before(input, offset))
                    .saturating_add(index as u64)
            });
            out.push(Row {
                cells: output_cells(text, number),
                source: Source::Page { offset, index },
            });
        }
        let extent = input.segs.get(&offset).map_or(0, |seg| seg.lines.len());
        while anchors
            .first()
            .is_some_and(|(at, _)| *at == (offset, extent))
        {
            let (at, resume) = anchors.remove(0);
            out.push(marker_row(at.0, at.1, resume));
        }
    }
    for (index, body) in input.closing.iter().enumerate() {
        out.push(Row {
            cells: body_cells(body, gutter(input)),
            source: Source::Closing(index),
        });
    }
    out
}

/// The uncapped rows for a result with no artifact, with their
/// gutter fitted to the largest number drawn.
fn plain_rows(input: &RowsInput<'_>) -> Vec<Row> {
    let gutter = input
        .plain
        .iter()
        .fold(0usize, |gutter, body| gutter.max(gutter_width(body)));
    input
        .plain
        .iter()
        .enumerate()
        .map(|(index, body)| Row {
            cells: body_cells(body, gutter),
            source: Source::Prefix(index),
        })
        .collect()
}

/// The gutter width the output numbers need: digits of the largest
/// drawn number, or of the start alone while nothing is held.
fn gutter(input: &RowsInput<'_>) -> usize {
    let Some(start) = input.numbering else {
        return 0;
    };
    let mut width = digits(start);
    for offset in input.pages.keys().copied().collect::<Vec<_>>() {
        let lines = input.segs.get(&offset).map_or(0, |seg| seg.lines.len());
        let last = start
            .saturating_add(line_before(input, offset))
            .saturating_add(lines.saturating_sub(1) as u64);
        width = width.max(digits(last));
    }
    width
}

/// How many output lines the fetched pages before `offset` hold:
/// every segment's line count, which is its starts' count.
fn line_before(input: &RowsInput<'_>, offset: u64) -> u64 {
    input
        .segs
        .range(..offset)
        .map(|(_, seg)| seg.lines.len() as u64)
        .sum()
}

/// The marker anchors the held pages can draw: for each `kept` edge
/// the first absolute logical line start at or past it, as a page
/// offset and line index, or the end of the final held fragment at
/// the end of the file, each with whether the model's view resumes
/// there. A page's first start equal to its offset is no boundary
/// when the previous page ends without a newline, so it is skipped;
/// an edge with no later start stays pending until the pages around
/// it, or the end of the file, arrive. No markers when the result
/// carries no `kept`.
fn markers(input: &RowsInput<'_>) -> Vec<((u64, usize), bool)> {
    let Some(kept) = input.kept else {
        return Vec::new();
    };
    let mut edges: Vec<(u64, bool)> = Vec::new();
    for range in kept {
        let [start, end] = *range;
        if start > 0 {
            edges.push((start, true));
        }
        if end < input.size {
            edges.push((end, false));
        }
    }
    edges.sort();
    edges
        .into_iter()
        .filter_map(|(edge, resume)| marker_for(input, edge).map(|at| (at, resume)))
        .collect()
}

/// The anchor for the `kept` edge: the first logical line start at
/// or past it that a held page draws, or the end of the final held
/// fragment when the file ends with no later start. `None` while the
/// edge's pages are still missing or paged out.
fn marker_for(input: &RowsInput<'_>, edge: u64) -> Option<(u64, usize)> {
    let mut first = true;
    let mut ends_newline = true;
    for (offset, seg) in input.segs {
        for (index, start) in seg.lines.iter().enumerate() {
            let continuation = index == 0 && !first && !ends_newline;
            if !continuation && *start >= edge && input.pages.contains_key(offset) {
                return Some((*offset, index));
            }
            first = false;
        }
        ends_newline = seg.ends_newline;
    }
    if input.complete {
        let (offset, seg) = input.segs.last_key_value()?;
        if input.pages.contains_key(offset) {
            return Some((*offset, seg.lines.len()));
        }
    }
    None
}

/// One text line of a held page: each line pairs with one `lines`
/// entry, with no trailing empty.
fn page_lines(text: &str) -> Vec<&str> {
    text.split_terminator('\n').collect()
}

/// The view's frame: the capitalised kind bold and its target dim
/// on the top row, the body rows, and "y copies the whole call" dim
/// on the last row, with the failure notice above it when a page
/// failed to load.
pub(in crate::app) fn build_frame(
    title: &str,
    command: &str,
    rows: Vec<Row>,
    list: List,
    below: Vec<String>,
) -> Frame {
    Frame {
        title: title.to_owned(),
        command: command.to_owned(),
        rows: rows
            .into_iter()
            .map(|row| {
                row.cells
                    .into_iter()
                    .map(|(text, ink)| (text, None, ink))
                    .collect()
            })
            .collect(),
        list,
        below,
        field: None,
        footer: "y copies the whole call".to_owned(),
    }
}

/// Restyles matched body rows with the query's hits: every hit on the
/// match tint, the current match brighter. Only body text ever
/// matches: markers draw from chrome and never match, as the top row
/// and footer never do.
pub(in crate::app) fn highlight_rows(
    rows: &mut [Row],
    query: &str,
    current: Option<usize>,
    matches: &[usize],
) {
    if query.is_empty() {
        return;
    }
    let current_row = current.and_then(|at| matches.get(at).copied());
    for (index, row) in rows.iter_mut().enumerate() {
        if !matches!(
            row.source,
            Source::Prefix(_) | Source::Page { .. } | Source::Closing(_)
        ) {
            continue;
        }
        let text: String = row.cells.iter().map(|(text, _)| text.as_str()).collect();
        let hits = crate::logical::matches(&text, query);
        if hits.is_empty() {
            continue;
        }
        let current = Some(index) == current_row;
        row.cells = highlight_cells(&row.cells, &text, &hits, current);
    }
}

/// A body row's cells with the gutter its numbers need.
fn body_cells(body: &opened::Body, gutter: usize) -> Vec<(String, Ink)> {
    let mut cells = body.cells();
    if body.gutter
        && let Some(prefix) = gutter_prefix(body.number, gutter)
    {
        cells.insert(0, (prefix, Ink::Muted));
    }
    cells
}

/// An output line's cells: its number's dim gutter when numbered, else
/// its text plain.
fn output_cells(text: &str, number: Option<u64>) -> Vec<(String, Ink)> {
    match number {
        Some(number) => {
            let width = digits(number);
            let prefix = gutter_prefix(Some(number), width).unwrap_or_default();
            vec![(prefix, Ink::Muted), (text.to_owned(), Ink::Plain)]
        }
        None => vec![(text.to_owned(), Ink::Plain)],
    }
}

/// A marker's row: its text dim across the whole row.
fn marker_row(offset: u64, index: usize, resume: bool) -> Row {
    Row {
        cells: vec![(
            if resume {
                RESUME_MARK.to_owned()
            } else {
                END_MARK.to_owned()
            },
            Ink::Muted,
        )],
        source: Source::Marker {
            offset,
            index,
            resume,
        },
    }
}

/// The digits `number` draws in.
fn digits(number: u64) -> usize {
    number.to_string().len().max(1)
}

/// The gutter width one numbered body needs.
fn gutter_width(body: &opened::Body) -> usize {
    if body.gutter {
        digits(body.number.unwrap_or(0))
    } else {
        0
    }
}

/// The cells' text restyled around the query's char-range hits: hit
/// cells on the match tint, or the current tint for the current match,
/// every other cell keeping its ink. A hit spanning cells marks each
/// part; ranges that end at a cell's start belong to the next cell.
fn highlight_cells(
    cells: &[(String, Ink)],
    text: &str,
    hits: &[std::ops::Range<usize>],
    current: bool,
) -> Vec<(String, Ink)> {
    let starts: Vec<usize> = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(text.len()))
        .collect();
    let byte_of = |char_index: usize| starts.get(char_index).copied().unwrap_or(text.len());
    let ranges: Vec<(usize, usize)> = hits
        .iter()
        .map(|hit| (byte_of(hit.start), byte_of(hit.end)))
        .collect();
    let mark = if current {
        Ink::Marked(Role::MatchCurrent)
    } else {
        Ink::Marked(Role::Match)
    };
    let mut out = Vec::new();
    let mut offset = 0usize;
    for cell in cells {
        let end = offset + cell.0.len();
        // Hit parts inside this cell, clipped to it, in order.
        let mut at = offset;
        for (from, to) in ranges.iter().copied() {
            if to <= offset || from >= end {
                continue;
            }
            let from = from.max(at);
            let to = to.min(end);
            if from < to {
                if at < from {
                    out.push((cell.0[at - offset..from - offset].to_owned(), cell.1));
                }
                out.push((cell.0[from - offset..to - offset].to_owned(), mark));
                at = to;
            }
        }
        if at < end {
            out.push((cell.0[at - offset..].to_owned(), cell.1));
        }
        offset = end;
    }
    out.into_iter()
        .filter(|(text, _)| !text.is_empty())
        .collect()
}

/// One row's whole text, cells joined.
pub(in crate::app) fn row_text(cells: &[(String, Ink)]) -> String {
    cells.iter().map(|(text, _)| text.as_str()).collect()
}

/// The rows one frame draws in `height`: the top row, the notice rows
/// and the footer leave this many body rows.
pub(in crate::app) fn shown_height(height: usize, below: usize) -> usize {
    height.saturating_sub(2 + below).max(1)
}

/// The visible body rows: the first row shown and how many show in
/// `height` less the top row, the notice rows and the footer.
pub(in crate::app) fn window(
    rows: &[Row],
    list: &List,
    below: usize,
    height: usize,
) -> (usize, usize) {
    let top = list.top().min(rows.len().saturating_sub(1));
    (top, shown_height(height, below))
}
