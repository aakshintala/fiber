//! Unit tests for `cap_markers`: the most cache markers Anthropic takes in
//! one request (`docs/prompt-cache.md`, "Cache markers and keys").

#![allow(clippy::unwrap_used, reason = "test code")]

use serde_json::{Value, json};

use super::cap_markers;

fn count_markers(value: &Value) -> usize {
    fn walk(value: &Value, n: &mut usize) {
        match value {
            Value::Object(map) => {
                if map.contains_key("cache_control") {
                    *n += 1;
                }
                for v in map.values() {
                    walk(v, n);
                }
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, n)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    let mut n = 0;
    walk(value, &mut n);
    n
}

fn marked_tool(name: &str) -> Value {
    json!({"name": name, "input_schema": {"type": "object"},
        "cache_control": {"type": "ephemeral"}})
}

/// The markers the four-turn request produced: the system prompt's, the
/// previous end's (the assistant's tool-call block) and the new end's (the
/// last user text), plus five markers model data added on tools. Past four
/// the excess goes, keeping Fiber's three in the doc's order, then the
/// first tool's.
#[test]
fn no_request_carries_more_than_four_cache_markers() {
    let marker = json!({"type": "ephemeral"});
    let mut body = json!({
        "system": [{"type": "text", "text": "You are terse.", "cache_control": marker}],
        "messages": [
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                 "input": {"city": "Paris"}, "cache_control": marker}]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18 C, clear"},
                {"type": "text", "text": "And Rome?", "cache_control": marker}]},
        ],
        "tools": [marked_tool("t0"), marked_tool("t1"), marked_tool("t2"),
            marked_tool("t3"), marked_tool("t4")],
    });
    cap_markers(&mut body);
    assert_eq!(count_markers(&body), 4);
    // Fiber's three, in the order the doc lists, then the first tool's.
    assert_eq!(body["system"][0]["cache_control"], marker);
    assert_eq!(body["messages"][1]["content"][0]["cache_control"], marker);
    assert_eq!(body["messages"][2]["content"][1]["cache_control"], marker);
    assert_eq!(body["tools"][0]["cache_control"], marker);
    assert_eq!(body["tools"][1].get("cache_control"), None);
}

/// Three marked system blocks with the previous end and the new end is
/// five, and the doc's order keeps the system prompt's and the previous
/// end's.
#[test]
fn past_four_markers_the_new_end_goes_before_the_previous_end() {
    let marked = json!({"type": "text", "text": "s", "cache_control": {"type": "ephemeral"}});
    let marker = json!({"type": "ephemeral"});
    let mut body = json!({
        "system": [marked, marked, marked],
        "messages": [
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                 "input": {"city": "Paris"}, "cache_control": marker}]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18 C, clear"},
                {"type": "text", "text": "And Rome?", "cache_control": marker}]},
        ],
    });
    cap_markers(&mut body);
    assert_eq!(body["system"], json!([marked, marked, marked]));
    assert_eq!(body["messages"][1]["content"][0]["cache_control"], marker);
    assert_eq!(body["messages"][2]["content"][1].get("cache_control"), None);
}
