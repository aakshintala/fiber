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
fn seventy_two_percent_of_the_width() {
    let text: String = std::iter::repeat_n('a', 200).collect();
    // Seventy-two percent, at least 4 columns: 124 gives 89, and 50
    // gives 36, neither 35 (70%) nor 37 (rounded up).
    for (columns, bubble) in [(5, 4), (50, 36), (124, 89)] {
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
    // Every text row of the 124-column bubble is 89 cells, with the
    // stripe at its last cell.
    let (shown, _) = bubbled(&text, 124);
    for row in shown.iter().skip(1).take(shown.len().saturating_sub(2)) {
        assert_eq!(width(row), 89);
        assert!(row.ends_with('▐'));
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
    // 72% of 14 is 10 columns, 7 inside the pads and the stripe.
    let (shown, texts) = bubbled("hello world abcdefghij", 14);
    assert_eq!(
        shown,
        [
            "▄▄▄▄▄▄▄▄▄▄",
            " hello   ▐",
            " world   ▐",
            " abcdefg ▐",
            " hij     ▐",
            "▀▀▀▀▀▀▀▀▀▀"
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

/// The bubble's rows as logical text: what a copy or a search reads.
fn copied(text: &str, columns: u16) -> Vec<String> {
    let mut out = Rows::default();
    rows(text, columns, &mut out);
    let (rows, texts) = out.into_parts();
    crate::logical::logical(&rows, &texts)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

#[test]
fn bubble_rows_carry_their_text_marks() {
    // A stripe row: one pad skipped, pad and stripe tailed.
    let (_, texts) = bubbled("hi", 80);
    assert_eq!(
        texts
            .iter()
            .map(|text| (text.join, text.skip, text.tail, text.decoration))
            .collect::<Vec<_>>(),
        [
            (Join::Break, 0, 0, true),
            (Join::Break, 1, 2, false),
            (Join::Break, 0, 0, true),
        ]
    );
    // A row that drops its stripe: one pad each side, no stripe.
    let (_, texts) = bubbled("界", 4);
    assert_eq!(
        texts
            .iter()
            .map(|text| (text.join, text.skip, text.tail, text.decoration))
            .collect::<Vec<_>>(),
        [
            (Join::Break, 0, 0, true),
            (Join::Break, 1, 1, false),
            (Join::Break, 0, 0, true),
        ]
    );
    // Narrow rows: edges decorate, text rows join as wrapped.
    let (_, texts) = bubbled("hi", 1);
    assert_eq!(
        texts
            .iter()
            .map(|text| (text.join, text.skip, text.tail, text.decoration))
            .collect::<Vec<_>>(),
        [
            (Join::Break, 0, 0, true),
            (Join::Break, 0, 0, false),
            (Join::Wrap, 0, 0, false),
            (Join::Break, 0, 0, true),
        ]
    );
}

#[test]
fn copies_hold_only_the_text() {
    // Edges, stripe and pads never reach copied text, on the wide path,
    // the dropped-stripe row, and the narrow path.
    assert_eq!(copied("hi", 80), ["hi"]);
    assert_eq!(
        copied("hello world abcdefghij", 14),
        ["hello world abcdefghij"]
    );
    assert_eq!(copied("界界", 4), ["界界"]);
    assert_eq!(copied("界", 4), ["界"]);
    assert_eq!(copied("hi", 3), ["hi"]);
    assert_eq!(copied("hi", 1), ["hi"]);
}

#[test]
fn no_color_keeps_the_prompt_bare_and_its_rows() {
    // The person's prompt bubble is the exception: its edges stay half
    // blocks, which the paint pass blanks, and its rows stay exactly as
    // with colour (`docs/tui.md`, "Look").
    // A stripe terminal, so the stripe draws in both modes.
    let ghost = |name: &str| match name {
        "NO_COLOR" => Some("1".to_owned()),
        "TERM_PROGRAM" => Some("ghostty".to_owned()),
        _ => None,
    };
    crate::surface::init(&ghost);
    for columns in [1, 2, 3, 4, 5, 80] {
        let (boxed, _) = bubbled("hi there", columns);
        crate::surface::init(&|name: &str| match name {
            "COLORTERM" => Some("truecolor".to_owned()),
            "TERM_PROGRAM" => Some("ghostty".to_owned()),
            _ => None,
        });
        let (coloured, _) = bubbled("hi there", columns);
        crate::surface::init(&ghost);
        assert_eq!(boxed, coloured, "columns {columns}");
        assert!(
            !boxed
                .iter()
                .any(|row| row.contains('┌') || row.contains('│')),
            "{boxed:?}"
        );
    }
    // Painted: the edges blank, the text stays, right-aligned.
    let (shown, _) = bubbled("hi", 80);
    assert_eq!(shown, ["▄▄▄▄▄", " hi ▐", "▀▀▀▀▀"]);
    let (look, notice) = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name: &str| {
        (name == "NO_COLOR").then(|| "1".to_owned())
    });
    assert_eq!(notice, None);
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 3));
    let mut out = Rows::default();
    rows("hi", 80, &mut out);
    let (lines, _) = out.into_parts();
    for (y, (line, _)) in lines.iter().enumerate() {
        buf.set_line(
            80 - u16::try_from(line.width()).expect("a width"),
            u16::try_from(y).expect("a row"),
            line,
            80,
        );
    }
    look.paint(&mut buf);
    let painted: Vec<String> = (0..3)
        .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    assert_eq!(painted[0].trim(), "");
    assert_eq!(painted[2].trim(), "");
    assert!(painted[1].contains("hi"), "{painted:?}");
    assert!(!painted[1].contains('│'), "{painted:?}");
}

/// The prompt input's text draws through the bubble's rows: the seam in
/// `images::bubble_rows` replaces this text with the extension's lines.
#[test]
fn the_prompt_text_draws_through_the_bubble() {
    use crate::turn::images::Prompt;
    let prompt = Prompt {
        text: "go".to_owned(),
        images: Vec::new(),
    };
    let input = crate::ext::Prompt {
        text: &prompt.text,
        images: Vec::new(),
        flat: false,
    };
    assert_eq!(input.text, "go");
    let mut out = Rows::default();
    rows(&prompt.text, 40, &mut out);
    let (shown, _) = out.into_parts();
    assert!(
        shown
            .iter()
            .any(|(line, _)| line.to_string().contains("go"))
    );
}
