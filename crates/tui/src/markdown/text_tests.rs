use ratatui::style::Style;

use crate::rows::Join::{self, Break, Wrap, WrapSpace};

/// Each rendered row's text and what it adds to its logical line.
fn rows(markdown: &str, width: u16) -> Vec<(String, Join, u16, bool)> {
    let rendered = super::super::render(markdown, width);
    assert_eq!(
        rendered.text.len(),
        rendered.lines.len(),
        "one row text per line"
    );
    rendered
        .lines
        .iter()
        .zip(&rendered.text)
        .map(|(line, text)| (line.to_string(), text.join, text.skip, text.decoration))
        .collect()
}

#[test]
fn each_row_reports_its_join_and_skip() {
    type Case<'a> = (&'a str, &'a str, u16, Vec<(&'a str, Join, u16, bool)>);
    let cases: Vec<Case<'_>> = vec![
        (
            "a paragraph wrapped at a space",
            "aaa bbb",
            5,
            vec![("aaa", Break, 0, false), ("bbb", WrapSpace, 0, false)],
        ),
        (
            "a word longer than the width",
            "abcdefgh",
            5,
            vec![("abcde", Break, 0, false), ("fgh", Wrap, 0, false)],
        ),
        (
            "a hard break",
            "aa  \nbb",
            20,
            vec![("aa", Break, 0, false), ("bb", Break, 0, false)],
        ),
        (
            "a list item's continuation skips the hang",
            "- aaa bbb",
            7,
            vec![("• aaa", Break, 0, false), ("  bbb", WrapSpace, 2, false)],
        ),
        (
            "a nested list",
            "- a\n  - bbb ccc",
            9,
            vec![
                ("• a", Break, 0, false),
                ("  • bbb", Break, 0, false),
                ("    ccc", WrapSpace, 4, false),
            ],
        ),
        (
            "a quote's continuation skips the bars and the hang",
            "> - aaa bbb",
            9,
            vec![
                ("│ • aaa", Break, 2, false),
                ("│   bbb", WrapSpace, 4, false),
            ],
        ),
        (
            "a code line wrapped skips its gutter; the header is decoration",
            "```\nabcdefghij\n```",
            10,
            vec![
                ("      copy", Break, 0, true),
                ("1 │ abcdef", Break, 4, false),
                ("  │ ghij  ", Wrap, 4, false),
            ],
        ),
        (
            "a blank separator",
            "aa\n\nbb",
            10,
            vec![
                ("aa", Break, 0, false),
                ("", Break, 0, false),
                ("bb", Break, 0, false),
            ],
        ),
    ];
    for (name, markdown, width, expected) in cases {
        let expected: Vec<(String, Join, u16, bool)> = expected
            .into_iter()
            .map(|(text, join, skip, decoration)| (text.to_owned(), join, skip, decoration))
            .collect();
        assert_eq!(rows(markdown, width), expected, "{name}");
    }
}

#[test]
fn a_table_row_is_a_break_with_no_skip() {
    let rows = rows("| a | b |\n|---|---|\n| c | d |", 20);
    assert!(rows.len() >= 3, "{rows:?}");
    for (text, join, skip, decoration) in rows {
        assert_eq!((join, skip, decoration), (Break, 0, false), "{text:?}");
    }
}

#[test]
fn wrapping_spaces_and_a_zero_width_character_ends() {
    let cells = |text: &str| -> Vec<(char, Style)> {
        text.chars().map(|ch| (ch, Style::default())).collect()
    };
    for text in ["     ", "\u{200b}", "a \u{200b} b", " \u{200b}\u{200b} "] {
        for width in 1..4 {
            let rows = super::super::wrap_joined(&cells(text), width, width, true);
            let kept: usize = rows.iter().map(|(row, _)| row.len()).sum();
            assert!(kept <= text.chars().count(), "{text:?} at {width}");
            assert!(rows.len() <= text.chars().count(), "{text:?} at {width}");
        }
    }
}
