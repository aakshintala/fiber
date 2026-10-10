//! The `/` and `@` completion panels: the match list in the overlay
//! frame above the input box (`docs/tui.md`, "Rules", "Look", "Overlays").
//! The Ctrl+R panel keeps its look and its reserved rows, drawn by the
//! input box; only the `/` and `@` kinds draw here.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::completion_rows::{Completions, Rows};
use crate::format::{cut, width as cells};
use crate::markdown::Role;
use crate::mouse::Target;
use crate::slash;
use crate::view::overlay::{self, Place, Row};

/// The panels wrap at this content width before shrinking to what the
/// wrapped rows need (`docs/tui.md`, "Look", "Overlays").
const PREFER: u16 = 96;

/// One dim body row.
fn dim_row(text: String) -> Row {
    Row {
        spans: vec![Span::styled(text, Style::new().add_modifier(Modifier::DIM))],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The range line for a list longer than the window: where the rows shown
/// sit, with what hides above in front once it has scrolled. `None` when
/// the whole list shows.
fn range_text(start: usize, shown: usize, total: usize) -> Option<String> {
    if total <= slash::SHOWN {
        return None;
    }
    let end = start.saturating_add(shown).min(total);
    let mut parts = Vec::new();
    if start > 0 {
        parts.push(format!("↑ {start} above"));
    }
    parts.push(format!("{}–{end} of {total}", start.saturating_add(1)));
    if end < total {
        parts.push(format!("↓ {} more", total.saturating_sub(end)));
    }
    Some(parts.join(" · "))
}

/// `text` cut to `room` columns with an ellipsis when cut.
fn dot_cut(text: &str, room: usize) -> String {
    if cells(text) <= room {
        return text.to_owned();
    }
    if room == 0 {
        return String::new();
    }
    format!("{}…", cut(text, room.saturating_sub(1)))
}

/// `path` cut from the left to `max` columns, so the file name stays:
/// from the first `/` whose remainder fits, else the last cells. Returns
/// the shown remainder and its first byte in `path`; the caller draws
/// the ellipsis in front while the remainder is not the whole path.
fn cut_left(path: &str, max: usize) -> (String, usize) {
    if cells(path) <= max {
        return (path.to_owned(), 0);
    }
    if let Some((at, _)) = path
        .match_indices('/')
        .find(|(at, _)| cells(&path[*at..]) < max)
    {
        return (path[at..].to_owned(), at);
    }
    let mut taken = String::new();
    let mut bytes: usize = 0;
    for ch in path.chars().rev() {
        let mut buf = [0u8; 4];
        if cells(&taken).saturating_add(cells(ch.encode_utf8(&mut buf))) > max.saturating_sub(1) {
            break;
        }
        bytes = bytes.saturating_add(ch.len_utf8());
        taken.insert(0, ch);
    }
    (taken, path.len().saturating_sub(bytes))
}

/// The match to bold in `path`: the occurrence in the file name when it
/// holds the query, else the first in the path, as the search ranks.
fn match_in(path: &str, query: &str) -> Option<Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.to_lowercase().contains(&query.to_lowercase()) {
        let at = path.len().saturating_sub(name.len());
        return slash::matched(name, query)
            .map(|found| at.saturating_add(found.start)..at.saturating_add(found.end));
    }
    slash::matched(path, query)
}

/// The spans for `shown`, a cut remainder of a path starting at byte
/// `at` and ending at byte `end`: the part of `found` still shown reads
/// bold in `accent`, the rest in `accent`.
fn path_spans(
    shown: &str,
    at: usize,
    found: Option<Range<usize>>,
    end: usize,
) -> Vec<Span<'static>> {
    let accent = Style::new().fg(Role::Accent.color());
    let bold = accent.add_modifier(Modifier::BOLD);
    let Some(found) = found else {
        return vec![Span::styled(shown.to_owned(), accent)];
    };
    let start = found.start.max(at).saturating_sub(at);
    let stop = found.end.min(end).saturating_sub(at);
    if start >= stop {
        return vec![Span::styled(shown.to_owned(), accent)];
    }
    vec![
        Span::styled(shown[..start].to_owned(), accent),
        Span::styled(shown[start..stop].to_owned(), bold),
        Span::styled(shown[stop..].to_owned(), accent),
    ]
}

