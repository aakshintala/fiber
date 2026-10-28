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
use crate::ext::Bound;
use crate::format;
use crate::mouse::{Target, TargetId};
use crate::text::to_u16;
use crate::theme::{Role, style};

/// The overlay's preferred content width (`docs/tui.md`, "Overlays").
const PREFER: u16 = 71;

/// The seam notices on `app`'s frame draw through: home binds the home
/// page with no session, so Lua reads nil; the conversation binds its
/// open page, so entries drop with it.
pub(crate) fn frame_bound(app: &App) -> Bound {
    match app.session() {
        Some(session) => {
            let pages = app.pages();
            let index = pages.page_count().saturating_sub(1);
            pages.ext.bind(
                crate::ext::cache::PageKey {
                    session: session.clone(),
                    index,
                },
                crate::ext::SessionCtx {
                    session_id: Some(session.clone()),
                    halves: pages.halves_for(session),
                },
            )
        }
        None => app.pages().ext.bind(
            crate::ext::cache::PageKey::home(),
            crate::ext::SessionCtx {
                session_id: None,
                halves: None,
            },
        ),
    }
}

/// Draws the notice rows bottom-up from `bottom`, moving it up past
/// them: at most the rows above `area.y`, the lowest rows kept, so rows
/// past the top are dropped from the top. Nothing at width zero. A bound
/// `notice` renderer replaces a notice row whole; its notice and dismiss
/// targets stay on the replacement rows.
pub(super) fn draw(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
    bound: &Bound,
) {
    let width = usize::from(area.width);
    if width == 0 {
        return;
    }
    let lines = app.notices.visible_lines(bound, area.width);
    let room = bottom.saturating_sub(area.y) as usize;
    let skip = lines.len().saturating_sub(room);
    let mut noted: Vec<crate::app::SpanRow> = Vec::new();
    for line in lines.iter().skip(skip).rev() {
        match line {
            NoticeLine::Notice { id, code, text } => {
                if paint_replacement(
                    bound, *id, code, text, area, buf, targets, &mut noted, bottom,
                ) {
                    continue;
                }
                let Some(row) = bottom.checked_sub(1).filter(|row| *row >= area.y) else {
                    break;
                };
                let (drawn, copied) = notice_row_and_copy(code, text, width);
                buf.set_line(area.x, row, &drawn, area.width);
                noted.push(crate::app::SpanRow {
                    y: row,
                    x: area.x,
                    end: area.x.saturating_add(area.width),
                    text: copied,
                    join: crate::rows::Join::Break,
                });
                targets.push(Target {
                    id: TargetId::Notice(*id),
                    rect: Rect::new(area.x, row, area.width, 1),
                });
                targets.push(Target {
                    id: TargetId::DismissNotice(*id),
                    rect: Rect::new(area.right().saturating_sub(1), row, 1, 1),
                });
                *bottom = row;
            }
            NoticeLine::More(n) => {
                let Some(row) = bottom.checked_sub(1).filter(|row| *row >= area.y) else {
                    break;
                };
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
                    rect: Rect::new(area.x, row, to_u16(kept), 1),
                });
                noted.push(crate::app::SpanRow {
                    y: row,
                    x: area.x,
                    end: area.x.saturating_add(area.width),
                    text: crate::format::cut(&text, kept),
                    join: crate::rows::Join::Break,
                });
                *bottom = row;
            }
        }
    }
    // Bottom-up drew newest first: reading order is oldest first.
    noted.reverse();
    app.note_surface(crate::app::NotedSurface {
        surface: crate::app::SurfaceId::Notices,
        rows: noted,
    });
    super::marks::paint_selection(app, buf, crate::app::SurfaceId::Notices, area);
}

