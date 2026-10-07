//! Tests for the escaped query, the matchers and the snippet.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use proptest::prelude::*;

use super::*;

#[test]
fn the_query_is_escaped_as_the_log_writes_it() {
    let table = [
        ("say \"hi\"", r#"say \"hi\""#),
        (r"C:\dir", r"C:\\dir"),
        ("a\nb", r"a\nb"),
        ("a\tb", r"a\tb"),
        ("a\rb", r"a\rb"),
        ("a\u{8}b", r"a\bb"),
        ("a\u{c}b", r"a\fb"),
        ("a\u{1}b", r"a\u0001b"),
        ("a\u{1b}b", r"a\u001bb"),
        ("ünï ✓", "ünï ✓"),
        ("a/b", "a/b"),
    ];
    for (text, want) in table {
        assert_eq!(escaped(text), want, "{text:?}");
    }
}

/// `s` with the case of each ASCII letter flipped.
fn flipped(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

proptest! {
    /// The raw pass never selects too little: any piece of a string, in any
    /// ASCII case, matches the string as the log writes it.
    #[test]
    fn the_raw_matcher_finds_any_piece_of_an_encoded_string(
        s in any::<String>(),
        a in any::<prop::sample::Index>(),
        b in any::<prop::sample::Index>(),
    ) {
        let chars: Vec<char> = s.chars().collect();
        prop_assume!(!chars.is_empty());
        let (x, y) = (a.index(chars.len()), b.index(chars.len()));
        let (from, to) = (x.min(y), x.max(y) + 1);
        let piece: String = chars[from..to].iter().collect();
        let text = Text::new(&flipped(&piece)).unwrap();
        let line = serde_json::to_string(&s).unwrap();
        prop_assert!(text.raw(&[]).unwrap().is_match(line.as_bytes()).unwrap());
    }
}

#[test]
fn the_raw_matcher_selects_the_naming_lines_and_extra_literals() {
    let raw = Text::new("absent")
        .unwrap()
        .raw(&["artifacts/x.txt".to_owned()]);
    let raw = raw.unwrap();
    for line in [
        r#"{"kind":"session_named","seq":1}"#,
        r#"{"kind":"turn_started","seq":1}"#,
        r#"{"kind":"tool_call_completed","payload":{"artifact":"artifacts/x.txt"}}"#,
    ] {
        assert!(raw.is_match(line.as_bytes()).unwrap(), "{line}");
    }
    assert!(
        !raw.is_match(br#"{"kind":"text_completed","seq":1}"#)
            .unwrap()
    );
}

#[test]
fn the_decoded_matcher_folds_non_ascii_case() {
    let text = Text::new("Ünïcode").unwrap();
    assert_eq!(text.find("x üNÏCODE y"), Some((2, 11)));
    assert_eq!(text.find("unicode"), None);
}

#[test]
fn a_query_with_a_newline_searches_artifacts_across_lines() {
    let text = Text::new("one\ntwo").unwrap();
    assert!(text.multi_line());
    assert!(text.artifact().is_match(b"ONE\nTWO").unwrap());
    let text = Text::new("one").unwrap();
    assert!(!text.multi_line());
    assert_eq!(
        text.artifact().line_terminator().map(|t| t.as_byte()),
        Some(b'\n')
    );
}

#[test]
fn the_snippet_keeps_100_characters_before_and_200_in_all() {
    let a = |n: usize| "a".repeat(n);
    let b = |n: usize| "b".repeat(n);
    let table: Vec<(String, usize, String)> = vec![
        // Short: whole, uncut.
        ("x retry y".into(), 2, "x retry y".into()),
        // At the start of a long text: cut on the right only.
        (format!("X{}", a(299)), 0, format!("X{}…", a(199))),
        // At the end of a long text: cut on the left only.
        (format!("{}X", a(299)), 299, format!("…{}X", a(100))),
        // In the middle: both ends cut.
        (
            format!("{}X{}", a(150), b(150)),
            150,
            format!("…{}X{}…", a(100), b(99)),
        ),
        // Exactly 100 before: the left is not cut.
        (format!("{}X", a(100)), 100, format!("{}X", a(100))),
        // 101 before: the left is cut.
        (format!("{}X", a(101)), 101, format!("…{}X", a(100))),
        // Exactly 200 in all: the right is not cut.
        (format!("X{}", a(199)), 0, format!("X{}", a(199))),
        // A match longer than the window is cut at 200 characters.
        (
            format!("{}{}{}", a(10), "M".repeat(300), b(10)),
            10,
            format!("{}{}…", a(10), "M".repeat(190)),
        ),
    ];
    for (text, start, want) in table {
        assert_eq!(snippet(&text, start), want, "start {start}");
    }
}

#[test]
fn the_snippet_counts_multibyte_characters_whole() {
    let text = format!("{}X{}", "é".repeat(150), "ü".repeat(150));
    let start = "é".len() * 150;
    let got = snippet(&text, start);
    assert_eq!(got, format!("…{}X{}…", "é".repeat(100), "ü".repeat(99)));
    assert_eq!(got.chars().count(), 202);
}
