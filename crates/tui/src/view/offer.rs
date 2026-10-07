//! Drawing the repository offer into the conversation area (`docs/tui.md`,
//! "Approving what a repository ships", "Swapped views"), with its chips,
//! Send row and ✕ as click targets.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use super::{paragraph, to_u16};
use crate::mouse::{Target, TargetId};
use crate::offer::Row;

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

#[cfg(test)]
#[path = "offer_tests.rs"]
mod tests;
