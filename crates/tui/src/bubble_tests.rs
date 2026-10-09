//! Tests for the prompt bubble: its edges and stripe, its widths and its
//! joins (`docs/tui.md`, "Turns", "Look").

use ratatui::layout::Alignment;
use ratatui::text::Line;

use super::rows;
use crate::format::width;
use crate::rows::{Join, RowText, Rows};
use crate::theme::Role;

/// The bubble's rows for `text` at `columns`: each line's string and its
/// text.
fn bubbled(text: &str, columns: u16) -> (Vec<String>, Vec<RowText>) {
    let mut out = Rows::default();
    rows(text, columns, &mut out);
    let (rows, texts) = out.into_parts();
    (
        rows.iter().map(|(line, _)| line.to_string()).collect(),
        texts,
    )
}

#[test]
fn bubble_has_edges_and_a_right_stripe() {
    let (shown, texts) = bubbled("hi", 80);
    assert_eq!(shown, ["▄▄▄▄▄", " hi ▐", "▀▀▀▀▀"]);
    assert_eq!(texts.len(), 3);
    assert!(texts[0].decoration);
    assert!(texts[2].decoration);
    assert_eq!(
        (texts[1].join, texts[1].skip, texts[1].tail),
        (Join::Break, 1, 2)
    );
    // One block, right-aligned, the edges as wide as the text row.
    let mut out = Rows::default();
    rows("hi", 80, &mut out);
    let (lines, _) = out.into_parts();
    for (line, _) in &lines {
        assert_eq!(line.alignment, Some(Alignment::Right));
        assert_eq!(width(&line.to_string()), 5);
    }
    let edge: Vec<&Line> = lines.iter().map(|(line, _)| line).collect();
    assert_eq!(
        edge[0].spans.first().map(|span| span.style.fg),
        Some(Some(Role::Prompt.color()))
    );
    let text = &edge[1].spans;
    assert_eq!(text.len(), 2);
    assert_eq!(text[1].content, "▐");
    assert_eq!(text[1].style.fg, Some(Role::Accent.color()));
    assert_eq!(text[1].style.bg, Some(Role::Prompt.color()));
}

#[test]
fn seventy_percent_from_width_six() {
    let text: String = std::iter::repeat_n('a', 100).collect();
    // Seventy percent, at least 4 columns.
    for (columns, bubble) in [(6, 4), (8, 5), (10, 7), (80, 56)] {
        let (shown, texts) = bubbled(&text, columns);
        assert!(!shown.is_empty(), "columns {columns}");
        for row in &shown {
            assert_eq!(width(row), bubble, "columns {columns}: {row:?}");
        }
        assert_eq!(shown.first().and_then(|row| row.chars().next()), Some('▄'));
        assert_eq!(shown.last().and_then(|row| row.chars().next()), Some('▀'));
        for row in shown.iter().skip(1).take(shown.len().saturating_sub(2)) {
            assert!(row.ends_with('▐'), "columns {columns}: {row:?}");
        }
        assert!(texts.first().is_some_and(|text| text.decoration));
        assert!(texts.last().is_some_and(|text| text.decoration));
    }
}

#[test]
fn bubble_at_narrow_widths() {
    assert!(bubbled("hi", 0).0.is_empty());
    assert!(bubbled("", 80).0.is_empty());
    assert!(bubbled("   ", 80).0.is_empty());
    // Below four columns the text draws alone between its edges, padded
    // to the width, with no stripe.
    assert_eq!(bubbled("hi", 1).0, ["▄", "h", "i", "▀"]);
    assert_eq!(bubbled("hi", 2).0, ["▄▄", "hi", "▀▀"]);
    assert_eq!(bubbled("hi", 3).0, ["▄▄▄", "hi ", "▀▀▀"]);
    // From four columns the 4-column bubble draws, at 5 and 6 too.
    for columns in [4, 5, 6] {
        assert_eq!(
            bubbled("hi", columns).0,
            ["▄▄▄▄", " h ▐", " i ▐", "▀▀▀▀"],
            "columns {columns}"
        );
    }
}

#[test]
fn wide_glyphs_at_narrow_widths() {
    // A row is never wider than its width, and no glyph splits: the
    // stripe goes first, then the pads, then the glyph clips to a blank.
    for text in ["界", "界界", "a界"] {
        for columns in 1..=6 {
            let (shown, _) = bubbled(text, columns);
            for row in &shown {
                assert!(
                    width(row) <= usize::from(columns),
                    "{text:?} at {columns}: {row:?}"
                );
            }
        }
    }
    assert_eq!(bubbled("界", 4).0, ["▄▄▄▄", " 界 ", "▀▀▀▀"]);
    assert_eq!(bubbled("界", 3).0, ["▄▄▄", "界 ", "▀▀▀"]);
    assert_eq!(bubbled("界", 1).0, ["▄", " ", "▀"]);
}

#[test]
fn continuation_rows_join_as_the_wrap_broke_them() {
    use Join::{Break, Wrap, WrapSpace};
    // 70% of 14 is 9 columns, 6 inside the pads and the stripe.
    let (shown, texts) = bubbled("hello world abcdefghij", 14);
    assert_eq!(
        shown,
        [
            "▄▄▄▄▄▄▄▄▄",
            " hello  ▐",
            " world  ▐",
            " abcdef ▐",
            " ghij   ▐",
            "▀▀▀▀▀▀▀▀▀"
        ]
    );
    let joins: Vec<_> = texts
        .iter()
        .map(|text| (text.join, text.skip, text.tail))
        .collect();
    assert_eq!(
        joins,
        [
            (Break, 0, 0),
            (Break, 1, 2),
            (WrapSpace, 1, 2),
            (WrapSpace, 1, 2),
            (Wrap, 1, 2),
            (Break, 0, 0),
        ]
    );
}