/// Paints the bound renderer's `notice` lines that fit above `area.y`,
/// keeping the notice target on each painted row and the dismiss target
/// on their last column: true when the replacement owns the notice. The
/// replacement owns its painted rows whole, ✕ cell included. A renderer
/// returning no lines hides the notice instead; the frame already dropped
/// those before the split, so an empty result here falls back to the
/// built-in row ([`Bound::notice_lines`]).
#[allow(clippy::too_many_arguments, reason = "one row's paint inputs")]
fn paint_replacement(
    bound: &Bound,
    id: usize,
    code: &str,
    text: &str,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    noted: &mut Vec<crate::app::SpanRow>,
    bottom: &mut u16,
) -> bool {
    let Some(fitted) = bound.notice_lines(id, code, text, area.width) else {
        return false;
    };
    if fitted.is_empty() {
        // An empty replacement falls back to the built-in row.
        return false;
    }
    let count = fitted
        .len()
        .min(usize::from(u16::MAX))
        .min(bottom.saturating_sub(area.y) as usize);
    if count == 0 {
        return true;
    }
    *bottom = bottom.saturating_sub(to_u16(count));
    for (dy, line) in fitted.iter().take(count).enumerate() {
        let row = bottom.saturating_add(to_u16(dy));
        buf.set_line(area.x, row, &line.line, area.width);
        targets.push(Target {
            id: TargetId::Notice(id),
            rect: Rect::new(area.x, row, area.width, 1),
        });
        noted.push(crate::app::SpanRow {
            y: row,
            x: area.x,
            end: area.x.saturating_add(area.width),
            text: line.line.to_string(),
            join: crate::rows::Join::Break,
        });
    }
    targets.push(Target {
        id: TargetId::DismissNotice(id),
        rect: Rect::new(area.right().saturating_sub(1), *bottom, 1, to_u16(count)),
    });
    true
}

