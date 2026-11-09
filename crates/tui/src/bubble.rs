//! The person's prompt as a tinted bubble on the right, at most 72% of
//! the width, with half-block edges and the prompt stripe
//! (`docs/tui.md`, "Turns", "Look").

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::format::{width, wrap_joined};
use crate::rows::{Join, RowText, Rows};
use crate::surface;
use crate::theme::Role;

/// The prompt as a tinted block on the right: `text` wrapped to fit the
/// bubble at `columns`, each row with the wrap's join, so a selection
/// copies the text unwrapped without its pads, edges or stripe.
pub(crate) fn rows(text: &str, columns: u16, out: &mut Rows) {
    if text.trim().is_empty() || columns == 0 {
        return;
    }
    if columns < 4 {
        narrow(text, columns, out);
        return;
    }
    // The bubble is 72% of the columns, at least 4: the left pad, the
    // text, the right pad and the stripe.
    let bubble = usize::from(columns).saturating_mul(72) / 100;
    let bubble = bubble.clamp(4, usize::from(columns));
    let inner = u16::try_from(bubble.saturating_sub(3)).unwrap_or(u16::MAX);
    let rendered = crate::markdown::render_prompt(text, inner);
    let wrapped: Vec<(String, RowText, Line<'static>)> = rendered
        .lines
        .iter()
        .zip(rendered.text.iter())
        .map(|(line, mark)| (line.to_string(), mark.clone(), line.clone()))
        .collect();
    let widest = wrapped
        .iter()
        .map(|(row, _, _)| width(row))
        .max()
        .unwrap_or(0);
    let block = (widest.saturating_add(3)).min(bubble);
    out.push_text(
        (
            surface::half_edge_row(block, Role::Prompt, true).right_aligned(),
            None,
        ),
        RowText {
            decoration: true,
            ..RowText::plain()
        },
    );
    let tint = Style::default().bg(Role::Prompt.color());
    for (row, mut mark, markdown) in wrapped {
        let pad = " ".repeat(
            widest
                .min(bubble.saturating_sub(3))
                .saturating_sub(width(&row)),
        );
        if width(&row) <= bubble.saturating_sub(3) {
            let mut spans = bubble_spans(&row, &pad, markdown, tint);
            spans.push(surface::stripe_cell(Role::Accent, Role::Prompt, true));
            let line = Line::from(spans).right_aligned();
            mark.skip = mark.skip.saturating_add(1);
            mark.tail = mark.tail.saturating_add(2);
            for (range, _) in &mut mark.links {
                range.start = range.start.saturating_add(1);
                range.end = range.end.saturating_add(1);
            }
            out.push_text((line, None), mark);
        } else {
            // A two-column glyph at a four-column bubble leaves no room
            // for the stripe: the pads stay, so the row keeps its columns.
            let spans = bubble_spans(&row, &pad, markdown, tint);
            mark.skip = mark.skip.saturating_add(1);
            mark.tail = mark.tail.saturating_add(1);
            for (range, _) in &mut mark.links {
                range.start = range.start.saturating_add(1);
                range.end = range.end.saturating_add(1);
            }
            out.push_text((Line::from(spans).right_aligned(), None), mark);
        }
    }
    out.push_text(
        (
            surface::half_edge_row(block, Role::Prompt, false).right_aligned(),
            None,
        ),
        RowText {
            decoration: true,
            ..RowText::plain()
        },
    );
}

/// The prompt text row between its tinted pads, retaining Markdown styles.
fn bubble_spans(row: &str, pad: &str, markdown: Line<'static>, tint: Style) -> Vec<Span<'static>> {
    if markdown
        .spans
        .iter()
        .all(|span| span.style == Style::default())
    {
        return vec![Span::styled(format!(" {row}{pad} "), tint)];
    }
    let mut spans = vec![Span::styled(" ", tint)];
    spans.extend(markdown.spans.into_iter().map(|span| {
        Span::styled(
            span.content.to_string(),
            span.style.bg(span.style.bg.unwrap_or(Role::Prompt.color())),
        )
    }));
    spans.push(Span::styled(format!("{pad} "), tint));
    spans
}

/// The prompt below four columns: its edges with no pads and no stripe,
/// each text row wrapped at `columns` and padded with tinted blanks to
/// it. A glyph wider than the columns is clipped to one tinted blank, so
/// no row is wider and no glyph splits.
fn narrow(text: &str, columns: u16, out: &mut Rows) {
    let max = usize::from(columns);
    out.push_text(
        (
            surface::half_edge_row(max, Role::Prompt, true).right_aligned(),
            None,
        ),
        RowText {
            decoration: true,
            ..RowText::plain()
        },
    );
    let tint = Style::default().bg(Role::Prompt.color());
    for (row, join) in wrap_joined(text, max) {
        let row = if width(&row) > max {
            " ".to_owned()
        } else {
            format!("{row}{}", " ".repeat(max.saturating_sub(width(&row))))
        };
        out.push_text(
            (Line::styled(row, tint).right_aligned(), None),
            RowText {
                join,
                ..RowText::plain()
            },
        );
    }
    out.push_text(
        (
            surface::half_edge_row(max, Role::Prompt, false).right_aligned(),
            None,
        ),
        RowText {
            decoration: true,
            ..RowText::plain()
        },
    );
}

#[cfg(test)]
#[path = "bubble_tests.rs"]
mod tests;
