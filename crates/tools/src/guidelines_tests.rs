//! Tests for `guidelines::of` and the guidelines byte pin.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::of;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash
}

#[test]
fn guidelines_md_bytes_are_pinned() {
    let bytes = include_bytes!("../prompt/guidelines.md");
    assert_eq!(
        bytes.len(),
        1264,
        "length changed: edit is a reviewed change"
    );
    assert_eq!(
        fnv1a(bytes),
        0x441b11994d5ae352,
        "bytes changed: edit is a reviewed change"
    );
}

#[test]
fn each_builtin_tool_returns_its_section() {
    let read = of("read").unwrap();
    assert!(read.contains("Read files with `read`"), "{read}");
    assert!(!read.starts_with("##"), "{read}");
    assert!(!read.ends_with('\n'));
    assert!(of("edit").unwrap().contains("Change an existing file"));
    assert!(of("write").unwrap().contains("Use `write` for new files"));
    assert!(
        of("shell")
            .unwrap()
            .contains("Commands run with no terminal")
    );
    assert!(of("skill").unwrap().contains("load it with `skill`"));
}

#[test]
fn unknown_tool_has_no_guidelines() {
    assert_eq!(of("nope"), None);
}

#[test]
fn a_section_stops_before_the_next_heading() {
    // `read` is followed by a blank line and `## edit`: neither the
    // blank line nor the next section's text is part of it.
    let read = of("read").unwrap();
    assert!(!read.contains("Change an existing file"), "{read}");
    assert!(!read.contains("## "), "{read}");
    assert_eq!(read, read.trim(), "{read}");
}

#[test]
fn leading_and_trailing_blank_lines_are_removed() {
    // Every section starts with a blank line after its heading and
    // ends before a blank line and the next heading.
    for name in ["read", "edit", "write", "shell"] {
        let text = of(name).unwrap();
        assert!(!text.is_empty(), "{name}");
        assert_eq!(text, text.trim(), "{name}: {text}");
    }
}
