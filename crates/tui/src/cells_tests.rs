use ratatui::text::{Line, Span};

use super::*;

/// `line`'s placed graphemes as `(text, row, col, width)`.
fn placed(line: &Line<'_>, width: u16) -> Vec<(String, u16, u16, u16)> {
    let text = line.to_string();
    place(line, width)
        .into_iter()
        .map(|placed| {
            let grapheme = text.get(placed.bytes.clone()).unwrap_or("?").to_owned();
            (grapheme, placed.row, placed.col, placed.width)
        })
        .collect()
}

/// Expected entries from `(text, row, col, width)` tuples.
fn want(entries: &[(&str, u16, u16, u16)]) -> Vec<(String, u16, u16, u16)> {
    entries
        .iter()
        .map(|(text, row, col, width)| ((*text).to_owned(), *row, *col, *width))
        .collect()
}

/// The ordering invariant: `(row, col)` and the byte ranges strictly
/// increase and are disjoint, and every entry lies inside the drawn rows.
fn assert_ordered(line: &Line<'_>, width: u16, placed: &[Placed]) {
    let rows = crate::view::rows(line.clone(), width);
    for pair in placed.windows(2) {
        let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        assert!(
            (a.row, a.col) < (b.row, b.col),
            "{line:?} at {width}: {pair:?}"
        );
        assert!(
            a.bytes.end <= b.bytes.start,
            "{line:?} at {width}: {pair:?}"
        );
        assert!(a.bytes.start < a.bytes.end, "{line:?} at {width}: {pair:?}");
    }
    for entry in placed {
        assert!(
            usize::from(entry.row) < rows,
            "{line:?} at {width}: {entry:?}"
        );
        assert!(
            entry.col.saturating_add(entry.width) <= width,
            "{line:?} at {width}: {entry:?}"
        );
    }
}

#[test]
fn each_grapheme_lands_where_the_paragraph_draws_it() {
    type Case<'a> = (&'a str, Line<'static>, u16, Vec<(&'a str, u16, u16, u16)>);
    let cases: Vec<Case<'_>> = vec![
        (
            "a fitting left line",
            Line::raw("ab"),
            5,
            vec![("a", 0, 0, 1), ("b", 0, 1, 1)],
        ),
        (
            "a right-aligned bubble line",
            Line::raw("ab").right_aligned(),
            5,
            vec![("a", 0, 3, 1), ("b", 0, 4, 1)],
        ),
        (
            "a centred line",
            Line::raw("ab").centered(),
            6,
            vec![("a", 0, 2, 1), ("b", 0, 3, 1)],
        ),
        (
            "a line wrapped at a space: the space at the wrap has no entry",
            Line::raw("ab cd"),
            3,
            vec![
                ("a", 0, 0, 1),
                ("b", 0, 1, 1),
                ("c", 1, 0, 1),
                ("d", 1, 1, 1),
            ],
        ),
        (
            "a word longer than the width",
            Line::raw("abcde"),
            3,
            vec![
                ("a", 0, 0, 1),
                ("b", 0, 1, 1),
                ("c", 0, 2, 1),
                ("d", 1, 0, 1),
                ("e", 1, 1, 1),
            ],
        ),
        (
            "a wide character at the wrap edge goes to the next row",
            Line::raw("ab世c"),
            3,
            vec![
                ("a", 0, 0, 1),
                ("b", 0, 1, 1),
                ("世", 1, 0, 2),
                ("c", 1, 2, 1),
            ],
        ),
        (
            "a combining mark stays with its letter",
            Line::raw("e\u{301}x"),
            5,
            vec![("e\u{301}", 0, 0, 1), ("x", 0, 1, 1)],
        ),
        (
            "a ZWJ emoji is one grapheme",
            Line::raw("a👩\u{200d}💻b"),
            6,
            vec![("a", 0, 0, 1), ("👩\u{200d}💻", 0, 1, 2), ("b", 0, 3, 1)],
        ),
        ("an empty line", Line::raw(""), 4, vec![]),
        (
            "spans count their bytes in the whole line",
            Line::from(vec![Span::raw("│ "), Span::raw("ab")]),
            8,
            vec![
                ("│", 0, 0, 1),
                (" ", 0, 1, 1),
                ("a", 0, 2, 1),
                ("b", 0, 3, 1),
            ],
        ),
    ];
    for (name, line, width, expected) in cases {
        let got = placed(&line, width);
        assert_eq!(got, want(&expected), "{name}");
        assert_ordered(&line, width, &place(&line, width));
    }
}

