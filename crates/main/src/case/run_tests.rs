//! Matching durable case events (`docs/testing.md`, "Event streams").

#![allow(
    clippy::indexing_slicing,
    reason = "small JSON fixtures are indexed by field"
)]

use serde_json::{Value, json};

use super::session_verdict;

fn line(kind: &str, seq: Option<u64>, payload: Value) -> Value {
    let mut value = json!({"kind": kind, "payload": payload});
    if let Some(seq) = seq {
        value["seq"] = json!(seq);
    }
    value
}

#[test]
fn equal_ordered_durable_lines_match_the_expected_subset() {
    let expected = [
        json!({"kind": "turn_started"}),
        json!({"kind": "turn_completed", "payload": {"outcome": "completed"}}),
    ];
    let actual = [
        line("turn_started", Some(4), json!({"extra": true})),
        line(
            "turn_completed",
            Some(5),
            json!({"outcome": "completed", "extra": 1}),
        ),
    ];
    assert!(session_verdict(&expected, &actual, &[]).is_empty());
}

#[test]
fn a_missing_or_extra_line_fails_with_its_index() {
    let expected = [
        json!({"kind": "turn_started"}),
        json!({"kind": "turn_completed"}),
    ];
    let missing = [line("turn_started", Some(1), json!({}))];
    let error = session_verdict(&expected, &missing, &[]).join("\n");
    assert!(error.contains("expect[1]"), "{error}");

    let extra = [
        line("turn_started", Some(1), json!({})),
        line("turn_completed", Some(2), json!({})),
        line("fiber_exited", Some(3), json!({})),
    ];
    let error = session_verdict(&expected, &extra, &[]).join("\n");
    assert!(error.contains("event[2]"), "{error}");
}

#[test]
fn reordered_and_wrong_fields_name_the_first_mismatch() {
    let expected = [
        json!({"kind": "turn_started"}),
        json!({"kind": "turn_completed", "payload": {"outcome": "completed"}}),
    ];
    let swapped = [
        line("turn_completed", Some(1), json!({"outcome": "completed"})),
        line("turn_started", Some(2), json!({})),
    ];
    let error = session_verdict(&expected, &swapped, &[]).join("\n");
    assert!(error.contains("expect[0].kind"), "{error}");

    let wrong = [
        line("turn_started", Some(1), json!({})),
        line("turn_completed", Some(2), json!({"outcome": "failed"})),
    ];
    let error = session_verdict(&expected, &wrong, &[]).join("\n");
    assert!(error.contains("expect[1].payload.outcome"), "{error}");
}

#[test]
fn ephemeral_lines_are_dropped_and_host_misses_fail_the_case() {
    let expected = [json!({"kind": "turn_completed"})];
    let lines = [
        line("assistant_message_delta", None, json!({"text": "fragment"})),
        line("turn_completed", Some(1), json!({})),
    ];
    assert!(session_verdict(&expected, &lines, &[]).is_empty());

    let unmet = ["host.http[1] miss: request {\"url\":\"https://example.test\"}".to_owned()];
    let error = session_verdict(&expected, &lines, &unmet).join("\n");
    assert!(error.contains("host.http[1]"), "{error}");
}
