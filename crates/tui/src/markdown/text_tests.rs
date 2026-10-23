use ratatui::style::Style;

use crate::rows::Join::{self, Break, Wrap, WrapSpace};

/// Each rendered row's text and what it adds to its logical line.
fn rows(markdown: &str, width: u16) -> Vec<(String, Join, u16, u16, bool)> {
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
        .map(|(line, text)| {
            (
                line.to_string(),
                text.join,
                text.skip,
                text.tail,
                text.decoration,
            )
        })
        .collect()
}

#[test]
fn each_row_reports_its_join_and_skip() {
    type Case<'a> = (&'a str, &'a str, u16, Vec<(&'a str, Join, u16, u16, bool)>);
    let cases: Vec<Case<'_>> = vec![
        (
            "a paragraph wrapped at a space",
            "aaa bbb",
            5,
            vec![("aaa", Break, 0, 0, false), ("bbb", WrapSpace, 0, 0, false)],
        ),
        (
            "a word longer than the width",
            "abcdefgh",
            5,
            vec![("abcde", Break, 0, 0, false), ("fgh", Wrap, 0, 0, false)],
        ),
        (
            "a hard break",
            "aa  \nbb",
            20,
            vec![("aa", Break, 0, 0, false), ("bb", Break, 0, 0, false)],
        ),
        (
            "a list item's continuation skips the hang",
            "- aaa bbb",
            7,
            vec![
                ("• aaa", Break, 0, 0, false),
                ("  bbb", WrapSpace, 2, 0, false),
            ],
        ),
        (
            "a nested list",
            "- a\n  - bbb ccc",
            9,
            vec![
                ("• a", Break, 0, 0, false),
                ("  • bbb", Break, 0, 0, false),
                ("    ccc", WrapSpace, 4, 0, false),
            ],
        ),
        (
            "a quote's continuation skips the bars and the hang",
            "> - aaa bbb",
            9,
            vec![
                ("│ • aaa", Break, 2, 0, false),
                ("│   bbb", WrapSpace, 4, 0, false),
            ],
        ),
        (
            "a code line wrapped skips its gutter; the header is decoration",
            "```\nabcdefghij\n```",
            10,
            vec![
                ("▄▄▄▄▄▄▄▄▄▄", Break, 0, 0, true),
                ("          ", Break, 0, 0, true),
                ("   1  abcd", Break, 6, 0, false),
                ("      efgh", Wrap, 6, 0, false),
                ("      ij  ", Wrap, 6, 0, false),
                ("▀▀▀▀▀▀▀▀▀▀", Break, 0, 0, true),
            ],
        ),
        (
            "a blank separator",
            "aa\n\nbb",
            10,
            vec![
                ("aa", Break, 0, 0, false),
                ("", Break, 0, 0, false),
                ("bb", Break, 0, 0, false),
            ],
        ),
    ];
    for (name, markdown, width, expected) in cases {
        let expected: Vec<(String, Join, u16, u16, bool)> = expected
            .into_iter()
            .map(|(text, join, skip, tail, decoration)| {
                (text.to_owned(), join, skip, tail, decoration)
            })
            .collect();
        assert_eq!(rows(markdown, width), expected, "{name}");
    }
}

#[test]
fn a_boxed_code_body_row_tails_its_side() {
    // Without colour the slab's own sides are decoration on both ends:
    // the leading side joins the gutter in `skip`, the trailing side in
    // `tail`, so the logical copy keeps the source bytes without the
    // border (`docs/tui.md`, "Look").
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    assert!(crate::surface::boxed());
    let rendered = super::super::render("```\nabcdefghij\n```", 10);
    let texts: Vec<(String, u16, u16, bool)> = rendered
        .lines
        .iter()
        .zip(&rendered.text)
        .map(|(line, text)| (line.to_string(), text.skip, text.tail, text.decoration))
        .collect();
    assert_eq!(
        texts,
        vec![
            ("┌────────┐".to_owned(), 0, 0, true),
            ("│        │".to_owned(), 0, 0, true),
            ("│   1  ab│".to_owned(), 7, 1, false),
            ("│      cd│".to_owned(), 7, 1, false),
            ("│      ef│".to_owned(), 7, 1, false),
            ("│      gh│".to_owned(), 7, 1, false),
            ("│      ij│".to_owned(), 7, 1, false),
            ("└────────┘".to_owned(), 0, 0, true),
        ]
    );
    // The logical text between the decoration is the source unwrapped.
    let mut out = String::new();
    for (at, (_, _, _, decoration)) in texts.iter().enumerate() {
        if *decoration {
            continue;
        }
        let (_, body) = crate::logical::line_text(&rendered.lines[at], &rendered.text[at]);
        if !out.is_empty() {
            out.push_str(match rendered.text[at].join {
                Join::WrapSpace => " ",
                Join::Wrap => "",
                Join::Break => "\n",
            });
        }
        out.push_str(&body);
    }
    assert_eq!(out, "abcdefghij");
}

