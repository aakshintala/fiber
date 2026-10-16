//! The Ctrl+R prompt-history panel on the shared overlay frame: the
//! typed query, how many match, and the matches with the query's first
//! hit marked (`docs/tui.md`, "The input box", "Overlays").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::completions::window;
use super::notice_rows::wrap_runs;
use crate::completion_rows::{Completions, Rows};
use crate::markdown::Role;
use crate::mouse::{Target, TargetId};
use crate::slash;
use crate::view::overlay::{self, Place, Row};

/// The panel's preferred content width (`docs/tui.md`, "Overlays").
const PREFER: u16 = 71;

/// One dim body row.
fn dim_row(text: String) -> Row {
    Row {
        spans: vec![Span::styled(text, Style::new().add_modifier(Modifier::DIM))],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The query row: `› <query>█`, the query bold.
fn query_row(query: &str) -> Row {
    Row {
        spans: vec![
            Span::styled("› ".to_owned(), Style::new().fg(Role::Accent.color())),
            Span::styled(query.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
            Span::styled("█".to_owned(), Style::new().add_modifier(Modifier::DIM)),
        ],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The spans for `entry`: its first case-folded occurrence of `query`
/// bold, the rest plain. The frame bolds the focused row throughout.
fn entry_spans(entry: &str, query: &str) -> Vec<(String, Style)> {
    let plain = Style::new();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let Some(found) = slash::matched(entry, query) else {
        return vec![(entry.to_owned(), plain)];
    };
    vec![
        (entry[..found.start].to_owned(), plain),
        (entry[found.start..found.end].to_owned(), bold),
        (entry[found.end..].to_owned(), plain),
    ]
}

/// One match's rows at most `room` columns wide: the gutter, then the
/// entry with its hit marked. Wrapped matches hang under the gutter.
/// Every row of the focused match sits on the bar.
fn entry_rows(entry: &str, query: &str, focused: bool, room: usize) -> Vec<Row> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let gutter = if focused { "› " } else { "  " };
    // The gutter takes two columns; the text wraps in the rest.
    let text_room = room.saturating_sub(2).max(1);
    wrap_runs(&entry_spans(entry, query), text_room)
        .into_iter()
        .enumerate()
        .map(|(at, spans)| {
            let mut out = vec![Span::styled(
                if at == 0 { gutter } else { "  " }.to_owned(),
                if focused { bold } else { Style::new() },
            )];
            out.extend(spans);
            Row {
                spans: out,
                right: Vec::new(),
                targets: Vec::new(),
                barred: focused,
            }
        })
        .collect()
}

/// The overlay body for `completions` at `wrap` content columns with
/// `fit` entry rows: the query row, the dim count row, and the windowed
/// matches around the selection. With no match the dim "no matching
/// prompts" takes the count row's place and nothing bars.
fn body(completions: &Completions, wrap: usize, fit: usize) -> Vec<Row> {
    let mut body = vec![query_row(&completions.query)];
    if completions.total == 0 {
        body.push(dim_row("no matching prompts".to_owned()));
        return body;
    }
    let count = if completions.total == 1 {
        "1 match · newest first".to_owned()
    } else {
        format!("{} matches · newest first", completions.total)
    };
    body.push(dim_row(count));
    let lines = completions.lines();
    let entries = lines.get(1..).unwrap_or_default();
    if entries.is_empty() {
        return body;
    }
    // The selected row counts the query line, so the entry index is one
    // less; the window keeps the selection in view (`docs/tui.md`,
    // "The input box").
    let selected = completions.selected.unwrap_or(1).saturating_sub(1);
    let (at, take) = window(
        selected.min(entries.len().saturating_sub(1)),
        entries.len(),
        fit,
    );
    let focused = completions.selected.map(|row| row.saturating_sub(1));
    for (n, entry) in entries.iter().skip(at).take(take).enumerate() {
        body.extend(entry_rows(
            entry,
            &completions.query,
            Some(at + n) == focused,
            wrap,
        ));
    }
    body
}

/// Draws the Ctrl+R panel over `area`: the query, the count and the
/// matches in the shared frame, centred across and down the area.
/// Returns the slab rect.
pub(super) fn draw(
    completions: &Completions,
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
) -> Rect {
    if !matches!(completions.rows, Rows::Search(_)) {
        return Rect::new(area.x, area.y, 0, 0);
    }
    // The content wraps at the preferred width first: rows are built
    // once at what fits, and the frame shrinks to what they need. The
    // body holds what fits below the chrome: two edges, two pad rows,
    // the title and its gap, and the footer's gap and row.
    let wrap = usize::from(PREFER.saturating_sub(4))
        .min(usize::from(area.width.saturating_sub(4)))
        .max(1);
    let fit = usize::from(area.height.saturating_sub(8)).saturating_sub(2);
    let framed = overlay::Overlay {
        title: Some(("Prompt history".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::CloseOverlay),
        body: body(completions, wrap, fit),
        footer: Some(overlay::legend(&[("Enter", "recalls"), ("Esc", "closes")])),
        prefer: PREFER,
    };
    overlay::draw(buf, area, &framed, Place::Centre, targets)
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
