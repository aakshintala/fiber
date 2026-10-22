//! Tests for the text helpers.

use super::{controls_to_space, paste_clean, printable, strip_controls};

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
