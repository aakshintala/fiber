//! The rows a card draws, each with how its text joins the row before
//! (`docs/tui.md`, "Selection and copy": a selection copies the text
//! unwrapped). Producers push into [`Rows`] as they would into a
//! `Vec<Row>`; a plain push is a row that starts a line of its own.

use std::ops::{Deref, Range};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::Target;
use crate::surface::{Edges, edge_row};
use crate::theme::Role;
use crate::turn::Row;

/// How a row's text follows the row before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Join {
    /// A new logical line.
    Break,
    /// The same line, with nothing between: a word broken at the edge.
    Wrap,
    /// The same line, with the one space the wrap dropped between.
    WrapSpace,
}

/// What a drawn row adds to its logical line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowText {
    /// How it joins the row before.
    pub(crate) join: Join,
    /// The leading cells that are decoration, not text: quote bars, a hang
    /// indent, a code gutter, the bubble's pad.
    pub(crate) skip: u16,
    /// The trailing cells that are decoration, not text: the bubble's
    /// right pad and stripe.
    pub(crate) tail: u16,
    /// The whole row is decoration and adds no text: a code block's header.
    pub(crate) decoration: bool,
    /// The links drawn on the row: each one's cells in the line and its
    /// destination (`docs/tui.md`, "Links": links are handled on click).
    pub(crate) links: Vec<(Range<u16>, String)>,
    /// The collapsible sections the row is inside, outermost first: what
    /// a search draw opens, and what a match names (`docs/tui.md`,
    /// "Search").
    pub(crate) scopes: Vec<Target>,
    /// The column of the one cell that spins while its line runs, on the
    /// working line's tick (`docs/tui.md`, "The working line"); none in
    /// [`RowText::plain`].
    pub(crate) spin: Option<u16>,
}

impl RowText {
    /// A row that starts a line of its own, all of it text.
    pub(crate) fn plain() -> Self {
        Self {
            join: Join::Break,
            skip: 0,
            tail: 0,
            decoration: false,
            links: Vec::new(),
            scopes: Vec::new(),
            spin: None,
        }
    }
}

/// The rows drawn so far and, one each, their texts.
#[derive(Debug, Default)]
pub(crate) struct Rows {
    rows: Vec<Row>,
    texts: Vec<RowText>,
    /// The collapsible sections the next row lands inside, outermost
    /// first.
    scopes: Vec<Target>,
    /// A search draw: every collapsible body draws, open or not
    /// (`docs/tui.md`, "Search": every match is counted at once).
    all_open: bool,
}

impl Rows {
    /// Rows for a search draw: every collapsible body draws.
    pub(crate) fn all_open() -> Self {
        Self {
            all_open: true,
            ..Self::default()
        }
    }

    /// Enters what `target` opens: a collapsible body draws when `open`
    /// on a normal draw, and always on a search draw. Every row pushed
    /// until [`Rows::end_scope`] names `target` among its scopes,
    /// outermost first. A body with no target draws as it would closed:
    /// nothing could reveal a match there.
    pub(crate) fn open_scope(&mut self, target: Target, open: bool) -> bool {
        self.scopes.push(target);
        open || self.all_open
    }

    /// Leaves the innermost open section; nothing without one.
    pub(crate) fn end_scope(&mut self) {
        self.scopes.pop();
    }

    /// Adds a row that starts a line of its own.
    pub(crate) fn push(&mut self, row: Row) {
        self.push_text(row, RowText::plain());
    }

    /// Adds a row with what it adds to its logical line.
    pub(crate) fn push_text(&mut self, row: Row, mut text: RowText) {
        text.scopes = self.scopes.clone();
        self.rows.push(row);
        self.texts.push(text);
    }

    /// The rows and their texts, one each.
    pub(crate) fn into_parts(self) -> (Vec<Row>, Vec<RowText>) {
        (self.rows, self.texts)
    }

