//! Tests for a card's figures.

use super::{Kinds, count, cut, duration, heading, money, seconds, tokens, wrap};

#[test]
fn durations_truncate_to_whole_seconds_at_each_threshold() {
    assert_eq!(duration(0), "0s");
    assert_eq!(duration(59_500), "59s");
    assert_eq!(duration(59_999), "59s");
    assert_eq!(duration(60_000), "1m 00s");
    assert_eq!(duration(245_000), "4m 05s");
    assert_eq!(duration(3_599_999), "59m 59s");
    assert_eq!(duration(3_600_000), "1h 00m");
    assert_eq!(duration(3_720_000), "1h 02m");
    assert_eq!(duration(u64::MAX), "5124095576030h 25m");
}

#[test]
fn tokens_are_exact_under_a_thousand_then_one_decimal_rounded_half_up() {
    assert_eq!(tokens(1), "1 token");
    assert_eq!(tokens(812), "812 tokens");
    assert_eq!(tokens(999), "999 tokens");
    assert_eq!(tokens(1000), "1.0k tokens");
    assert_eq!(tokens(1049), "1.0k tokens");
    assert_eq!(tokens(1050), "1.1k tokens");
    assert_eq!(tokens(18_249), "18.2k tokens");
    assert_eq!(tokens(999_949), "999.9k tokens");
    assert_eq!(tokens(999_950), "1.0M tokens");
    assert_eq!(tokens(1_000_000), "1.0M tokens");
    assert_eq!(tokens(1_249_999), "1.2M tokens");
    assert_eq!(tokens(1_250_000), "1.3M tokens");
    assert_eq!(tokens(u64::MAX), "18446744073709.5M tokens");
}

#[test]
fn money_has_two_decimals_and_a_floor_for_tiny_amounts() {
    assert_eq!(money(0.41), "$0.41");
    assert_eq!(money(1.1), "$1.10");
    assert_eq!(money(0.004), "<$0.01");
    assert_eq!(money(0.005), "$0.01");
    assert_eq!(money(0.0), "$0.00");
    assert_eq!(money(12.345_67), "$12.35");
}

#[test]
fn one_takes_the_singular() {
    assert_eq!(count(0, "call", "calls"), "0 calls");
    assert_eq!(count(1, "call", "calls"), "1 call");
    assert_eq!(count(2, "call", "calls"), "2 calls");
}

#[test]
fn every_kind_in_order_capitalised() {
    let kinds = Kinds {
        read: 34,
        searched: 84,
        edited: 24,
        added: 175,
        removed: 83,
        ran: 9,
        other: 3,
        thoughts: 4,
    };
    assert_eq!(
        kinds.summary(),
        "Read 34 files, searched 84 patterns, edited 24 files +175 −83, ran 9 commands, \
         3 other calls, thought 4 times"
    );
}

#[test]
fn every_kind_singular() {
    let kinds = Kinds {
        read: 1,
        searched: 1,
        edited: 1,
        added: 3,
        removed: 1,
        ran: 1,
        other: 1,
        thoughts: 1,
    };
    assert_eq!(
        kinds.summary(),
        "Read 1 file, searched 1 pattern, edited 1 file +3 −1, ran 1 command, 1 other call, \
         thought once"
    );
}

#[test]
fn kinds_with_no_calls_are_left_out() {
    let only = |kinds: Kinds| kinds.summary();
    assert_eq!(only(Kinds::default()), "");
    assert_eq!(
        only(Kinds {
            searched: 2,
            ..Kinds::default()
        }),
        "Searched 2 patterns"
    );
    assert_eq!(
        only(Kinds {
            edited: 2,
            ..Kinds::default()
        }),
        "Edited 2 files"
    );
    assert_eq!(
        only(Kinds {
            edited: 1,
            removed: 4,
            ..Kinds::default()
        }),
        "Edited 1 file +0 −4"
    );
    assert_eq!(
        only(Kinds {
            edited: 1,
            added: 4,
            ..Kinds::default()
        }),
        "Edited 1 file +4 −0"
    );
    assert_eq!(
        only(Kinds {
            ran: 2,
            ..Kinds::default()
        }),
        "Ran 2 commands"
    );
    assert_eq!(
        only(Kinds {
            other: 2,
            ..Kinds::default()
        }),
        "2 other calls"
    );
    assert_eq!(
        only(Kinds {
            thoughts: 2,
            ..Kinds::default()
        }),
        "Thought twice"
    );
    assert_eq!(
        only(Kinds {
            thoughts: 3,
            ..Kinds::default()
        }),
        "Thought 3 times"
    );
}

