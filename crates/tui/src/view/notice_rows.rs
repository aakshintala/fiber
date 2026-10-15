//! The notice rows above the working line (`docs/tui.md`, "Notices").
//!
//! One row per notice, oldest first, then one "+N more" row for the
//! rest. Each notice row carries its registry code, and ✕ on its last
//! cell dismisses it. The rows are untinted.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::overlay_cross;
use crate::app::App;
use crate::app::NoticeLine;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};

/// Draws the notice rows bottom-up from `bottom`, moving it up past
/// them: at most the rows above `area.y`, the lowest rows kept, so rows
/// past the top are dropped from the top. Nothing at width zero.
pub(super) fn draw(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    let width = usize::from(area.width);
    if width == 0 {
        return;
    }
    let lines = app.notices();
    let room = bottom.saturating_sub(area.y) as usize;
    let skip = lines.len().saturating_sub(room);
    for line in lines.iter().skip(skip).rev() {
        let Some(row) = bottom.checked_sub(1).filter(|row| *row >= area.y) else {
            return;
        };
        match line {
            NoticeLine::Notice { id, code, text } => {
                buf.set_line(area.x, row, &notice_row(code, text, width), area.width);
                targets.push(Target {
                    id: TargetId::Notice(*id),
                    rect: Rect::new(area.x, row, area.width, 1),
                });
                targets.push(Target {
                    id: TargetId::DismissNotice(*id),
                    rect: Rect::new(area.right().saturating_sub(1), row, 1, 1),
                });
            }
            NoticeLine::More(n) => {
                let text = format!("  +{n} more");
                let kept = format::width(&text).min(width);
                buf.set_stringn(
                    area.x,
                    row,
                    &text,
                    kept,
                    Style::default().add_modifier(Modifier::DIM),
                );
                targets.push(Target {
                    id: TargetId::MoreNotices,
                    rect: Rect::new(area.x, row, super::to_u16(kept), 1),
                });
            }
        }
        *bottom = row;
    }
}

/// Draws the notice overlay over `area`, hiding the conversation's
/// targets as before.
pub(super) fn overlay(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let Some(texts) = app.notice_overlay() else {
        return;
    };
    targets.retain(|target| {
        !matches!(
            target.id,
            TargetId::Line(_)
                | TargetId::Link { .. }
                | TargetId::FindResult(_)
                | TargetId::NewBelow
                | TargetId::Turn(_)
                | TargetId::Offer(_)
        )
    });
    let rows: Vec<String> = texts
        .iter()
        .flat_map(|text| format::wrap(text, usize::from(area.width)))
        .collect();
    let mut y = area.y;
    for row in rows {
        if y >= area.bottom() {
            return;
        }
        let width = usize::from(area.width);
        let blank = " ".repeat(width);
        buf.set_stringn(area.x, y, blank, width, super::NOTICE_TINT);
        buf.set_stringn(area.x, y, &row, width, super::NOTICE_TINT);
        y = y.saturating_add(1);
    }
    overlay_cross(buf, area, targets);
}

/// A notice's row `width` cells: "  ⚠ " and `code` in `attention`,
/// " · " and the message dim, cut with one "…" so the text ends before
/// the closing blank, and "✕" dim in the last column. A newline in the
/// message draws as a space.
fn notice_row(code: &str, text: &str, width: usize) -> Line<'static> {
    let text = text.replace('\n', " ");
    let head = format!("  ⚠ {code}");
    let tail = format!(" · {text}");
    // The cells before the closing blank and ✕.
    let room = width.saturating_sub(2);
    let head_width = format::width(&head);
    let tail_width = format::width(&tail);
    let (head, tail) = if head_width + tail_width <= room {
        let fill = room - head_width - tail_width;
        (head, format!("{tail}{}", " ".repeat(fill)))
    } else if head_width < room {
        // The message is cut with one "…"; the code keeps its columns.
        // A wide glyph at the cut can leave the cut short of its
        // budget, so the pad fills it: the ✕ stays in the last column.
        let kept = format::cut(&tail, room - head_width - 1);
        let cut = format!("{kept}…");
        let fill = room - head_width - format::width(&cut);
        (head, format!("{cut}{}", " ".repeat(fill)))
    } else {
        // Even the code does not fit: it is cut with one "…", padded
        // the same way.
        let kept = if room > 0 {
            let cut = format!("{}…", format::cut(&head, room - 1));
            format!("{cut}{}", " ".repeat(room - format::width(&cut)))
        } else {
            String::new()
        };
        (kept, String::new())
    };
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut spans = vec![
        Span::styled(head, style(Role::Attention)),
        Span::styled(tail, dim),
    ];
    if width >= 2 {
        spans.push(Span::styled(" ".to_owned(), dim));
    }
    if width >= 1 {
        spans.push(Span::styled("✕".to_owned(), dim));
    }
    Line::from(spans)
}

#[cfg(test)]
#[path = "notice_rows_tests.rs"]
mod tests;