    /// Prefixes every row from `from` with `pad` blank cells, splitting
    /// any row wider than `inner` first (`docs/tui.md`, "Turns":
    /// nothing inside the card is indented past its one-column padding).
    /// A split breaks after the last space in its window, dropping that
    /// space, or cuts hard with no space there; no glyph is cut, and a
    /// glyph wider than `inner` draws as one blank cell. Each piece keeps
    /// its spans' styles and the line's style; the row's target stays on
    /// the first piece, while `skip`, `tail`, `links` and `spin` are
    /// redistributed by original cells and then shifted by the pad. The
    /// blank prefix takes the line's background, so call this before
    /// [`Rows::on_surface`], whose edge rows are never padded.
    pub(crate) fn pad(&mut self, from: usize, pad: u16, inner: u16) {
        let mut at = from;
        while at < self.rows.len() {
            let ((line, target), text) = (self.rows[at].clone(), self.texts[at].clone());
            let cells = cells_of(&line);
            let total: usize = cells.iter().map(|cell| cell.width).sum();
            let room = usize::from(inner);
            let plans = plan(&cells, room);
            if total <= room || room == 0 {
                prefix_row(&mut self.rows[at].0, &mut self.texts[at], pad);
                at += 1;
                continue;
            }
            debug_assert!(plans.len() >= 1);
            let mut pieces = Vec::with_capacity(plans.len());
            for (index, planned) in plans.iter().enumerate() {
                let (start, end) = (planned.start, planned.end);
                let start_cell: usize = cells[..start].iter().map(|cell| cell.width).sum();
                let end_cell: usize = start_cell
                    + cells[start..end]
                        .iter()
                        .map(|cell| cell.width)
                        .sum::<usize>();
                let piece = build_piece(&line, &cells, planned, inner);
                let mut piece_text = RowText {
                    join: if index == 0 { text.join } else { planned.join },
                    skip: text
                        .skip
                        .min(u16::try_from(end_cell).unwrap_or(u16::MAX))
                        .saturating_sub(u16::try_from(start_cell).unwrap_or(u16::MAX)),
                    tail: tail_in(&text, total, start_cell, end_cell),
                    decoration: text.decoration,
                    links: text
                        .links
                        .iter()
                        .filter_map(|(range, to)| {
                            let start = u16::try_from(start_cell).unwrap_or(u16::MAX);
                            let end = u16::try_from(end_cell).unwrap_or(u16::MAX);
                            let from = range.start.max(start);
                            let to_cell = range.end.min(end);
                            (from < to_cell).then(|| (from - start..to_cell - start, to.clone()))
                        })
                        .collect(),
                    scopes: text.scopes.clone(),
                    spin: text
                        .spin
                        .filter(|spin| {
                            let spin = usize::from(*spin);
                            spin >= start_cell && spin < end_cell
                        })
                        .map(|spin| {
                            spin.saturating_sub(u16::try_from(start_cell).unwrap_or(u16::MAX))
                        }),
                };
                let target = (index == 0).then(|| target.clone()).flatten();
                let mut row = (piece, target);
                prefix_row(&mut row.0, &mut piece_text, pad);
                pieces.push((row, piece_text));
            }
            let end = at + 1;
            self.rows
                .splice(at..end, pieces.iter().map(|(row, _)| row.clone()));
            self.texts
                .splice(at..end, pieces.iter().map(|(_, text)| text.clone()));
            at += pieces.len();
        }
    }

    /// Inserts one plain empty row at `at`, with the scopes current at
    /// the call (`docs/tui.md`, "Turns": blocks inside the card are
    /// separated by a blank row).
    pub(crate) fn gap(&mut self, at: usize) {
        let at = at.min(self.rows.len());
        self.rows.insert(at, (Line::default(), None));
        self.texts.insert(
            at,
            RowText {
                scopes: self.scopes.clone(),
                ..RowText::plain()
            },
        );
    }

    /// The rows from `from` on sit on `tint`: each takes it as its line's
    /// background; with `edges`, an `edge_row` (`width` cells,
    /// decoration) is inserted at `from` and pushed after the last. No
    /// rows from `from`: nothing (`docs/tui.md`, "Look").
    pub(crate) fn on_surface(&mut self, from: usize, width: u16, tint: Role, edges: Edges) {
        let Some(body) = self.rows.get_mut(from..) else {
            return;
        };
        if body.is_empty() {
            return;
        }
        for (line, _) in body {
            line.style.bg = Some(tint.color());
        }
        let width = usize::from(width);
        if edges.bottom {
            self.rows.push((edge_row(width, tint, false), None));
            self.texts.push(RowText {
                decoration: true,
                scopes: self.scopes.clone(),
                ..RowText::plain()
            });
        }
        if edges.top {
            self.rows.insert(from, (edge_row(width, tint, true), None));
            self.texts.insert(
                from,
                RowText {
                    decoration: true,
                    scopes: self.scopes.clone(),
                    ..RowText::plain()
                },
            );
        }
    }
}

/// How many pieces [`Rows::pad`] splits `line` into at `inner` (1 when
/// it fits): the one split rule both the draw and the row counts use
/// (`docs/tui.md`, "Turns").
pub(crate) fn fitted(line: &Line<'_>, inner: u16) -> usize {
    plan(&cells_of(line), usize::from(inner)).len().max(1)
}

