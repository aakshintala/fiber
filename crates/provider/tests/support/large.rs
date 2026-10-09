//! Comparing large test values without printing them whole on failure
//! (`docs/testing.md`, "What a test asserts"): a recorded block can be tens
//! of kilobytes, which `assert_eq!` would print whole into the CI log.

use serde_json::Value;

/// Asserts two byte strings hold the same bytes, reporting only the lengths
/// and the first differing offset. Exact equality: nothing is truncated or
/// hashed away.
pub(crate) fn assert_bytes_eq(expected: &[u8], actual: &[u8], what: &str) {
    let offset = expected.iter().zip(actual.iter()).position(|(a, b)| a != b);
    let same = expected.len() == actual.len() && offset.is_none();
    assert!(
        same,
        "{what}: lengths {} vs {}, first difference at {}",
        expected.len(),
        actual.len(),
        offset.unwrap_or(expected.len().min(actual.len()))
    );
}

/// Asserts two JSON values are exactly equal, reporting only the lengths
/// and the first differing offset of their canonical serialisations (both
/// sides go through the same serialiser with its keys sorted, so equal
/// values serialise to equal bytes and back).
pub(crate) fn assert_json_eq(expected: &Value, actual: &Value, what: &str) {
    assert_bytes_eq(
        serde_json::to_string(expected).unwrap().as_bytes(),
        serde_json::to_string(actual).unwrap().as_bytes(),
        what,
    );
}
