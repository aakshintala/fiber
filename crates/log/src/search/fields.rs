//! Which strings of an event are searched, and under which label
//! (`docs/tools.md`, "Searching past sessions"), and which artifact a line
//! names. No other event is searched, and reasoning is not.

use contract::session_search::Label;
use serde_json::{Map, Value};

/// The searched strings of a `kind` line's `payload`, in field order, each
/// with its label. A kind that is not searched gives none.
pub(super) fn labelled<'a>(kind: &str, payload: &'a Map<String, Value>) -> Vec<(Label, &'a str)> {
    let mut out = Vec::new();
    match kind {
        "turn_started" => {
            for item in array(payload.get("input")) {
                if item.get("type").and_then(Value::as_str) == Some("message") {
                    text_parts(item.get("content"), Label::Message, &mut out);
                }
            }
        }
        "steering_applied" => text_parts(payload.get("content"), Label::Message, &mut out),
        "text_completed" => push(&mut out, Label::Message, payload.get("text")),
        "interaction_resolved" => answers(payload, &mut out),
        // `tool_call_started` carries arguments only when a hook rewrote
        // them.
        "tool_call_requested" | "tool_call_started" => {
            if let Some(arguments) = payload.get("arguments") {
                strings(arguments, Label::ToolInput, &mut out);
            }
        }
        "shell_command" => {
            push(&mut out, Label::ToolInput, payload.get("command"));
            push(&mut out, Label::ToolOutput, payload.get("output"));
        }
        "tool_call_completed" => text_parts(payload.get("content"), Label::ToolOutput, &mut out),
        "job_line" => push(&mut out, Label::ToolOutput, payload.get("lines")),
        "delegate_finished" => push(&mut out, Label::ToolOutput, payload.get("text")),
        "job_completed" => push(&mut out, Label::ToolOutput, payload.get("output_tail")),
        _ => {}
    }
    out
}

/// The artifact path a `kind` line names: `artifact` on
/// `tool_call_completed`, `shell_command` and `delegate_finished`, and
/// `output_path` on `job_started`.
pub(super) fn names<'a>(kind: &str, payload: &'a Map<String, Value>) -> Option<&'a str> {
    let field = match kind {
        "tool_call_completed" | "shell_command" | "delegate_finished" => "artifact",
        "job_started" => "output_path",
        _ => return None,
    };
    payload.get(field).and_then(Value::as_str)
}

/// `value` under `label` when it is a string.
fn push<'a>(out: &mut Vec<(Label, &'a str)>, label: Label, value: Option<&'a Value>) {
    if let Some(text) = value.and_then(Value::as_str) {
        out.push((label, text));
    }
}

/// `value`'s items when it is an array; none otherwise.
fn array(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// The text of each text part in `content`; image parts hold no text.
fn text_parts<'a>(content: Option<&'a Value>, label: Label, out: &mut Vec<(Label, &'a str)>) {
    for part in array(content) {
        if part.get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = part.get("text").and_then(Value::as_str)
        {
            out.push((label, text));
        }
    }
}

/// Every string in `value` at any depth, in document order. Keys, numbers,
/// booleans and nulls are not searched.
fn strings<'a>(value: &'a Value, label: Label, out: &mut Vec<(Label, &'a str)>) {
    match value {
        Value::String(text) => out.push((label, text)),
        Value::Array(items) => {
            for item in items {
                strings(item, label, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                strings(item, label, out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// The answers in an `interaction_resolved` payload: the chosen labels, the
/// typed text, each answered form field's labels and text, then the form's
/// note. A declined, confirmed or skipped answer holds no text.
fn answers<'a>(payload: &'a Map<String, Value>, out: &mut Vec<(Label, &'a str)>) {
    let answered = |answer: &'a Map<String, Value>, out: &mut Vec<(Label, &'a str)>| {
        for label in array(answer.get("labels")) {
            push(out, Label::Message, Some(label));
        }
        push(out, Label::Message, answer.get("text"));
    };
    answered(payload, out);
    for answer in array(payload.get("answers")) {
        if let Some(answer) = answer.as_object() {
            answered(answer, out);
        }
    }
    push(out, Label::Message, payload.get("note"));
}

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;
