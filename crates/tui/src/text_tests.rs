//! Tests for the text helpers.

use super::{char_width, controls_to_space, cut, paste_clean, printable, strip_controls, width};

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
}
