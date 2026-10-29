//! GFM tables: a bold header row, a rule under each column, columns
//! three spaces apart, numbers right-aligned, and a table wider than the
//! area shrunk widest column first (`docs/tui.md`, "Look").

use std::ops::Range;

use pulldown_cmark::Alignment;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::wrap::{Cell, link_cols, spans_of, visit_wrap_ranges, wrap_joined_indices};
use crate::theme::{Role, style};

/// Cells between columns.
const GAP: usize = 3;
/// A column shrinks no narrower than this.
const MIN_COLUMN: usize = 5;

/// One table cell: its inline cells and the link ranges in them, as
/// indices into the cells (`docs/tui.md`, "Links": links are handled on
/// click).
type TableCell = (Vec<Cell>, Vec<(Range<usize>, String)>);

/// One laid-out table line and the links drawn on it.
type TableLine = (Line<'static>, Vec<(Range<u16>, String)>);

/// One wrapped table cell's row: its cells, their indices into the
/// cell, and the cell's link ranges.
type WrappedCell<'a> = (Vec<Cell>, Vec<usize>, &'a [(Range<usize>, String)]);

/// A table being gathered: the alignment row, finished rows (the header
/// first) and the row in progress.
pub(super) struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<TableCell>>,
    row: Vec<TableCell>,
}

impl Table {
    pub(super) fn new(aligns: Vec<Alignment>) -> Self {
        Self {
            aligns,
            rows: Vec::new(),
            row: Vec::new(),
        }
    }

    pub(super) fn push_cell(&mut self, cell: Vec<Cell>, links: Vec<(Range<usize>, String)>) {
        self.row.push((cell, links));
    }

    pub(super) fn end_row(&mut self) {
        self.rows.push(std::mem::take(&mut self.row));
    }

    /// Counts the table's lines without assembling styled output.
    pub(super) fn count_rows(self, width: usize) -> (usize, bool) {
        let count = self
            .rows
            .iter()
            .map(Vec::len)
            .chain([self.aligns.len()])
            .max()
            .unwrap_or_default();
        let mut widths = vec![0usize; count];
        for row in &self.rows {
            for (column, (cell, _)) in widths.iter_mut().zip(row) {
                *column = (*column).max(cells_width(cell));
            }
        }
        shrink(&mut widths, width);
        let table_width = widths
            .iter()
            .fold(0usize, |sum, column| sum.saturating_add(*column))
            .saturating_add(count.saturating_sub(1).saturating_mul(GAP));
        let mut over_width = width > 0 && table_width > width;
        let mut lines = 0usize;
        for row in &self.rows {
            let mut height = 1usize;
            for (column, column_width) in widths.iter().enumerate() {
                let cell = row.get(column).map_or(&[][..], |(cell, _)| cell.as_slice());
                let cell_widths: Vec<usize> = cell
                    .iter()
                    .map(|(ch, _)| super::wrap::char_width(*ch))
                    .collect();
                let mut cell_rows = 0usize;
                visit_wrap_ranges(
                    cell,
                    &cell_widths,
                    *column_width,
                    *column_width,
                    true,
                    |range, _| {
                        let piece_width = cell_widths
                            .get(range)
                            .unwrap_or_default()
                            .iter()
                            .fold(0usize, |sum, width| sum.saturating_add(*width));
                        over_width |= width > 0 && piece_width > *column_width;
                        cell_rows = cell_rows.saturating_add(1);
                    },
                );
                height = height.max(cell_rows);
            }
            lines = lines.saturating_add(height);
        }
        if !self.rows.is_empty() {
            lines = lines.saturating_add(1);
        }
        (lines, over_width)
    }