/// Draws the notice overlay over `area` in the shared frame, hiding the
/// conversation's targets as before: one notice's whole text, or every
/// notice as `code · text` paragraphs, newest first. An emptied list
/// draws nothing. Returns the drawn slab, empty when nothing drew.
/// Hidden notices drop out of the entries, so the overlay never opens
/// one.
pub(super) fn overlay(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    bound: &Bound,
) -> Rect {
    let Some(entries) = app.notices.visible_overlay_entries(bound, area.width) else {
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
    let mut body: Vec<Row> = Vec::new();
    let mut joins: Vec<crate::rows::Join> = Vec::new();
    let title = if entries.all {
        if entries.entries.is_empty() {
            return Rect::default();
        }
        for (at, (code, text)) in entries.entries.iter().enumerate() {
            if at > 0 {
                body.push(blank_row());
                joins.push(crate::rows::Join::Break);
            }
            for (row, join) in paragraph(code, text, room) {
                body.push(row);
                joins.push(join);
            }
        }
        "Notices".to_owned()
    } else {
        let Some((code, text)) = entries.entries.first() else {
            return Rect::default();
        };
        for (row, join) in text_rows(text, room) {
            body.push(row);
            joins.push(join);
        }
        format!("Notice · {code}")
    };
    let framed = overlay::Overlay {
        title: Some((title, Some(Span::raw("✕")))),
        close: Some(TargetId::CloseOverlay),
        body,
        footer: Some(overlay::legend(&[("Esc", "closes")])),
        prefer: PREFER,
    };
    let slab = overlay::draw(buf, area, &framed, Place::Centre, targets);
    // The overlay's text rows read through the same kept rows the draw
    // kept: titles, the key-hint legend and blank gaps never copy.
    overlay::note_overlay(app, buf, slab, &framed, area, Place::Centre, &joins);
    slab
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
/// flush left. Each row carries its join, so the copy reads unwrapped.
fn paragraph(code: &str, text: &str, room: usize) -> Vec<(Row, crate::rows::Join)> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    wrap_joined_runs(
        &[(format!("{code} · "), dim), (text.to_owned(), Style::new())],
        room,
    )
    .into_iter()
    .map(|(spans, join)| {
        (
            Row {
                spans,
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            },
            join,
        )
    })
    .collect()
}

/// One notice's whole text as plain rows at most `room` columns wide,
/// each with its join.
fn text_rows(text: &str, room: usize) -> Vec<(Row, crate::rows::Join)> {
    wrap_joined_runs(&[(text.to_owned(), Style::new())], room)
        .into_iter()
        .map(|(spans, join)| {
            (
                Row {
                    spans,
                    right: Vec::new(),
                    targets: Vec::new(),
                    barred: false,
                },
                join,
            )
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
    wrap_joined_runs(runs, room)
        .into_iter()
        .map(|(spans, _)| spans)
        .collect()
}

/// `wrap_runs` with each row's join: how its text follows the row
/// before it, a new line, a word broken at the edge, or the one space
/// the wrap dropped (`docs/tui.md`, "Selection and copy"). The copy
/// joins wrapped rows by these, so the text reads unwrapped.
fn wrap_joined_runs(
    runs: &[(String, Style)],
    room: usize,
) -> Vec<(Vec<Span<'static>>, crate::rows::Join)> {
    use crate::rows::Join;
    let room = room.max(1);
    let mut rows: Vec<(Vec<Span<'static>>, Join)> = vec![(Vec::new(), Join::Break)];
    let mut taken = 0usize;
    // Pushes `word` (holding no space) in `style`, with a space before
    // it when the source holds one, breaking a word wider than a row
    // where it reaches the edge.
    let push_word = |word: &str,
                     style: Style,
                     space_before: bool,
                     rows: &mut Vec<(Vec<Span<'static>>, Join)>,
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
                let Some((row, _)) = rows.last_mut() else {
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
                // The wrap drops the space before the word: the join
                // remembers it, so the copy reads the space back.
                let join = if leading { Join::WrapSpace } else { Join::Wrap };
                rows.push((Vec::new(), join));
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
            let Some((row, _)) = rows.last_mut() else {
                return;
            };
            row.push(Span::styled(chunk.clone(), style));
            rest = &rest[chunk.len()..];
            if rest.is_empty() {
                *taken += format::width(&chunk);
            } else {
                rows.push((Vec::new(), Join::Wrap));
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
                rows.push((Vec::new(), Join::Break));
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
/// A notice's row `width` cells: "  ⚠ " and `code` in `attention`,
/// " · " and the message dim, cut with one "…" so the text ends before
/// the closing blank, and "✕" dim in the last column. A newline in the
/// message draws as a space.
#[cfg(test)]
fn notice_row(code: &str, text: &str, width: usize) -> Line<'static> {
    notice_row_and_copy(code, text, width).0
}

/// The built-in notice row's spans at `width`, for the seam's
/// decorator path: the drawn row, without the copy text.
pub(crate) fn builtin_spans(code: &str, text: &str, width: usize) -> Vec<Vec<crate::ext::RawSpan>> {
    let (line, _) = notice_row_and_copy(code, text, width);
    vec![crate::ext::spans::from_ratatui(&line.spans)]
}

/// A notice's drawn row with the text a copy reads: the head and the
/// tail as drawn, without the "…" a cut adds and without the closing
/// blank and ✕ (`docs/tui.md`, "Selection and copy"). Trailing pad
/// stays in the text; the copy drops trailing spaces on each row.
fn notice_row_and_copy(code: &str, text: &str, width: usize) -> (Line<'static>, String) {
    let text = text.replace('\n', " ");
    let head = format!("  ⚠ {code}");
    let tail = format!(" · {text}");
    // The cells before the closing blank and ✕.
    let room = width.saturating_sub(2);
    let head_width = format::width(&head);
    let tail_width = format::width(&tail);
    let (head, tail, copied) = if head_width + tail_width <= room {
        let fill = room - head_width - tail_width;
        let padded = format!("{tail}{}", " ".repeat(fill));
        let copied = format!("{head}{tail}");
        (head, padded, copied)
    } else if head_width < room {
        // The message is cut with one "…"; the code keeps its columns.
        // A wide glyph at the cut can leave the cut short of its
        // budget, so the pad fills it: the ✕ stays in the last column.
        let kept = format::cut(&tail, room - head_width - 1);
        let cut = format!("{kept}…");
        let fill = room - head_width - format::width(&cut);
        let copied = format!("{head}{kept}");
        (head, format!("{cut}{}", " ".repeat(fill)), copied)
    } else {
        // Even the code does not fit: it is cut with one "…", padded
        // the same way.
        let kept = if room > 0 {
            let cut = format!("{}…", format::cut(&head, room - 1));
            format!("{cut}{}", " ".repeat(room - format::width(&cut)))
        } else {
            String::new()
        };
        let copied = format::cut(&head, room.saturating_sub(1));
        (kept, String::new(), copied)
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
    (Line::from(spans), copied)
}

#[cfg(test)]
#[path = "notice_rows_tests.rs"]
mod tests;
