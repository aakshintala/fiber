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
        1018,
        "length changed: edit is a reviewed change"
    );
    assert_eq!(
        fnv1a(bytes),
        0xb7406b9871d18543,
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
}

#[test]
fn unknown_tool_has_no_guidelines() {
    assert_eq!(of("nope"), None);
}
