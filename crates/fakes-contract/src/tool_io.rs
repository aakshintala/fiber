//! What many tool tests share: a call's argument map and a result's text.

use serde_json::{Map, Value};

/// The argument map of a tool call value. Every tool test builds its call
/// from JSON, and a non-object is the test's mistake.
///
/// # Panics
///
/// When `value` is not an object (`null`, an array, a string, and the
/// rest), naming that it must be an object.
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
pub fn tool_args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

/// The lenient form of a tool result's text: the first content part's text,
/// and `""` when there is no part or the first part is not text. A test
/// that needs exactly one text part asserts that itself.
pub fn text_of(output: &contract::tool::Output) -> String {
    match output.content.first() {
        Some(contract::shapes::ContentPart::Text { text }) => text.clone(),
        Some(
            contract::shapes::ContentPart::Image { .. }
            | contract::shapes::ContentPart::Pdf(_)
            | contract::shapes::ContentPart::Unknown,
        )
        | None => String::new(),
    }
}

#[cfg(test)]
#[path = "tool_io_tests.rs"]
mod tests;
