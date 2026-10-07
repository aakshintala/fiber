//! GFM tables: a bold header row, a rule under it, columns two spaces
//! apart, numbers right-aligned, and a table wider than the area shrunk
//! widest column first (`docs/tui.md`, "Look").

use pulldown_cmark::Alignment;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::{Cell, Role, char_width, spans_of, style, wrap_cells};

/// Cells between columns.
const GAP: usize = 2;
/// A column shrinks no narrower than this.
const MIN_COLUMN: usize = 6;

/// A table being gathered: the alignment row, finished rows (the header
/// first) and the row in progress.
pub(super) struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Vec<Cell>>>,
    row: Vec<Vec<Cell>>,
}

impl Table {
    pub(super) fn new(aligns: Vec<Alignment>) -> Self {
        Self {
            aligns,
            rows: Vec::new(),
            row: Vec::new(),
        }
    }

    pub(super) fn push_cell(&mut self, cell: Vec<Cell>) {
        self.row.push(cell);
    }

    pub(super) fn end_row(&mut self) {
        self.rows.push(std::mem::take(&mut self.row));
    }

    /// The table's lines at `width`.
    pub(super) fn layout(self, width: usize) -> Vec<Line<'static>> {
        let count = self
            .rows
            .iter()
            .map(Vec::len)
            .chain([self.aligns.len()])
            .max()
            .unwrap_or_default();
        let mut widths = vec![0usize; count];
        for row in &self.rows {
            for (column, cell) in widths.iter_mut().zip(row) {
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
                    .map(|cell| cell.iter().map(|(ch, _)| ch).collect::<String>())
                    .filter(|text| !text.trim().is_empty())
                    .peekable();
                body.peek().is_some() && body.all(|text| is_number(&text))
            })
            .collect();
        shrink(&mut widths, width);
        let total = widths
            .iter()
            .sum::<usize>()
            .saturating_add(GAP.saturating_mul(count.saturating_sub(1)));
        let mut lines = Vec::new();
        for (at, row) in self.rows.iter().enumerate() {
            let wrapped: Vec<Vec<Vec<Cell>>> = widths
                .iter()
                .enumerate()
                .map(|(column, width)| {
                    let cell = row.get(column).map_or(&[][..], Vec::as_slice);
                    let cell: Vec<Cell> = if at == 0 {
                        cell.iter()
                            .map(|&(ch, style)| (ch, style.add_modifier(Modifier::BOLD)))
                            .collect()
                    } else {
                        cell.to_vec()
                    };
                    wrap_cells(&cell, *width, *width, true)
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
                for (column, width) in widths.iter().enumerate() {
                    if column > 0 {
                        spans.push(Span::raw(" ".repeat(GAP)));
                    }
                    let piece = wrapped
                        .get(column)
                        .and_then(|rows| rows.get(line))
                        .map_or(&[][..], Vec::as_slice);
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
                    spans.extend(spans_of(piece));
                    spans.push(Span::raw(" ".repeat(pad.saturating_sub(before))));
                }
                lines.push(Line::from(spans));
            }
            if at == 0 {
                lines.push(Line::from(Span::styled(
                    "─".repeat(total),
                    style(Role::Dim),
                )));
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
    cells.iter().map(|(ch, _)| char_width(*ch)).sum()
}
