//! Tests for bare URLs (`docs/tui.md`, "Links").

use super::urls;

/// The URLs `text` holds, as text.
fn found(text: &str) -> Vec<String> {
    urls(text)
        .into_iter()
        .map(|range| text.get(range).unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn urls_finds_bare_urls_and_cuts_trailing_punctuation() {
    type Case<'a> = (&'a str, &'a str, Vec<&'a str>);
    let cases: Vec<Case<'_>> = vec![
        ("a bare URL", "see http://example.com/a here", vec![
            "http://example.com/a",
        ]),
        ("an https URL", "see https://example.com", vec![
            "https://example.com",
        ]),
        ("a trailing period", "see http://example.com.", vec![
            "http://example.com",
        ]),
        ("trailing punctuation", "see http://example.com,;:", vec![
            "http://example.com",
        ]),
        (
            "a quoted URL",
            "\"http://example.com/a\"",
            vec!["http://example.com/a"],
        ),
        (
            "a parenthesised URL",
            "(http://example.com/a)",
            vec!["http://example.com/a"],
        ),
        (
            "a URL with a balanced paren",
            "http://example.com/a(b)",
            vec!["http://example.com/a(b)"],
        ),
        (
            "a URL followed by a close paren it did not open",
            "see (http://example.com/a),",
            vec!["http://example.com/a"],
        ),
        (
            "a bracketed URL",
            "[http://example.com/a]",
            vec!["http://example.com/a"],
        ),
        ("http alone is none", "see http:// here", vec![]),
        (
            "a URL inside a word",
            "abhttp://example.com/cd",
            vec!["http://example.com/cd"],
        ),
        (
            "two URLs",
            "http://a.example and https://b.example/c",
            vec!["http://a.example", "https://b.example/c"],
        ),
        (
            "a long URL that wraps on screen",
            "see http://example.com/aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh for more",
            vec!["http://example.com/aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh"],
        ),
    ];
    for (name, text, expected) in cases {
        let expected: Vec<String> = expected.into_iter().map(str::to_owned).collect();
        assert_eq!(found(text), expected, "{name}");
    }
}
