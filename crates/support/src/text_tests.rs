//! [`one_line`](super::one_line) blanks control characters.

use super::one_line;

#[test]
fn crlf_pairs_and_other_controls_become_one_space_each() {
    for (input, expected) in [
        ("a\r\nb", "a b"),
        ("a\rb", "a b"),
        ("a\nb", "a b"),
        ("a\tb", "a b"),
        ("\u{1b}[31m", " [31m"),
        ("plain", "plain"),
        ("x\n\ry", "x  y"),
        ("a\r\r\nb", "a  b"),
    ] {
        assert_eq!(one_line(input), expected, "{input:?}");
    }
}

#[test]
fn a_clean_string_is_unchanged() {
    assert_eq!(one_line("say hi"), "say hi");
}

#[test]
fn the_output_holds_no_control_character() {
    let out = one_line("a\r\nb\tc\u{1b}d\0e");
    assert!(out.chars().all(|c| !c.is_control()), "{out:?}");
    assert_eq!(out, "a b c d e");
}
