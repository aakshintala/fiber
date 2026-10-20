//! Shared wire helpers for the protocols: reading a string field, decoding
//! a tool call's argument text (`docs/events.md`, `tool_call_requested`,
//! `arguments`) and encoding a model id into a URL path.

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
    if let Value::Object(map) = value {
        map
    } else {
        Map::new()
    }
}

/// The tools in name order, so two calls built from the same inputs send
/// the same bytes (`docs/prompt-cache.md`, "Bytes").
pub(crate) fn sorted_by_name(tools: &[ToolDefinition]) -> Vec<&ToolDefinition> {
    let mut sorted: Vec<&ToolDefinition> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted
}

/// `text` as one URL path segment: every byte outside RFC 3986's unreserved
/// set percent-encoded, so a Bedrock model id such as
/// `global.anthropic.claude-haiku-4-5-20251001-v1:0` keeps its `:` as `%3A`,
/// as AWS's own SDKs send it.
pub(crate) fn path_segment(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
