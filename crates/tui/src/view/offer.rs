//! Drawing the repository offer into the conversation area (`docs/tui.md`,
//! "Approving what a repository ships", "Swapped views"), with its chips,
//! Send row and ✕ as click targets.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use super::paragraph;
use crate::mouse::{Target, TargetId};
use crate::offer::Row;
use crate::rows::Join;
use crate::text::to_u16;

/// Draws `rows` from wrapped row `top` into `area`, pushing a target over
/// each spot of a row shown. A `top` past the end shows the last screenful.
pub(super) fn render(
    rows: &[Row],
    top: usize,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) {
    let height = usize::from(area.height);
    let total: usize = rows.iter().map(|row| row.height(area.width)).sum();
    let top = top.min(total.saturating_sub(height));
    let end = top.saturating_add(height);
    let mut start = 0usize;
    let mut y = area.y;
    for row in rows {
        let next = start.saturating_add(row.height(area.width));
        let count = next.min(end).saturating_sub(start.max(top));
        if count > 0 {
            let rect = Rect::new(area.x, y, area.width, to_u16(count));
            if row.clip {
                buf.set_line(area.x, y, &row.line, area.width);
                for (from, to, spot) in &row.spots {
                    targets.push(Target {
                        id: TargetId::Offer(*spot),
                        rect: Rect::new(
                            area.x.saturating_add(*from),
                            y,
                            to.saturating_sub(*from),
                            1,
                        ),
                    });
                }
            } else {
                let skip = top.saturating_sub(start);
                paragraph(row.line.clone())
                    .scroll((to_u16(skip), 0))
                    .render(rect, buf);
            }
            y = y.saturating_add(to_u16(count));
        }
        if next >= end {
            break;
        }
        start = next;
    }
}

/// The offer's drawn rows as selection text at `area` with the `top`
/// wrapped rows scrolled off: one entry per drawn row, in reading
/// order, with its copy join (`docs/tui.md`, "Selection and copy").
/// The walk matches [`render`]'s, so the noted rows sit where the draw
/// put them; the title row never copies, as an overlay title never
/// does.
pub(super) fn selection_rows(rows: &[Row], top: usize, area: Rect) -> Vec<crate::app::SpanRow> {
    let width = area.width;
    let height = usize::from(area.height);
    let total: usize = rows.iter().map(|row| row.height(width)).sum();
    let top = top.min(total.saturating_sub(height));
    let end = top.saturating_add(height);
    let mut start = 0usize;
    let mut y = area.y;
    let mut noted = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let next = start.saturating_add(row.height(width));
        let count = next.min(end).saturating_sub(start.max(top));
        if count > 0 {
            // The title row names the view and carries its close hint;
            // only the body rows read as selection text.
            if at > 0 {
                let skip = top.saturating_sub(start);
                for (dy, (text, join)) in note_row(row, skip, count, width).into_iter().enumerate()
                {
                    noted.push(crate::app::SpanRow {
                        y: y.saturating_add(to_u16(dy)),
                        x: area.x,
                        end: area.right(),
                        text,
                        join,
                    });
                }
            }
            y = y.saturating_add(to_u16(count));
        }
        if next >= end {
            break;
        }
        start = next;
    }
    noted
}

/// Row `row`'s drawn texts at `width`, windowed to the `count` rows
/// past `skip`, each with its copy join.
fn note_row(row: &Row, skip: usize, count: usize, width: u16) -> Vec<(String, Join)> {
    if row.clip {
        let text: String = row
            .line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        return vec![(crate::format::cut(&text, usize::from(width)), Join::Break)];
    }
    wrapped_texts(&row.line, width)
        .into_iter()
        .skip(skip)
        .take(count)
        .collect()
}

/// The paragraph's drawn rows at `width`, each with its copy join:
/// rendered through the same [`paragraph`] the draw draws, so the
/// noted rows read what the draw put down. Trailing spaces never copy
/// (`docs/tui.md`, "Selection and copy"). The first row starts its
/// own line; a continuation joins with the space the wrap dropped,
/// and with nothing when the break fell inside a word or the space
/// rode into the row's own cells.
fn wrapped_texts(line: &Line<'static>, width: u16) -> Vec<(String, Join)> {
    if width == 0 {
        return Vec::new();
    }
    let height = crate::view::rows(line.clone(), width);
    if height == 0 {
        return Vec::new();
    }
    let scratch_area = Rect::new(0, 0, width, to_u16(height));
    let mut scratch = Buffer::empty(scratch_area);
    paragraph(line.clone()).render(scratch_area, &mut scratch);
    let mut raws = Vec::with_capacity(height);
    for row in 0..to_u16(height) {
        let mut text = String::new();
        for col in 0..width {
            if let Some(cell) = scratch.cell((col, row)) {
                text.push_str(cell.symbol());
            }
        }
        raws.push(text);
    }
    // The source with its line breaks out: the renderer drops a `\n`
    // inside a span, so the drawn rows read the joined text. Each
    // drawn row matches the source at the first offset past the
    // cursor, skipping only spaces the wrap dropped; anything else
    // means the wrapper changed under the note.
    let source: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
        .replace('\n', "");
    let mut out = Vec::with_capacity(height);
    let mut cursor = 0usize;
    for (at, raw) in raws.iter().enumerate() {
        let trimmed = raw.trim_end();
        let Some(relative) = source[cursor..].find(trimmed) else {
            // The wrapper changed under the note: never join across
            // rows the note cannot map.
            return raws
                .into_iter()
                .map(|raw| (raw.trim_end().to_owned(), Join::Break))
                .collect();
        };
        let start = cursor + relative;
        if source[cursor..start].chars().any(|cell| cell != ' ') {
            // The wrapper changed under the note: never join across
            // rows the note cannot map.
            return raws
                .into_iter()
                .map(|raw| (raw.trim_end().to_owned(), Join::Break))
                .collect();
        }
        let join = if at == 0 {
            Join::Break
        } else if start > cursor && !trimmed.starts_with(' ') {
            Join::WrapSpace
        } else {
            Join::Wrap
        };
        cursor = start + trimmed.len();
        out.push((trimmed.to_owned(), join));
    }
    // What the rows did not read must be spaces the trim dropped,
    // else the wrapper changed under the note.
    if !source[cursor..].chars().all(|cell| cell == ' ') {
        return raws
            .into_iter()
            .map(|raw| (raw.trim_end().to_owned(), Join::Break))
            .collect();
    }
    out
}

#[cfg(test)]
#[path = "offer_tests.rs"]
mod tests;
