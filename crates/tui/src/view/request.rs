//! Drawing the request panel in the input box's place (`docs/tui.md`,
//! "Approvals and questions", "A question form"): its lines, its tint, and
//! a form's tabs and rows as click targets.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::{ALERT_TINT, APPROVAL_TINT, paragraph, rows};
use crate::approvals::Panel;
use crate::mouse::{Target, TargetId};
use crate::surface;
use crate::text::to_u16;
use crate::theme::Role;

/// Panel line `line` with its styled runs: the marks' bytes in their
/// style, the rest plain. Wrapping measures the same spans, so the rows
/// drawn and the rows counted agree.
fn styled<'a>(panel: &'a Panel, line: usize, text: &'a str) -> Line<'a> {
    let mut runs: Vec<&crate::approvals::Mark> = panel
        .marks
        .iter()
        .filter(|mark| mark.line == line)
        .collect();
    runs.sort_by_key(|mark| mark.bytes.start);
    let mut spans = Vec::new();
    let mut at = 0;
    for run in runs {
        let from = run.bytes.start.max(at);
        if from > at {
            spans.push(Span::raw(text.get(at..from).unwrap_or_default()));
        }
        spans.push(Span::styled(
            text.get(run.bytes.clone()).unwrap_or_default(),
            run.style,
        ));
        at = run.bytes.end.max(at);
    }
    if at < text.len() {
        spans.push(Span::raw(text.get(at..).unwrap_or_default()));
    }
    Line::from(spans)
}

/// Where the panel's lines land in the rows above `bottom`.
struct Layout {
    /// Each line's wrapped rows.
    rows: Vec<usize>,
    /// The panel's rows scrolled off its top.
    skip: usize,
    /// The panel's top row on screen.
    top: u16,
    /// The rows it shows.
    shown: usize,
}

impl Layout {
    /// Lays `panel` out in `area` above `bottom`: one surface with its
    /// text inset two columns, the stripe's and the gap's, where they fit
    /// (`docs/tui.md`, "Look"). A panel taller than its rows keeps its
    /// top, except that a form scrolls by the fewest rows that keep its
    /// cursor line's last row shown.
    fn new(panel: &Panel, area: Rect, bottom: u16) -> Self {
        let width = surface::inset(area.width);
        let rows: Vec<usize> = panel
            .lines
            .iter()
            .enumerate()
            .map(|(line, text)| rows(styled(panel, line, text), width))
            .collect();
        let space = usize::from(bottom.saturating_sub(area.y));
        let total: usize = rows.iter().sum();
        let shown = total.min(space);
        let skip = panel.cursor.map_or(0, |cursor| {
            let end: usize = rows.iter().take(cursor.saturating_add(1)).sum();
            end.saturating_sub(space)
        });
        Self {
            rows,
            skip,
            top: bottom.saturating_sub(to_u16(shown)),
            shown,
        }
    }

    /// Line `line`'s rows on screen and how many of its own rows sit above
    /// them, scrolled off; `None` when none of its rows shows. The rect
    /// sits on the inset: past the stripe and the gap where they fit.
    /// Without colour the stripe's column is the box's left side and the
    /// right column is reserved, so the text sits one column in
    /// (`docs/tui.md`, "Look").
    fn place(&self, line: usize, area: Rect) -> Option<(Rect, usize)> {
        let start: usize = self.rows.iter().take(line).sum();
        let next = start.saturating_add(*self.rows.get(line)?);
        let end = self.skip.saturating_add(self.shown);
        let first = start.max(self.skip);
        let count = next
            .min(end)
            .checked_sub(first)
            .filter(|count| *count > 0)?;
        let y = self
            .top
            .saturating_add(to_u16(first.saturating_sub(self.skip)));
        let width = surface::inset(area.width);
        let x = if surface::box_fit(area.width) {
            area.x.saturating_add(1)
        } else {
            area.x.saturating_add(area.width.saturating_sub(width))
        };
        let rect = Rect::new(x, y, width, to_u16(count));
        Some((rect, first.saturating_sub(start)))
    }
}

