//! Shared wire helpers for the four protocols: reading a string field
//! and decoding a tool call's argument text (`docs/events.md`,
//! `tool_call_requested`, `arguments`).

use contract::provider::ToolDefinition;
use serde_json::{Map, Value};

/// The string at `key`, or `""`.
pub(crate) fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A tool call's argument text as its arguments: the JSON object or array
/// it holds, `{}` for no text (a tool that takes no arguments), or the raw
/// text as a string when it holds neither (`docs/events.md`,
/// `tool_call_requested`, `arguments`).
pub(crate) fn args_value(text: String) -> Value {
    if text.is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    match serde_json::from_str(&text) {
        Ok(value @ (Value::Object(_) | Value::Array(_))) => value,
        Ok(_) | Err(_) => Value::String(text),
    }
}

/// The map a built tool object holds, moved; any other value an empty
/// map. Two calls built from the same inputs send the same bytes
/// (`docs/prompt-cache.md`, "Bytes").
pub(crate) fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_) => Map::new(),
    }
}

/// The tools in name order, so two calls built from the same inputs send
/// the same bytes (`docs/prompt-cache.md`, "Bytes").
pub(crate) fn sorted_by_name(tools: &[ToolDefinition]) -> Vec<&ToolDefinition> {
    let mut sorted: Vec<&ToolDefinition> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted
}
