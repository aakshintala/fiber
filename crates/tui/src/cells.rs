//! Where each grapheme of a drawn line lands on screen (`docs/tui.md`,
//! "Selection and copy": a selection is screen cells, its copy is the text
//! under them). A line wider than the width wraps the way
//! [`crate::view::paragraph`] draws it, so the cells come from drawing it.

use std::ops::Range;

use ratatui::buffer::{Buffer, Cell, CellWidth};
use ratatui::layout::{Alignment, Rect};
use ratatui::text::Line;
use ratatui::widgets::Widget;

/// One drawn grapheme of `line.to_string()`: its bytes, the row of the line
/// it draws on, counted from the line's first, its column and its cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placed {
    pub(crate) bytes: Range<usize>,
    pub(crate) row: u16,
    pub(crate) col: u16,
    pub(crate) width: u16,
}

/// What a cell no grapheme was drawn into holds while placing: a control
/// character, which a drawn line never holds.
const SENTINEL: &str = "\u{80}";

/// Where each drawn grapheme of `line` lands at `width`, in reading order:
/// `(row, col)` and the byte ranges strictly increase. A grapheme drawn
/// into no cell (a space dropped at a wrap, a zero-width one) has no entry.
pub(crate) fn place(line: &Line<'_>, width: u16) -> Vec<Placed> {
    let mut steps = 0usize;
    layout(line, width, &mut steps)
}

/// [`place`], how many steps matching cells to graphemes took, and how
/// many scratch cells it matched against (tests only: the match ends
/// within `cells + graphemes` steps).
#[cfg(test)]
pub(crate) fn place_steps(line: &Line<'_>, width: u16) -> (Vec<Placed>, usize, usize) {
    let mut steps = 0usize;
    let placed = layout(line, width, &mut steps);
    let rows = crate::view::rows(line.clone(), width);
    (
        placed,
        steps,
        rows.saturating_mul(usize::from(scratch_width(width))),
    )
}

/// `line`'s graphemes as drawn, each with its bytes in `line.to_string()`.
fn graphemes<'a>(line: &'a Line<'_>) -> Vec<(Range<usize>, &'a str)> {
    let mut out = Vec::new();
    let mut base = 0usize;
    for span in &line.spans {
        let content: &str = span.content.as_ref();
        let start = content.as_ptr().addr();
        for grapheme in span.styled_graphemes(ratatui::style::Style::default()) {
            let at = base.saturating_add(grapheme.symbol.as_ptr().addr().saturating_sub(start));
            out.push((
                at..at.saturating_add(grapheme.symbol.len()),
                grapheme.symbol,
            ));
        }
        base = base.saturating_add(content.len());
    }
    out
}

/// The scratch buffer's width for a line drawn `width` wide.
fn scratch_width(width: u16) -> u16 {
    width.saturating_mul(2).saturating_add(4)
}

fn layout(line: &Line<'_>, width: u16, steps: &mut usize) -> Vec<Placed> {
    let graphemes = graphemes(line);
    if width == 0 {
        return Vec::new();
    }
    if line.width() <= usize::from(width) {
        // It fits: laid out directly, after the offset its alignment gives,
        // as the paragraph draws it.
        let drawn: u16 = graphemes
            .iter()
            .map(|(_, symbol)| symbol.cell_width())
            .fold(0, u16::saturating_add);
        let mut col = match line.alignment {
            Some(Alignment::Right) => width.saturating_sub(drawn),
            Some(Alignment::Center) => (width / 2).saturating_sub(drawn / 2),
            Some(Alignment::Left) | None => 0,
        };
        let mut out = Vec::with_capacity(graphemes.len());
        for (bytes, symbol) in graphemes {
            let cells = symbol.cell_width();
            if cells == 0 {
                continue;
            }
            out.push(Placed {
                bytes,
                row: 0,
                col,
                width: cells,
            });
            col = col.saturating_add(cells);
        }
        return out;
    }
    let rows = crate::view::rows(line.clone(), width);
    let area = Rect::new(0, 0, width, u16::try_from(rows).unwrap_or(u16::MAX));
    // The paragraph can draw a wide grapheme, and what follows it on its
    // row, past the area's right edge; the scratch buffer has room for
    // that, and what lands outside the area has no entry.
    let stride = scratch_width(width);
    let mut buf = Buffer::filled(
        Rect {
            width: stride,
            ..area
        },
        Cell::new(SENTINEL),
    );
    crate::view::paragraph(line.clone()).render(area, &mut buf);
    let cells = buf.content();
    let stride = usize::from(stride);
    let mut out = Vec::new();
    // Two cursors that only move forward: each step moves cell `i`, or
    // grapheme `g`, or both, so the loop ends within `cells + graphemes`
    // steps and an entry is never repeated or out of order.
    let (mut i, mut g) = (0usize, 0usize);
    while let (Some(cell), Some((bytes, symbol))) = (cells.get(i), graphemes.get(g)) {
        *steps = steps.saturating_add(1);
        let drawn = cell.symbol();
        if drawn == SENTINEL {
            // Nothing drawn here, or the second cell of a wide grapheme.
            i = i.saturating_add(1);
        } else if drawn == *symbol {
            let col = u16::try_from(i % stride).unwrap_or(u16::MAX);
            let cells = symbol.cell_width();
            if col.saturating_add(cells) <= width {
                out.push(Placed {
                    bytes: bytes.clone(),
                    row: u16::try_from(i / stride).unwrap_or(u16::MAX),
                    col,
                    width: cells,
                });
            }
            i = i.saturating_add(1);
            g = g.saturating_add(1);
        } else {
            // Not drawn where the cells stand: dropped at a wrap, or zero
            // width.
            g = g.saturating_add(1);
        }
    }
    out
}

#[cfg(test)]
#[path = "cells_tests.rs"]
mod tests;