/// The panel's text rows at the column's width: its lines wrapped at
/// the inset (`docs/tui.md`, "Look").
fn text_rows(panel: &Panel, width: u16) -> usize {
    panel
        .lines
        .iter()
        .enumerate()
        .map(|(line, text)| rows(styled(panel, line, text), surface::inset(width)))
        .sum()
}

/// The panel's rows at the column's width: its text rows with the edge
/// rows where they fit (`docs/tui.md`, "Look").
pub(crate) fn height(panel: &Panel, width: u16, room: usize) -> usize {
    surface::edged(text_rows(panel, width), room)
}

/// Whether the panel's edges draw: its text rows with an edge row above
/// and below fit the body `room` high (`docs/tui.md`, "Look").
fn edged(panel: &Panel, width: u16, room: usize) -> bool {
    let rows = text_rows(panel, width);
    surface::edged(rows, room) > rows
}

/// Draws `panel` on the rows above `bottom` in `area`: one surface in
/// its tint with edges where they fit, the stripe on an approval's rows,
/// pushing a target per spot shown; returns the row above its top edge,
/// so the conversation never draws over it
/// (`docs/tui.md`, "Approvals and questions", "Look").
pub(super) fn draw(
    panel: &Panel,
    area: Rect,
    bottom: u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> u16 {
    let room = usize::from(area.height);
    // The edges draw only when the rows with an edge above and below
    // fit; the text then sits one row above `bottom`, leaving the bottom
    // edge its row.
    let fits = edged(panel, area.width, room);
    let bottom = bottom.saturating_sub(u16::from(fits));
    let layout = Layout::new(panel, area, bottom);
    let tint = if panel.alert {
        Role::Alert
    } else {
        Role::Approval
    };
    let rect = Rect::new(area.x, layout.top, area.width, to_u16(layout.shown));
    // Without colour the panel is a thin box: ┌─┐ and └─┘ on the edge
    // rows and │ on the rect's columns, dim, with no tint and no stripe
    // (`docs/tui.md`, "Look"). The text keeps the narrowed rect the
    // layout places, so spots and the caret stay off the borders.
    if surface::box_fit(area.width) {
        surface::draw_box(
            buf,
            area,
            rect,
            surface::Edges {
                top: fits,
                bottom: fits,
            },
        );
    } else {
        buf.set_style(
            rect,
            if panel.alert {
                ALERT_TINT
            } else {
                APPROVAL_TINT
            },
        );
        // The stripe marks an approval in `attention` or `error`, and a
        // question form in `secondary` (`docs/tui.md`, "Look"). Too
        // narrow for a stripe, there is none.
        if area.width >= 3 {
            let stripe = if panel.cursor.is_some() {
                Role::Secondary
            } else if panel.alert {
                Role::Error
            } else {
                Role::Attention
            };
            surface::draw_stripe(buf, rect, stripe, tint, false);
        }
    }
    for (line, text) in panel.lines.iter().enumerate() {
        if let Some((rect, skip)) = layout.place(line, area) {
            paragraph(styled(panel, line, text))
                .scroll((to_u16(skip), 0))
                .render(rect, buf);
        }
    }
    if fits && !surface::box_fit(area.width) {
        surface::draw_edges(buf, rect, tint);
    }
    for spot in &panel.spots {
        let Some((rect, _)) = layout.place(spot.line, area) else {
            continue;
        };
        // A spot over columns sits on a line that fits one row.
        let rect = match spot.cols {
            None => rect,
            Some((from, to)) => Rect::new(
                rect.x.saturating_add(from),
                rect.y,
                to.saturating_sub(from),
                1,
            ),
        };
        targets.push(Target {
            id: TargetId::Form(spot.spot),
            rect,
        });
    }
    layout.top.saturating_sub(u16::from(fits))
}

/// Draws `panel` as `draw` does and notes its text rows for the
/// selection: each line's drawn rows with its text, wrapping joined
/// with nothing and lines broken, the marks' styles adding no text
/// (`docs/tui.md`, "Selection and copy"). Spots stay clickable: a drag
/// from one still selects.
pub(super) fn draw_noted(
    app: &crate::app::App,
    panel: &Panel,
    area: Rect,
    bottom: u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> u16 {
    let top = draw(panel, area, bottom, buf, targets);
    // The draw's layout recomputed: `Layout::new` is pure in the panel
    // and the area, so the noted rows sit where the draw put them.
    let room = usize::from(area.height);
    let fits = edged(panel, area.width, room);
    let layout = Layout::new(panel, area, bottom.saturating_sub(u16::from(fits)));
    let mut noted = Vec::new();
    for (line, text) in panel.lines.iter().enumerate() {
        let Some((rect, skip)) = layout.place(line, area) else {
            continue;
        };
        // The rows the paragraph draws, read from the line's own spans
        // through the shared placer: a selection is screen cells, its
        // copy the text under them (`docs/tui.md`, "Selection and
        // copy"). The scratch stays inside the placer; no buffer is
        // read back.
        let drawn = styled(panel, line, text);
        let source = drawn.to_string();
        let mut shown: Vec<(u16, String, usize, usize)> = Vec::new();
        for cell in crate::cells::place(&drawn, rect.width) {
            if cell.col >= rect.width {
                continue;
            }
            let piece = source.get(cell.bytes.clone()).unwrap_or_default();
            match shown.last_mut() {
                Some(last) if last.0 == cell.row => {
                    last.1.push_str(piece);
                    last.3 = cell.bytes.end;
                }
                _ => shown.push((cell.row, piece.to_owned(), cell.bytes.start, cell.bytes.end)),
            }
        }
        // The shown rows in reading order, blank ones included: a gap
        // holding a newline starts its own line, one holding the space
        // the wrap dropped reads it back, and a wrap inside a word
        // joins with nothing.
        let first = skip;
        let mut at = 0usize;
        for row_no in 0..rect.height {
            let prow = first.saturating_add(usize::from(row_no));
            while shown.get(at).is_some_and(|row| usize::from(row.0) < prow) {
                at = at.saturating_add(1);
            }
            let (text, join) = match shown.get(at) {
                Some(row) if usize::from(row.0) == prow => {
                    let gap = if prow == 0 {
                        ""
                    } else {
                        let before = shown
                            .iter()
                            .rev()
                            .find(|other| usize::from(other.0) < prow)
                            .map_or(0, |other| other.3);
                        source.get(before..row.2).unwrap_or_default()
                    };
                    at = at.saturating_add(1);
                    (
                        row.1.clone(),
                        if prow == 0 || gap.contains('\n') {
                            crate::rows::Join::Break
                        } else if gap.contains(' ') {
                            crate::rows::Join::WrapSpace
                        } else {
                            crate::rows::Join::Wrap
                        },
                    )
                }
                _ => (String::new(), crate::rows::Join::Break),
            };
            noted.push(crate::app::SpanRow {
                y: rect.y.saturating_add(row_no),
                x: rect.x,
                end: rect.x.saturating_add(rect.width),
                text,
                join,
            });
        }
    }
    app.note_surface(crate::app::NotedSurface {
        surface: crate::app::SurfaceId::Request,
        rows: noted,
    });
    for rect in app.selection_cells(crate::app::SurfaceId::Request, area) {
        buf.set_style(rect.intersection(area), super::marks::selection_style());
    }
    top
}

/// The text cursor's cell on the form's words row, while the panel draws
/// that row; the column stops at the inset's last one.
pub(super) fn caret(panel: &Panel, area: Rect, bottom: u16) -> Option<Position> {
    let (line, col) = panel.caret?;
    let room = usize::from(area.height);
    let bottom = bottom.saturating_sub(u16::from(edged(panel, area.width, room)));
    let (rect, _) = Layout::new(panel, area, bottom).place(line, area)?;
    let width = surface::inset(area.width);
    let col = col.min(width.saturating_sub(1));
    Some(Position::new(rect.x.saturating_add(col), rect.y))
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
