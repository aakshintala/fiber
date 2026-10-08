//! Provider call dispatch and case verdicts (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::unwrap_used,
    reason = "the call dispatch assertion fails with its result"
)]

use serde_json::{Value, json};

use super::{CallOutcome, compare_result, function};

#[test]
fn only_cost_is_a_supported_provider_call() {
    assert_eq!(function("cost"), Ok(()));
    let error = function("models").unwrap_err();
    assert!(error.contains("cost"), "{error}");
}

fn returns(value: Value) -> CallOutcome {
    CallOutcome::Returns(value)
}

fn error(value: Value) -> CallOutcome {
    CallOutcome::Error(value)
}

#[test]
fn returns_and_error_use_the_case_json_subset_matcher() {
    assert!(compare_result(&returns(json!(0.5)), Ok(json!(0.5))).is_empty());

    let mismatch = compare_result(&returns(json!(0.4)), Ok(json!(0.5))).join("\n");
    assert!(mismatch.contains("returns"), "{mismatch}");

    assert!(
        compare_result(
            &error(json!({"code": "extension_failed"})),
            Err(json!({"code": "extension_failed", "message": "lookup failed"}))
        )
        .is_empty()
    );

    let mismatch = compare_result(
        &error(json!({"code": "io_failed"})),
        Err(json!({"code": "extension_failed", "message": "lookup failed"})),
    )
    .join("\n");
    assert!(mismatch.contains("error.code"), "{mismatch}");
}

#[test]
fn an_error_when_a_return_was_expected_names_returns() {
    let error = compare_result(
        &returns(json!(0.5)),
        Err(json!({"code": "extension_failed", "message": "lookup failed"})),
    )
    .join("\n");
    assert!(error.contains("returns"), "{error}");
}