#[test]
fn a_tab_draws_no_cell() {
    let line = Line::raw("a\tb");
    let got = placed(&line, 8);
    assert!(
        got.iter().all(|(text, ..)| text != "\t"),
        "a control character is never drawn: {got:?}"
    );
    assert_ordered(&line, 8, &place(&line, 8));
}

#[test]
fn runs_of_spaces_wrap_in_order_at_every_position() {
    let line = Line::raw("a   b   c  d");
    for width in 1..14 {
        let placed = place(&line, width);
        assert_ordered(&line, width, &placed);
        let letters: String = placed
            .iter()
            .filter_map(|entry| line.to_string().get(entry.bytes.clone()).map(str::to_owned))
            .filter(|text| text != " ")
            .collect();
        assert_eq!(letters, "abcd", "at {width}");
    }
}

#[test]
fn a_wide_grapheme_drawn_past_the_last_column_has_no_entry() {
    // The paragraph draws the wide grapheme from the last column on, past
    // the area's edge: it is outside the area, so it is not placed.
    let line = Line::raw("abc世");
    let got = placed(&line, 4);
    assert_eq!(got, want(&[("a", 0, 0, 1), ("b", 0, 1, 1), ("c", 0, 2, 1)]));
    assert_ordered(&line, 4, &place(&line, 4));
    // A following grapheme takes the wide one to the next row.
    let line = Line::raw("abc世d");
    assert_eq!(
        placed(&line, 4),
        want(&[
            ("a", 0, 0, 1),
            ("b", 0, 1, 1),
            ("c", 0, 2, 1),
            ("世", 1, 0, 2),
            ("d", 1, 2, 1)
        ])
    );
}

#[test]
fn a_zwj_family_wrapped_at_its_edge_keeps_its_order() {
    let family = "👨\u{200d}👩\u{200d}👧";
    let line = Line::raw(format!("abc{family}d"));
    for width in 2..7 {
        assert_ordered(&line, width, &place(&line, width));
    }
}

#[test]
fn a_combining_mark_after_a_wrap_stays_with_its_letter() {
    let line = Line::raw("abc e\u{301}f");
    let got = placed(&line, 3);
    assert!(got.contains(&("e\u{301}".to_owned(), 1, 0, 1)), "{got:?}");
    assert_ordered(&line, 3, &place(&line, 3));
}

/// A small seeded generator, so the property test is the same every run.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

#[test]
fn seeded_lines_keep_the_order_and_end_within_their_steps() {
    const PIECES: [&str; 8] = [
        " ",
        "  ",
        "word",
        "世界",
        "e\u{301}",
        "👩\u{200d}💻",
        "x",
        "\u{200b}",
    ];
    let mut seeded = Seeded(681);
    for _ in 0..500 {
        let len = seeded.next() % 12;
        let mut text = String::new();
        for _ in 0..len {
            let at = usize::try_from(seeded.next()).unwrap_or(0) % PIECES.len();
            text.push_str(PIECES.get(at).copied().unwrap_or(" "));
        }
        let line = Line::raw(text);
        for width in 1..=12 {
            let (placed, steps, cells) = place_steps(&line, width);
            assert_ordered(&line, width, &placed);
            let graphemes = line
                .styled_graphemes(ratatui::style::Style::default())
                .count();
            assert!(
                steps <= cells + graphemes,
                "{line:?} at {width}: {steps} steps"
            );
        }
    }
}