    /// The table's lines at `width`, each with the links drawn on it and
    /// their cells in the line (`docs/tui.md`, "Links").
    pub(super) fn layout(self, width: usize) -> Vec<TableLine> {
        let count = self
            .rows
            .iter()
            .map(Vec::len)
            .chain([self.aligns.len()])
            .max()
            .unwrap_or_default();
        let mut widths = vec![0usize; count];
        for row in &self.rows {
            for (column, (cell, _)) in widths.iter_mut().zip(row) {
                *column = (*column).max(cells_width(cell));
            }
        }
        let numeric: Vec<bool> = (0..count)
            .map(|at| {
                let mut body = self
                    .rows
                    .iter()
                    .skip(1)
                    .filter_map(|row| row.get(at))
                    .map(|(cell, _)| cell.iter().map(|(ch, _)| ch).collect::<String>())
                    .filter(|text| !text.trim().is_empty())
                    .peekable();
                body.peek().is_some() && body.all(|text| is_number(&text))
            })
            .collect();
        shrink(&mut widths, width);
        let mut lines = Vec::new();
        for (at, row) in self.rows.iter().enumerate() {
            // The wrapped rows with their source indices, per column, so
            // each cell's link ranges map through its own wrapping.
            let wrapped: Vec<Vec<WrappedCell<'_>>> = widths
                .iter()
                .enumerate()
                .map(|(column, width)| {
                    let (cell, links) = row
                        .get(column)
                        .map(|(cell, links)| (cell.as_slice(), links.as_slice()))
                        .unwrap_or((&[][..], &[][..]));
                    let owned: Vec<Cell> = if at == 0 {
                        cell.iter()
                            .map(|&(ch, style)| (ch, style.add_modifier(Modifier::BOLD)))
                            .collect()
                    } else if numeric.get(column).copied().unwrap_or_default() {
                        // A number column's body cells are plain text.
                        cell.iter()
                            .map(|&(ch, _)| (ch, style(Role::Text)))
                            .collect()
                    } else {
                        cell.to_vec()
                    };
                    wrap_joined_indices(&owned, *width, *width, true)
                        .into_iter()
                        .map(|(row, _, indices)| (row, indices, links))
                        .collect()
                })
                .collect();
            let height = wrapped
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or_default()
                .max(1);
            for line in 0..height {
                let mut spans = Vec::new();
                let mut links_out: Vec<(Range<u16>, String)> = Vec::new();
                let mut col: usize = 0;
                for (column, width) in widths.iter().enumerate() {
                    if column > 0 {
                        spans.push(Span::raw(" ".repeat(GAP)));
                        col = col.saturating_add(GAP);
                    }
                    let (piece, indices, cell_links) = wrapped
                        .get(column)
                        .and_then(|rows| rows.get(line))
                        .map(|(row, indices, links)| (row.as_slice(), indices.as_slice(), *links))
                        .unwrap_or((&[][..], &[][..], &[][..]));
                    let pad = width.saturating_sub(cells_width(piece));
                    let align = if numeric.get(column).copied().unwrap_or_default() {
                        Alignment::Right
                    } else {
                        self.aligns.get(column).copied().unwrap_or(Alignment::None)
                    };
                    let before = match align {
                        Alignment::Right => pad,
                        Alignment::Center => pad / 2,
                        Alignment::Left | Alignment::None => 0,
                    };
                    spans.push(Span::raw(" ".repeat(before)));
                    col = col.saturating_add(before);
                    let piece_start = col;
                    // Each link of this cell that this visual line holds.
                    links_out.extend(link_cols(piece, indices, piece_start, cell_links));
                    spans.extend(spans_of(piece));
                    col = col.saturating_add(cells_width(piece));
                    spans.push(Span::raw(" ".repeat(pad.saturating_sub(before))));
                    col = col.saturating_add(pad.saturating_sub(before));
                }
                lines.push((Line::from(spans), links_out));
            }
            if at == 0 {
                // A rule under each column, breaking at the gaps.
                let mut rule = Vec::new();
                for (column, width) in widths.iter().enumerate() {
                    if column > 0 {
                        rule.push(Span::raw(" ".repeat(GAP)));
                    }
                    rule.push(Span::styled("─".repeat(*width), style(Role::Rule)));
                }
                lines.push((Line::from(rule), Vec::new()));
            }
        }
        lines
    }
}

/// Narrows the widest column by one, again and again, until the columns
/// and their gaps fit `width` or every column is at [`MIN_COLUMN`].
fn shrink(widths: &mut [usize], width: usize) {
    let gaps = GAP.saturating_mul(widths.len().saturating_sub(1));
    while widths.iter().sum::<usize>().saturating_add(gaps) > width {
        let widest = widths
            .iter_mut()
            .filter(|column| **column > MIN_COLUMN)
            .reduce(|widest, column| if *column > *widest { column } else { widest });
        match widest {
            Some(column) => *column = column.saturating_sub(1),
            None => return,
        }
    }
}

/// Whether a cell reads as a number: an optional sign, then digits with
/// `,`, `.` and `%`, and no unit.
fn is_number(text: &str) -> bool {
    let text = text.trim();
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    digits.chars().any(|ch| ch.is_ascii_digit())
        && digits
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, ',' | '.' | '%'))
}

/// The cells `cells` take on screen.
fn cells_width(cells: &[Cell]) -> usize {
    cells
        .iter()
        .map(|(ch, _)| super::wrap::char_width(*ch))
        .sum()
}
