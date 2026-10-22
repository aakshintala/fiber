//! Tests for the text helpers.

use super::{
    char_width, controls_to_space, cut, cut_left, fit, paste_clean, printable, strip_controls,
    width,
};

#[test]
fn paste_clean_turns_line_ends_into_newlines_and_drops_controls() {
    assert_eq!(paste_clean("a\r\nb\rc\u{1b}\td"), "a\nb\nc\td");
    // `\r\n` is one break and a lone `\r` another: swapping the two
    // replaces would give three.
    assert_eq!(paste_clean("\r\r\n"), "\n\n");
    assert_eq!(paste_clean("x\u{7f}y\u{85}z"), "xyz");
    assert_eq!(paste_clean(""), "");
    assert_eq!(paste_clean("a\tb\n"), "a\tb\n");
}

#[test]
fn paste_clean_is_idempotent() {
    let once = paste_clean("a\r\nb\rc\u{1b}\td\u{7f}");
    assert_eq!(paste_clean(&once), once);
}

#[test]
fn controls_to_space_turns_each_control_into_one_space() {
    assert_eq!(controls_to_space("a\nb\rc\td\x1b[2J"), "a b c d [2J");
    assert_eq!(controls_to_space("\r\n"), "  ");
    assert_eq!(controls_to_space("x\u{7f}y"), "x y");
    assert_eq!(controls_to_space(""), "");
}

#[test]
fn strip_controls_drops_every_control() {
    assert_eq!(strip_controls("a\x1bb\x07c\u{9b}d\u{7f}e\nf"), "abcdef");
    assert_eq!(strip_controls("\r\n"), "");
    assert_eq!(strip_controls("é 語"), "é 語");
}

#[test]
fn printable_is_false_for_c0_c1_and_del() {
    for ch in ['a', ' ', 'é'] {
        assert!(printable(ch), "{ch:?}");
    }
    for ch in ['\t', '\n', '\u{7f}', '\u{85}', '\u{1b}'] {
        assert!(!printable(ch), "{ch:?}");
    }
}

#[test]
fn width_counts_columns() {
    assert_eq!(width("abc"), 3);
    assert_eq!(width("語"), 2);
    assert_eq!(width(""), 0);
    // A control character takes one column, as `cut` and `wrap` count it.
    assert_eq!(width("\u{1b}"), 1);
}

#[test]
fn char_width_counts_columns() {
    assert_eq!(char_width('a'), 1);
    assert_eq!(char_width('語'), 2);
    assert_eq!(char_width('\u{301}'), 0);
}

#[test]
fn cut_without_an_ellipsis_keeps_what_fits() {
    assert_eq!(cut("abcdef", 3, ""), "abc");
    assert_eq!(cut("abc", 2, ""), "ab");
    // A wide grapheme that does not fit is dropped whole.
    assert_eq!(cut("語語", 3, ""), "語");
    assert_eq!(cut("語語", 1, ""), "");
    assert_eq!(cut("", 3, ""), "");
}

#[test]
fn cut_with_an_ellipsis_ends_in_it_only_when_it_cuts() {
    assert_eq!(cut("abcdef", 6, "…"), "abcdef");
    assert_eq!(cut("abcdefg", 6, "…"), "abcde…");
    assert_eq!(cut("abcdefg", 1, "…"), "…");
    assert_eq!(cut("abcdefg", 0, "…"), "");
    assert_eq!(cut("caf\u{e9}x", 5, "…"), "caf\u{e9}x");
    assert_eq!(cut("語語語", 4, "…"), "語…");
    assert_eq!(cut("", 0, "…"), "");
    assert_eq!(cut("abcdef", 7, "…"), "abcdef");
    assert_eq!(cut("caf\u{e9}xy", 4, "…"), "caf…");
    assert_eq!(cut("▣ a.png · 1×1", 40, "…"), "▣ a.png · 1×1");
    assert_eq!(cut("▣ a.png · 1×1", 0, "…"), "");
}

#[test]
fn cut_left_keeps_the_end_behind_an_ellipsis() {
    assert_eq!(cut_left("abcdef", 4), "…def");
    assert_eq!(cut_left("abc", 3), "abc");
    // A wide character that would cross the edge is left out.
    assert_eq!(cut_left("日本語", 4), "…語");
}

#[test]
fn fit_fills_exactly_the_columns() {
    assert_eq!(fit("ab", 5), "ab   ");
    assert_eq!(fit("abcdefg", 5), "abcd…");
    assert_eq!(fit("abcde", 5), "abcde");
    assert_eq!(fit("abcdefg", 0), "");
    assert_eq!(width(&fit(&"\u{3042}".repeat(10), 20)), 20);
    assert_eq!(fit(&"\u{3042}".repeat(10), 20), "\u{3042}".repeat(10));
    assert_eq!(
        fit(&"\u{3042}".repeat(11), 20),
        format!("{}… ", "\u{3042}".repeat(9))
    );
    assert_eq!(width(&fit(&"\u{3042}".repeat(11), 20)), 20);
    let combining = "e\u{301}".repeat(20);
    assert_eq!(width(&combining), 20);
    assert_eq!(fit(&combining, 20), combining);
    assert_eq!(width(&fit("\u{1F389}", 20)), 20);
}
