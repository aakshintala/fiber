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

use super::overlay::{self, Place, Row};
use crate::app::App;
use crate::app::NoticeLine;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};

/// The overlay's preferred content width (`docs/tui.md`, "Overlays").
const PREFER: u16 = 71;

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

/// Draws the notice overlay over `area` in the shared frame, hiding the
/// conversation's targets as before: one notice's whole text, or every
/// notice as `code · text` paragraphs, newest first. An emptied list
/// draws nothing. Returns the drawn slab, empty when nothing drew.
pub(super) fn overlay(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
    let Some(entries) = app.notice_overlay_entries() else {
        return Rect::default();
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
    // The text wraps at the preferred width first and the overlay
    // shrinks to what the wrapped content needs (`docs/tui.md`,
    // "Overlays").
    let room = usize::from(PREFER.saturating_sub(4))
        .min(usize::from(area.width.saturating_sub(4)))
        .max(1);
    let mut body = Vec::new();
    let title = if entries.all {
        if entries.entries.is_empty() {
            return Rect::default();
        }
        for (at, (code, text)) in entries.entries.iter().enumerate() {
            if at > 0 {
                body.push(blank_row());
            }
            body.extend(paragraph(code, text, room));
        }
        "Notices".to_owned()
    } else {
        let Some((code, text)) = entries.entries.first() else {
            return Rect::default();
        };
        body.extend(text_rows(text, room));
        format!("Notice · {code}")
    };
    let framed = overlay::Overlay {
        title: Some((title, Some(Span::raw("✕")))),
        close: Some(TargetId::CloseOverlay),
        body,
        footer: Some(overlay::legend(&[("Esc", "closes")])),
        prefer: PREFER,
    };
    overlay::draw(buf, area, &framed, Place::Centre, targets)
}

/// One blank body row.
fn blank_row() -> Row {
    Row {
        spans: Vec::new(),
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// One notice as `code · text` rows at most `room` columns wide: the
/// code and its separator dim, the text in the normal colour, wrapped
/// flush left.
fn paragraph(code: &str, text: &str, room: usize) -> Vec<Row> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    wrap_runs(
        &[(format!("{code} · "), dim), (text.to_owned(), Style::new())],
        room,
    )
    .into_iter()
    .map(|spans| Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    })
    .collect()
}

/// One notice's whole text as plain rows at most `room` columns wide.
fn text_rows(text: &str, room: usize) -> Vec<Row> {
    wrap_runs(&[(text.to_owned(), Style::new())], room)
        .into_iter()
        .map(|spans| Row {
            spans,
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        })
        .collect()
}

/// Wraps styled `runs` at word boundaries into rows at most `room`
/// columns wide, keeping each piece's style across its wrapped rows: a
/// dim code stays dim, the text after it stays plain. Runs join with
/// nothing: a space reads only where the source holds one, so a marked
/// hit split across runs stays one word. A word wider than a row breaks
/// where it reaches the edge; a newline starts a row, as
/// `format::wrap` does; consecutive spaces read as one.
pub(super) fn wrap_runs(runs: &[(String, Style)], room: usize) -> Vec<Vec<Span<'static>>> {
    let room = room.max(1);
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut taken = 0usize;
    // Pushes `word` (holding no space) in `style`, with a space before
    // it when the source holds one, breaking a word wider than a row
    // where it reaches the edge.
    let push_word = |word: &str,
                     style: Style,
                     space_before: bool,
                     rows: &mut Vec<Vec<Span<'static>>>,
                     taken: &mut usize| {
        if word.is_empty() {
            return;
        }
        let mut rest = word;
        let mut leading = space_before;
        while !rest.is_empty() {
            let wide = format::width(rest);
            let gap = usize::from(leading);
            if *taken + gap + wide <= room {
                let Some(row) = rows.last_mut() else {
                    return;
                };
                if leading {
                    row.push(Span::styled(" ".to_owned(), style));
                }
                row.push(Span::styled(rest.to_owned(), style));
                *taken += gap + wide;
                return;
            }
            if *taken > 0 {
                rows.push(Vec::new());
                *taken = 0;
            }
            // At the row's start a word that fits takes the whole rest,
            // and a wider one breaks at the edge.
            let mut chunk = format::cut(rest, room);
            if chunk.is_empty() {
                // A single grapheme wider than the row still moves:
                // the frame cuts the overflow.
                let Some(first) = rest.chars().next() else {
                    return;
                };
                chunk.push(first);
            }
            let Some(row) = rows.last_mut() else {
                return;
            };
            row.push(Span::styled(chunk.clone(), style));
            rest = &rest[chunk.len()..];
            if rest.is_empty() {
                *taken += format::width(&chunk);
            } else {
                rows.push(Vec::new());
            }
            leading = false;
        }
    };
    // Whether the run before ended its text with a space, so a run
    // starting without one still follows it.
    let mut prev_ended_with_space = false;
    // Whether any word has been pushed: the paragraph's own leading
    // spaces drop, as a wrap does.
    let mut started = false;
    for (text, style) in runs {
        for (line_no, line) in text.split('\n').enumerate() {
            if line_no > 0 {
                rows.push(Vec::new());
                taken = 0;
            }
            for (word_no, word) in line.split(' ').enumerate() {
                let space_before = if word_no > 0 {
                    true
                } else if line_no == 0 {
                    (line.starts_with(' ') || prev_ended_with_space) && started
                } else {
                    false
                };
                if !word.is_empty() {
                    started = true;
                }
                push_word(word, *style, space_before, &mut rows, &mut taken);
            }
        }
        prev_ended_with_space = text.ends_with(' ');
    }
    rows
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
