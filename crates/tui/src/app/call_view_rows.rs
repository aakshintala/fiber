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
            // Closing rows never carry numbers, so no gutter is fitted:
            // `gutter` would always be zero here (numbering only comes
            // with reads and writes, which draw no closing row).
            cells: body_cells(body, 0),
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

/// The row to park the window on when the page at `offset` completes a
/// kept edge: two rows above the end marker its arrival anchors, so the
/// head segment's last rows show above the marker. `None` when no end
/// edge ends in the landed page, or its marker anchors in another page.
pub(in crate::app) fn edge_anchor_row(
    input: &RowsInput<'_>,
    rows: &[Row],
    offset: u64,
) -> Option<usize> {
    let kept = input.kept?;
    kept.iter()
        .filter_map(|[_, end]| (*end < input.size).then_some(*end))
        .filter(|end| *end >= offset)
        .filter_map(|edge| marker_for(input, edge))
        .filter(|(at, _)| *at == offset)
        .filter_map(|(at, index)| {
            rows.iter().position(|row| {
                row.source
                    == Source::Marker {
                        offset: at,
                        index,
                        resume: false,
                    }
            })
        })
        .min()
        .map(|marker_at| marker_at.saturating_sub(2))
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
        joins: Vec::new(),
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
/// Empty slices push nothing visible: the trailing filter drops them,
/// so the guards below stay unconditional.
pub(in crate::app) fn highlight_cells(
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
            let from = from.max(at);
            let to = to.min(end);
            if from < to {
                // Unconditional: an empty `at..from` filters out below.
                out.push((cell.0[at - offset..from - offset].to_owned(), cell.1));
                out.push((cell.0[from - offset..to - offset].to_owned(), mark));
                at = to;
            }
        }
        // Unconditional: an empty tail filters out below.
        out.push((cell.0[at - offset..].to_owned(), cell.1));
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn row(source: Source) -> Row {
        Row {
            cells: vec![("t".to_owned(), Ink::Plain)],
            source,
        }
    }

    fn page(lines: &[u64], next: Option<u64>) -> (BTreeMap<u64, Page>, BTreeMap<u64, Seg>) {
        (
            BTreeMap::from([(
                0,
                Page {
                    text: String::new(),
                },
            )]),
            BTreeMap::from([(
                0,
                Seg {
                    lines: lines.to_vec(),
                    next,
                    ends_newline: true,
                },
            )]),
        )
    }

    fn rows_input<'a>(
        kept: &'a Option<Vec<[u64; 2]>>,
        size: u64,
        pages: &'a BTreeMap<u64, Page>,
        segs: &'a BTreeMap<u64, Seg>,
    ) -> RowsInput<'a> {
        RowsInput {
            prefix: &[],
            plain: &[],
            closing: &[],
            numbering: None,
            paged: true,
            kept: kept.as_ref(),
            size,
            complete: true,
            pages,
            segs,
        }
    }

    #[test]
    fn completing_page_parks_two_rows_above_its_end_marker() {
        let kept = Some(vec![[0, 150]]);
        let (pages, segs) = page(&[0, 100, 200], Some(14400));
        let input = rows_input(&kept, 1000, &pages, &segs);
        // The edge at 150 anchors at page 0 line 2, so the marker sits
        // at index 4 with two prefix rows ahead of the page: parking
        // two above it lands on index 2.
        let rows = vec![
            row(Source::Prefix(0)),
            row(Source::Prefix(1)),
            row(Source::Page {
                offset: 0,
                index: 0,
            }),
            row(Source::Page {
                offset: 0,
                index: 1,
            }),
            row(Source::Marker {
                offset: 0,
                index: 2,
                resume: false,
            }),
            row(Source::Page {
                offset: 0,
                index: 2,
            }),
        ];
        assert_eq!(edge_anchor_row(&input, &rows, 0), Some(2));
        // A marker in the first two rows saturates at the top.
        let rows = vec![
            row(Source::Page {
                offset: 0,
                index: 0,
            }),
            row(Source::Marker {
                offset: 0,
                index: 2,
                resume: false,
            }),
            row(Source::Page {
                offset: 0,
                index: 2,
            }),
        ];
        assert_eq!(edge_anchor_row(&input, &rows, 0), Some(0));
        // Two end edges in the landed page park under the earlier
        // marker even when the later edge comes first: the 250 edge
        // anchors at line 3, below the 150 edge.
        let kept = Some(vec![[0, 250], [0, 150]]);
        let (pages, segs) = page(&[0, 100, 200, 300], Some(14400));
        let input = rows_input(&kept, 1000, &pages, &segs);
        let rows = vec![
            row(Source::Page {
                offset: 0,
                index: 0,
            }),
            row(Source::Page {
                offset: 0,
                index: 1,
            }),
            row(Source::Marker {
                offset: 0,
                index: 2,
                resume: false,
            }),
            row(Source::Page {
                offset: 0,
                index: 2,
            }),
            row(Source::Marker {
                offset: 0,
                index: 3,
                resume: false,
            }),
            row(Source::Page {
                offset: 0,
                index: 3,
            }),
        ];
        assert_eq!(edge_anchor_row(&input, &rows, 0), Some(0));
        let rows = vec![
            row(Source::Marker {
                offset: 0,
                index: 2,
                resume: false,
            }),
            row(Source::Page {
                offset: 0,
                index: 2,
            }),
        ];
        assert_eq!(edge_anchor_row(&input, &rows, 0), Some(0));
    }

    #[test]
    fn other_pages_edges_and_missing_kept_park_nothing() {
        // No kept ranges: no edges, no anchor.
        let (pages, segs) = page(&[0, 100, 200], Some(14400));
        let input = rows_input(&None, 1000, &pages, &segs);
        let rows = vec![row(Source::Marker {
            offset: 0,
            index: 2,
            resume: false,
        })];
        assert_eq!(edge_anchor_row(&input, &rows, 0), None);
        // An end past the file size is no edge, and neither is an end
        // exactly at it: only a strictly earlier end leaves a marker.
        let kept = Some(vec![[0, 100000]]);
        let input = rows_input(&kept, 500, &pages, &segs);
        assert_eq!(edge_anchor_row(&input, &rows, 0), None);
        // The end-of-file fallback marker sits in the rows: the complete
        // file would anchor it at line 3, so a looser guard seats there.
        let kept = Some(vec![[0, 500]]);
        let input = rows_input(&kept, 500, &pages, &segs);
        let rows = vec![row(Source::Marker {
            offset: 0,
            index: 3,
            resume: false,
        })];
        assert_eq!(edge_anchor_row(&input, &rows, 0), None);
        // A kept start alone is a resume edge, never an end anchor.
        let kept = Some(vec![[500, 200000]]);
        let input = rows_input(&kept, 112293, &pages, &segs);
        assert_eq!(edge_anchor_row(&input, &rows, 0), None);
        // The edge lives in page 0 but page 0 paged out: the anchor the
        // landed page offers is a proxy for the older edge, not its own.
        let kept = Some(vec![[0, 150]]);
        let mut pages = BTreeMap::new();
        pages.insert(
            72000,
            Page {
                text: String::new(),
            },
        );
        let mut segs = BTreeMap::new();
        segs.insert(
            0,
            Seg {
                lines: vec![0, 100, 200],
                next: Some(72000),
                ends_newline: true,
            },
        );
        segs.insert(
            72000,
            Seg {
                lines: vec![72000, 72100],
                next: None,
                ends_newline: true,
            },
        );
        let input = rows_input(&kept, 112293, &pages, &segs);
        let rows = vec![
            row(Source::Page {
                offset: 72000,
                index: 0,
            }),
            row(Source::Marker {
                offset: 72000,
                index: 0,
                resume: false,
            }),
        ];
        assert_eq!(edge_anchor_row(&input, &rows, 72000), None);
        // The edge sits past the landed page's last line start: its
        // anchor lands in the next page, not here.
        let (pages, mut segs) = page(&[0, 100], Some(14400));
        segs.insert(
            14400,
            Seg {
                lines: vec![14400, 14500],
                next: None,
                ends_newline: true,
            },
        );
        let mut held = pages;
        held.insert(
            14400,
            Page {
                text: String::new(),
            },
        );
        let input = rows_input(&kept, 100000, &held, &segs);
        let rows = vec![
            row(Source::Page {
                offset: 0,
                index: 0,
            }),
            row(Source::Page {
                offset: 0,
                index: 1,
            }),
            row(Source::Page {
                offset: 14400,
                index: 0,
            }),
            row(Source::Marker {
                offset: 14400,
                index: 0,
                resume: false,
            }),
        ];
        assert_eq!(edge_anchor_row(&input, &rows, 0), None);
    }
}
