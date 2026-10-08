//! Provider call dispatch and case verdicts (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::unwrap_used,
    reason = "the call dispatch assertion fails with its result"
)]

use serde_json::json;

use super::{compare_result, function};

#[test]
fn only_cost_is_a_supported_provider_call() {
    assert_eq!(function("cost"), Ok(()));
    let error = function("models").unwrap_err();
    assert!(error.contains("cost"), "{error}");
}

#[test]
fn returns_and_error_use_the_case_json_subset_matcher() {
    assert!(compare_result(Some(&json!(0.5)), None, Ok(json!(0.5))).is_empty());

    let mismatch = compare_result(Some(&json!(0.4)), None, Ok(json!(0.5))).join("\n");
    assert!(mismatch.contains("returns"), "{mismatch}");

    assert!(
        compare_result(
            None,
            Some(&json!({"code": "extension_failed"})),
            Err(json!({"code": "extension_failed", "message": "lookup failed"}))
        )
        .is_empty()
    );

    let mismatch = compare_result(
        None,
        Some(&json!({"code": "io_failed"})),
        Err(json!({"code": "extension_failed", "message": "lookup failed"})),
    )
    .join("\n");
    assert!(mismatch.contains("error.code"), "{mismatch}");
}

#[test]
fn an_error_when_a_return_was_expected_names_returns() {
    let error = compare_result(
        Some(&json!(0.5)),
        None,
        Err(json!({"code": "extension_failed", "message": "lookup failed"})),
    )
    .join("\n");
    assert!(error.contains("returns"), "{error}");
}