/// One drawn glyph with its style.
struct Cell {
    ch: char,
    width: usize,
    style: Style,
}

/// One piece of a split row: its chars, and how it joins the row before.
struct Planned {
    start: usize,
    end: usize,
    join: Join,
}

/// `line` flattened to its glyphs with their widths and styles.
fn cells_of(line: &Line<'_>) -> Vec<Cell> {
    let mut cells = Vec::new();
    for span in &line.spans {
        for ch in span.content.chars() {
            let mut buf = [0u8; 4];
            cells.push(Cell {
                ch,
                width: crate::format::width(ch.encode_utf8(&mut buf)),
                style: span.style,
            });
        }
    }
    cells
}

/// Where `cells` breaks at `inner`: each piece holds at most `inner`
/// cells, breaking after the last space in its window (that space is
/// dropped and the next piece joins with it) or cutting hard with no
/// space there. No glyph is cut; a glyph wider than `inner` takes a
/// piece of its own. One plan when the row fits, none when it is empty.
fn plan(cells: &[Cell], inner: usize) -> Vec<Planned> {
    if inner == 0 {
        return Vec::new();
    }
    let mut plans = Vec::new();
    let mut at = 0;
    let mut join = Join::Break;
    while at < cells.len() {
        let mut end = at;
        let mut used = 0;
        while end < cells.len() && used + cells[end].width <= inner {
            used += cells[end].width;
            end += 1;
        }
        if end == at {
            // One glyph wider than the room: it takes a piece of its
            // own and draws as one blank cell.
            plans.push(Planned {
                start: at,
                end: at + 1,
                join: Join::Wrap,
            });
            at += 1;
            join = Join::Wrap;
            continue;
        }
        if end == cells.len() {
            plans.push(Planned {
                start: at,
                end,
                join,
            });
            break;
        }
        let space = cells[at..end]
            .iter()
            .rposition(|cell| cell.ch == ' ')
            .map(|found| found + at);
        match space {
            Some(found) if found > at => {
                plans.push(Planned {
                    start: at,
                    end: found,
                    join,
                });
                at = found + 1;
                join = Join::WrapSpace;
            }
            _ => {
                plans.push(Planned {
                    start: at,
                    end,
                    join,
                });
                at = end;
                join = Join::Wrap;
            }
        }
    }
    plans
}

/// The piece's line: its cells' text under their styles, keeping the
/// line's style and alignment. A glyph wider than `inner` draws as one
/// blank cell, so the piece fits the room.
fn build_piece(line: &Line<'_>, cells: &[Cell], planned: &Planned, inner: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current = (Style::default(), String::new());
    let mut started = false;
    for cell in &cells[planned.start..planned.end] {
        let (text, style) = if cell.width > usize::from(inner) {
            (" ".to_owned(), cell.style)
        } else {
            (cell.ch.to_string(), cell.style)
        };
        if !started || current.0 != style {
            if !current.1.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut current.1), current.0));
            }
            current.0 = style;
            started = true;
        }
        current.1.push_str(&text);
    }
    if !current.1.is_empty() {
        spans.push(Span::styled(std::mem::take(&mut current.1), current.0));
    }
    let mut piece = Line::from(spans);
    piece.style = line.style;
    piece.alignment = line.alignment;
    piece
}

/// The tail cells of the original tail run `[total - tail, total)` that
/// fall in `[start, end)`.
fn tail_in(text: &RowText, total: usize, start: usize, end: usize) -> u16 {
    let tail = usize::from(text.tail);
    let from = total.saturating_sub(tail).max(start);
    let to = total.min(end);
    u16::try_from(to.saturating_sub(from)).unwrap_or(u16::MAX)
}

/// Prefixes `line` with `pad` blank cells, which take the line's
/// background, and shifts the row's text marks past them.
fn prefix_row(line: &mut Line<'static>, text: &mut RowText, pad: u16) {
    if pad == 0 {
        return;
    }
    line.spans
        .insert(0, Span::raw(" ".repeat(usize::from(pad))));
    text.skip = text.skip.saturating_add(pad);
    for (range, _) in &mut text.links {
        range.start = range.start.saturating_add(pad);
        range.end = range.end.saturating_add(pad);
    }
    if let Some(spin) = text.spin {
        text.spin = Some(spin.saturating_add(pad));
    }
}

impl Deref for Rows {
    type Target = [Row];

    fn deref(&self) -> &[Row] {
        &self.rows
    }
}

impl Extend<Row> for Rows {
    fn extend<I: IntoIterator<Item = Row>>(&mut self, rows: I) {
        for row in rows {
            self.push(row);
        }
    }
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
