//! The person's prompt as a tinted bubble on the right, at most 70% of
//! the width, with half-block edges and the prompt stripe
//! (`docs/tui.md`, "Turns", "Look").

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::format::{width, wrap_joined};
use crate::rows::{RowText, Rows};
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
    // The bubble is 70% of the columns, at least 4: the left pad, the
    // text, the right pad and the stripe.
    let bubble = usize::from(columns).saturating_mul(7) / 10;
    let bubble = bubble.clamp(4, usize::from(columns));
    let wrapped = wrap_joined(text, bubble.saturating_sub(3));
    let widest = wrapped.iter().map(|(row, _)| width(row)).max().unwrap_or(0);
    let block = (widest.saturating_add(3)).min(bubble);
    out.push_text(
        (
            surface::edge_row(block, Role::Prompt, true).right_aligned(),
            None,
        ),
        RowText {
            decoration: true,
            ..RowText::plain()
        },
    );
    let tint = Style::default().bg(Role::Prompt.color());
    for (row, join) in wrapped {
        let pad = " ".repeat(
            widest
                .min(bubble.saturating_sub(3))
                .saturating_sub(width(&row)),
        );
        if width(&row) <= bubble.saturating_sub(3) {
            let span = Span::styled(format!(" {row}{pad} "), tint);
            let line = Line::from(vec![
                span,
                surface::stripe_cell(Role::Accent, Role::Prompt, true),
            ])
            .right_aligned();
            out.push_text(
                (line, None),
                RowText {
                    join,
                    skip: 1,
                    tail: 2,
                    ..RowText::plain()
                },
            );
        } else {
            // A two-column glyph at a four-column bubble leaves no room
            // for the stripe: the pads stay, so the row keeps its columns.
            let span = Span::styled(format!(" {row}{pad} "), tint);
            out.push_text(
                (Line::from(span).right_aligned(), None),
                RowText {
                    join,
                    skip: 1,
                    tail: 1,
                    ..RowText::plain()
                },
            );
        }
    }
    out.push_text(
        (
            surface::edge_row(block, Role::Prompt, false).right_aligned(),
            None,
        ),
        RowText {
            decoration: true,
            ..RowText::plain()
        },
    );
}

/// The prompt below four columns: no pads and no stripe, each row wrapped
/// at `columns` and padded with tinted blanks to it. A glyph wider than
/// the columns is clipped to one tinted blank, so no row is wider and no
/// glyph splits.
fn narrow(text: &str, columns: u16, out: &mut Rows) {
    let tint = Style::default().bg(Role::Prompt.color());
    for (row, join) in wrap_joined(text, usize::from(columns)) {
        let row = if width(&row) > usize::from(columns) {
            " ".to_owned()
        } else {
            format!(
                "{row}{}",
                " ".repeat(usize::from(columns).saturating_sub(width(&row)))
            )
        };
        out.push_text(
            (Line::styled(row, tint).right_aligned(), None),
            RowText {
                join,
                ..RowText::plain()
            },
        );
    }
}

#[cfg(test)]
#[path = "bubble_tests.rs"]
mod tests;