#[test]
fn a_heading_is_a_markdown_heading_or_a_bold_line() {
    assert_eq!(
        heading("intro\n## Plan the fix\n**Check**", false).as_deref(),
        Some("Plan the fix")
    );
    assert_eq!(
        heading("intro\n**Check the tests**\n# Later", false).as_deref(),
        Some("Check the tests")
    );
    // Neither: the first non-empty line.
    assert_eq!(
        heading("\n  look at a.rs  \nthen b", false).as_deref(),
        Some("look at a.rs")
    );
    // A bold run that is not the whole line, and bare markers, are no
    // heading.
    assert_eq!(
        heading("**a** b\n####\n****", false).as_deref(),
        Some("**a** b")
    );
    assert_eq!(heading("", false), None);
    assert_eq!(heading("\n \n", false), None);
}

#[test]
fn the_latest_heading_is_the_last_one_so_far() {
    assert_eq!(
        heading("# One\ntext\n**Two**\nmore", true).as_deref(),
        Some("Two")
    );
    assert_eq!(heading("just text", true).as_deref(), Some("just text"));
    assert_eq!(heading("", true), None);
}

#[test]
fn wrapping_breaks_at_spaces_and_inside_long_words() {
    assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
    assert_eq!(wrap("one two", 7), vec!["one two"]);
    // The space between two words counts toward the row.
    assert_eq!(wrap("ab cd", 5), vec!["ab cd"]);
    assert_eq!(wrap("ab cd", 4), vec!["ab", "cd"]);
    assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    assert_eq!(wrap("a\nb", 10), vec!["a", "b"]);
    // A wide character takes two columns.
    assert_eq!(wrap("世界世界", 4), vec!["世界", "世界"]);
    assert_eq!(wrap("x", 0), vec!["x"]);
}

#[test]
fn a_span_under_a_second_is_left_out() {
    assert_eq!(seconds(999), None);
    assert_eq!(seconds(1000).as_deref(), Some("1s"));
}

#[test]
fn cutting_keeps_what_fits_in_columns() {
    assert_eq!(cut("abcdef", 4), "abcd");
    assert_eq!(cut("abc", 3), "abc");
    assert_eq!(cut("abc", 0), "");
    // A wide character that would cross the edge is left out.
    assert_eq!(cut("a界b", 2), "a");
    assert_eq!(cut("a界b", 3), "a界");
}

#[test]
fn wrap_joined_marks_word_and_character_breaks() {
    use crate::rows::Join::{Break, Wrap, WrapSpace};
    let cases = [
        (
            "a word break",
            "one two",
            4,
            vec![("one", Break), ("two", WrapSpace)],
        ),
        (
            "a long word",
            "abcdef",
            4,
            vec![("abcd", Break), ("ef", Wrap)],
        ),
        ("a newline", "a\nb", 10, vec![("a", Break), ("b", Break)]),
        (
            "an empty line",
            "a\n\nb",
            10,
            vec![("a", Break), ("", Break), ("b", Break)],
        ),
    ];
    for (name, text, max, expected) in cases {
        let expected: Vec<(String, crate::rows::Join)> = expected
            .into_iter()
            .map(|(row, join)| (row.to_owned(), join))
            .collect();
        assert_eq!(super::wrap_joined(text, max), expected, "{name}");
    }
}

#[test]
fn bubble_rows_skip_the_pad_and_join_their_continuations() {
    use crate::rows::Join::{Break, Wrap, WrapSpace};
    use crate::rows::Rows;
    // 70% of 14 is 9 columns, 7 inside the padding.
    let mut out = Rows::default();
    super::bubble("hello world abcdefghij", 14, &mut out);
    let (rows, texts) = out.into_parts();
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(shown, [" hello   ", " world   ", " abcdefg ", " hij     "]);
    let joins: Vec<_> = texts.iter().map(|text| (text.join, text.skip)).collect();
    assert_eq!(
        joins,
        [(Break, 1), (WrapSpace, 1), (WrapSpace, 1), (Wrap, 1)]
    );
}