#[test]
fn a_line_number_wider_than_four_columns_widens_its_gutter() {
    let block = |lines: usize| {
        let code: String = (1..=lines).map(|n| format!("l{n}\n")).collect();
        super::super::render(&format!("```\n{code}```"), 24)
    };
    // An edge row, the header, one row per code line, then the edge.
    let narrow = block(9_999);
    assert_eq!(narrow.text.len(), narrow.lines.len());
    assert_eq!(
        narrow.lines[2 + 9998].to_string(),
        format!("9999  l9999{}", " ".repeat(13))
    );
    assert_eq!(narrow.text[2 + 9998].skip, 6);
    // Past four digits every row in the block widens, keeping the
    // numbers aligned.
    let wide = block(10_000);
    assert_eq!(
        wide.lines[2 + 9998].to_string(),
        format!(" 9999  l9999{}", " ".repeat(12))
    );
    assert_eq!(
        wide.lines[2 + 9999].to_string(),
        format!("10000  l10000{}", " ".repeat(11))
    );
    assert_eq!(wide.text[2 + 9998].skip, 7);
    assert_eq!(wide.text[2 + 9999].skip, 7);
    let wider = block(100_000);
    assert_eq!(
        wider.lines[2 + 99999].to_string(),
        format!("100000  l100000{}", " ".repeat(9))
    );
    assert_eq!(wider.text[2 + 99999].skip, 8);
}

#[test]
fn a_table_row_is_a_break_with_no_skip() {
    let rows = rows("| a | b |\n|---|---|\n| c | d |", 20);
    assert!(rows.len() >= 3, "{rows:?}");
    for (text, join, skip, tail, decoration) in rows {
        assert_eq!(
            (join, skip, tail, decoration),
            (Break, 0, 0, false),
            "{text:?}"
        );
    }
}

#[test]
fn wrapping_spaces_and_a_zero_width_character_ends() {
    let cells = |text: &str| -> Vec<(char, Style)> {
        text.chars().map(|ch| (ch, Style::default())).collect()
    };
    for text in ["     ", "\u{200b}", "a \u{200b} b", " \u{200b}\u{200b} "] {
        for width in 1..4 {
            let rows = super::super::wrap_joined_indices(&cells(text), width, width, true);
            let kept: usize = rows.iter().map(|(row, _, _)| row.len()).sum();
            assert!(kept <= text.chars().count(), "{text:?} at {width}");
            assert!(rows.len() <= text.chars().count(), "{text:?} at {width}");
        }
    }
}

/// One row's link columns and destinations.
type RowLinks = Vec<(std::ops::Range<u16>, String)>;

/// Each rendered row's link columns and destinations.
fn links(markdown: &str, width: u16) -> Vec<(String, RowLinks)> {
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
        .map(|(line, text)| (line.to_string(), text.links.clone()))
        .collect()
}

#[test]
fn a_link_records_its_columns_on_each_row_it_wraps_onto() {
    let rows = links("[a long docs link here](http://example.com/a) and more", 16);
    assert!(rows.len() >= 2, "{rows:?}");
    let flat: RowLinks = rows.iter().flat_map(|(_, links)| links.clone()).collect();
    assert!(!flat.is_empty(), "{rows:?}");
    for (_, url) in &flat {
        assert_eq!(url, "http://example.com/a");
    }
    // The first row holds the link's start, the next its continuation.
    let (first_text, first_links) = rows.first().cloned().unwrap_or_default();
    let (_, next_links) = rows.get(1).cloned().unwrap_or_default();
    assert!(
        !first_links.is_empty() && !next_links.is_empty(),
        "{rows:?}"
    );
    assert!(first_text.starts_with("a long"), "{first_text:?}");
}

#[test]
fn a_relative_link_is_no_link() {
    for markdown in [
        "[docs](/relative/path)",
        "[docs](relative)",
        "[docs](#anchor)",
    ] {
        let rows = links(markdown, 40);
        for (text, links) in rows {
            assert!(links.is_empty(), "{markdown:?} in {text:?}");
        }
    }
}

#[test]
fn two_links_on_one_row() {
    let rows = links("[a](http://a.example) and [b](https://b.example/c)", 60);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (_, links) = rows.into_iter().next().unwrap_or_default();
    assert_eq!(links.len(), 2, "{links:?}");
    assert_eq!(links[0].1, "http://a.example");
    assert_eq!(links[1].1, "https://b.example/c");
    assert!(links[0].0.start < links[1].0.start);
}
