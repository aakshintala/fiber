#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use crate::{Secret, redact::Secrets};

fn secrets(values: &[&str]) -> Secrets {
    let mut secrets = Secrets::default();
    for value in values {
        secrets.add(Secret::new((*value).to_owned()));
    }
    secrets
}

#[test]
fn secrets_redact_exact_output() {
    let cases: &[(&[&str], &str, &str)] = &[
        (&["sk-1"], "nothing here", "nothing here"),
        (
            &["sk-1"],
            "sk-1 and sk-1 again",
            "[redacted] and [redacted] again",
        ),
        (&[""], "abc", "abc"),
        (&["sk-abc", "sk-abcdef"], "sk-abcdef", "[redacted]"),
        (&["sk-abcdef", "sk-abc"], "sk-abcdef", "[redacted]"),
        (&["red", "sk-1"], "sk-1", "[redacted]"),
        (&["sk-1"], "é sk-1 ü", "é [redacted] ü"),
    ];
    for (values, text, expected) in cases {
        assert_eq!(
            secrets(values).redact(text),
            *expected,
            "values {values:?} in {text:?}"
        );
    }
}

#[test]
fn add_header_splits_authorization() {
    let mut signed = Secrets::default();
    signed.add_header("authorization", "Bearer tok");
    assert_eq!(signed.redact("Bearer tok"), "[redacted]");
    assert_eq!(signed.redact("tok"), "[redacted]");

    let mut mixed = Secrets::default();
    mixed.add_header("Authorization", "Bearer tok");
    assert_eq!(mixed.redact("Bearer tok"), "[redacted]");
    assert_eq!(mixed.redact("tok"), "[redacted]");

    let mut other = Secrets::default();
    other.add_header("x-sig", "a b");
    assert_eq!(other.redact("a b"), "[redacted]");
    assert_eq!(other.redact("b"), "b");

    let mut bare = Secrets::default();
    bare.add_header("authorization", "Bearer ");
    assert_eq!(bare.redact("Bearer "), "[redacted]");
    assert_eq!(bare.redact("xyz"), "xyz");

    let mut nospace = Secrets::default();
    nospace.add_header("authorization", "tok");
    assert_eq!(nospace.redact("tok"), "[redacted]");
}

#[test]
fn secrets_debug_shows_no_added_value() {
    let mut secrets = Secrets::default();
    secrets.add(Secret::new("sk-q9".to_owned()));
    secrets.add_header("authorization", "Bearer tok-q9");
    let shown = format!("{secrets:?}");
    assert!(!shown.contains("sk-q9"), "{shown:?}");
    assert!(!shown.contains("tok-q9"), "{shown:?}");
}
