//! Tests for which strings of an event are searched and the artifact a line
//! names.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use contract::session_search::Label::{self, Message, ToolInput, ToolOutput};
use serde_json::{Value, json};

use super::*;

/// A kind, its payload and the strings it gives.
type Row = (&'static str, Value, Vec<(Label, &'static str)>);

fn got(kind: &str, payload: &Value) -> Vec<(Label, String)> {
    labelled(kind, payload.as_object().unwrap())
        .into_iter()
        .map(|(label, text)| (label, text.to_owned()))
        .collect()
}

fn want(rows: &[(Label, &str)]) -> Vec<(Label, String)> {
    rows.iter()
        .map(|(label, text)| (*label, (*text).to_owned()))
        .collect()
}

fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

fn image() -> Value {
    json!({"type": "image", "path": "artifacts/i.png", "mime_type": "image/png", "width": 1, "height": 1})
}

#[test]
fn each_searched_kind_gives_its_strings_under_its_label() {
    let table: Vec<Row> = vec![
        (
            "turn_started",
            json!({"input": [
                {"type": "message", "content": [text("one"), image(), text("two")], "from": "person"},
                {"type": "shell_command", "seq": 3},
                {"type": "message", "content": [text("three")]},
            ]}),
            vec![(Message, "one"), (Message, "two"), (Message, "three")],
        ),
        (
            "steering_applied",
            json!({"content": [image(), {"type": "unknown", "text": "no"}, text("steer")]}),
            vec![(Message, "steer")],
        ),
        (
            "text_completed",
            json!({"text": "reply"}),
            vec![(Message, "reply")],
        ),
        (
            "tool_call_requested",
            json!({"name": "shell", "arguments": {"command": "ls", "n": 3, "nested": {"deep": ["x", 1, true, null]}}}),
            vec![(ToolInput, "ls"), (ToolInput, "x")],
        ),
        (
            "tool_call_requested",
            json!({"name": "shell", "arguments": "{not json"}),
            vec![(ToolInput, "{not json")],
        ),
        (
            "tool_call_started",
            json!({"effects": ["reads"], "arguments": {"path": "a.rs"}}),
            vec![(ToolInput, "a.rs")],
        ),
        (
            "tool_call_started",
            json!({"effects": ["reads"], "reversible": true}),
            vec![],
        ),
        (
            "shell_command",
            json!({"command": "make", "output": "done", "artifact": "artifacts/s.txt"}),
            vec![(ToolInput, "make"), (ToolOutput, "done")],
        ),
        (
            "tool_call_completed",
            json!({"status": "completed", "content": [text("out"), image()], "reason": "r"}),
            vec![(ToolOutput, "out")],
        ),
        (
            "job_line",
            json!({"job_id": "j", "lines": "a\nb"}),
            vec![(ToolOutput, "a\nb")],
        ),
        (
            "delegate_finished",
            json!({"job_id": "j", "text": "final", "questions": [{"question": "q?"}]}),
            vec![(ToolOutput, "final")],
        ),
        (
            "job_completed",
            json!({"job_id": "j", "status": "failed", "output_tail": "tail"}),
            vec![(ToolOutput, "tail")],
        ),
        (
            "job_completed",
            json!({"job_id": "j", "status": "completed"}),
            vec![],
        ),
        ("reasoning_completed", json!({"text": "thinking"}), vec![]),
        ("session_started", json!({"workspace": "/w"}), vec![]),
        (
            "job_started",
            json!({"description": "d", "output_path": "artifacts/j.out"}),
            vec![],
        ),
    ];
    for (kind, payload, rows) in table {
        assert_eq!(got(kind, &payload), want(&rows), "{kind}: {payload}");
    }
}

#[test]
fn a_key_equal_to_the_query_is_not_searched() {
    let payload = json!({"arguments": {"needle": 5}});
    assert_eq!(got("tool_call_requested", &payload), want(&[]));
}

#[test]
fn every_answer_shape_gives_its_text() {
    let base = json!({"request_id": "r", "by": "person"});
    let with = |extra: Value| {
        let mut payload = base.clone();
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        payload
    };
    let table: Vec<(Value, Vec<(Label, &str)>)> = vec![
        (with(json!({"declined": true})), vec![]),
        (with(json!({"confirmed": true})), vec![]),
        (
            with(json!({"labels": ["yes", "no"]})),
            vec![(Message, "yes"), (Message, "no")],
        ),
        (with(json!({"text": "typed"})), vec![(Message, "typed")]),
        (
            with(json!({"answers": [
                {"skipped": true},
                {"labels": ["a"], "text": "t"},
                {"labels": []},
            ], "note": "n"})),
            vec![(Message, "a"), (Message, "t"), (Message, "n")],
        ),
        (with(json!({"answers": [{"skipped": true}]})), vec![]),
    ];
    for (payload, rows) in table {
        assert_eq!(
            got("interaction_resolved", &payload),
            want(&rows),
            "{payload}"
        );
    }
}

#[test]
fn the_naming_fields_name_an_artifact() {
    let table = [
        (
            "tool_call_completed",
            json!({"artifact": "artifacts/a"}),
            Some("artifacts/a"),
        ),
        (
            "shell_command",
            json!({"artifact": "artifacts/b"}),
            Some("artifacts/b"),
        ),
        (
            "delegate_finished",
            json!({"artifact": "artifacts/c"}),
            Some("artifacts/c"),
        ),
        (
            "job_started",
            json!({"output_path": "artifacts/d"}),
            Some("artifacts/d"),
        ),
        ("tool_call_completed", json!({"content": []}), None),
        ("job_started", json!({"artifact": "artifacts/e"}), None),
        ("text_completed", json!({"artifact": "artifacts/f"}), None),
    ];
    for (kind, payload, path) in table {
        assert_eq!(names(kind, payload.as_object().unwrap()), path, "{kind}");
    }
}