/// The spans for a `/` name in a column `wide`: the first case-folded
/// occurrence of `query` bold, the rest in `accent`, padded so every
/// description starts together.
fn name_spans(name: &str, query: &str, wide: usize) -> Vec<Span<'static>> {
    let accent = Style::new().fg(Role::Accent.color());
    let bold = accent.add_modifier(Modifier::BOLD);
    let mut spans = match slash::matched(name, query) {
        Some(found) => vec![
            Span::styled(name[..found.start].to_owned(), accent),
            Span::styled(name[found.start..found.end].to_owned(), bold),
            Span::styled(name[found.end..].to_owned(), accent),
        ],
        None => vec![Span::styled(name.to_owned(), accent)],
    };
    let pad = wide.saturating_sub(cells(name));
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), accent));
    }
    spans
}

/// One `/` row: the gutter, the name in its column, the description dim
/// and cut so the tag always fits, the hint in `attention`, and the tag
/// dim at the right end. The focused row sits on the bar.
fn slash_row(row: &slash::Row, query: &str, name_w: usize, room: usize, focused: bool) -> Row {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans = vec![Span::styled(
        if focused { "› " } else { "  " }.to_owned(),
        if focused { bold } else { dim },
    )];
    spans.extend(name_spans(&row.name, query, name_w));
    spans.push(Span::raw(" ".to_owned()));
    let hint = row
        .hint
        .as_deref()
        .map_or_else(String::new, |hint| format!(" {hint}"));
    let fixed = 2 + name_w + 1 + cells(&hint) + 1 + cells(&row.tag);
    spans.push(Span::styled(
        dot_cut(&row.description, room.saturating_sub(fixed)),
        dim,
    ));
    spans.push(Span::styled(hint, Style::new().fg(Role::Attention.color())));
    Row {
        spans,
        right: vec![Span::styled(row.tag.clone(), dim)],
        targets: Vec::new(),
        barred: focused,
    }
}

/// One `@` row: the gutter and the path in `accent`, cut from the left
/// when long, with the match still shown bold. The focused row sits on
/// the bar.
fn file_row(path: &str, query: &str, room: usize, focused: bool) -> Row {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans = vec![Span::styled(
        if focused { "› " } else { "  " }.to_owned(),
        if focused { bold } else { dim },
    )];
    let found = match_in(path, query);
    let (shown, at) = cut_left(path, room.saturating_sub(2));
    if at > 0 {
        spans.push(Span::styled(
            "…".to_owned(),
            Style::new().fg(Role::Accent.color()),
        ));
    }
    let end = at.saturating_add(shown.len());
    spans.extend(path_spans(&shown, at, found, end));
    Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: focused,
    }
}

/// The overlay body for `completions` at `room` content columns: the
/// windowed entries with the focused one barred, then the range line
/// while the list does not fit. A message kind is its one dim row with
/// no bar.
fn body(completions: &Completions, room: usize) -> Vec<Row> {
    let mut rows = match &completions.rows {
        Rows::Slash(entries) => {
            let name_w = entries
                .iter()
                .map(|row| cells(&row.name))
                .max()
                .unwrap_or(0);
            entries
                .iter()
                .enumerate()
                .map(|(at, row)| {
                    slash_row(
                        row,
                        &completions.query,
                        name_w,
                        room,
                        completions.selected == Some(at),
                    )
                })
                .collect()
        }
        Rows::Files(paths) => paths
            .iter()
            .enumerate()
            .map(|(at, path)| {
                file_row(
                    path,
                    &completions.query,
                    room,
                    completions.selected == Some(at),
                )
            })
            .collect(),
        Rows::Message(text) => vec![dim_row(text.clone())],
        Rows::Search(_) => Vec::new(),
    };
    if let Some(range) = range_text(completions.start, rows.len(), completions.total) {
        rows.push(dim_row(range));
    }
    rows
}

/// Draws the `/` and `@` panels over `area`, above the input box's top
/// edge `bottom`: the entries in the shared frame, centred across the
/// box's width, with the screen behind reading around the side margins.
/// Nothing for the Ctrl+R kind, which the input box draws in its
/// reserved rows. Returns the slab rect.
pub(super) fn draw(
    completions: &Completions,
    area: Rect,
    bottom: u16,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    if matches!(completions.rows, Rows::Search(_)) {
        return Rect::new(area.x, area.y, 0, 0);
    }
    // The content wraps at the preferred width first: rows are built
    // once at what fits, and the frame shrinks to what they need.
    let room = usize::from(PREFER.saturating_sub(4)).min(usize::from(area.width.saturating_sub(4)));
    let framed = overlay::Overlay {
        title: None,
        close: None,
        body: body(completions, room),
        footer: None,
        prefer: PREFER,
    };
    overlay::draw(buf, area, &framed, Place::Across { bottom }, targets)
}

#[cfg(test)]
#[path = "completions_tests.rs"]
mod tests;
