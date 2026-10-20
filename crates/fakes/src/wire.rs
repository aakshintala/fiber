//! Builders for the `openai-responses` reply bytes the tests serve.

use serde_json::{Value, json};

use crate::Response;

/// An `openai-responses` stream of `events`, then a completed reply.
pub fn stream(events: &[Value]) -> Response {
    stream_with_usage(
        events,
        json!({"input_tokens": 10,
        "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}),
    )
}

/// An `openai-responses` stream of `events`, then a completed reply
/// carrying `usage` as its terminal usage: a caller warming the cache or
/// reading from it names its own counts here.
#[allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "a test helper; a failure is the test's"
)]
pub fn stream_with_usage(events: &[Value], usage: Value) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed", "usage": usage}});
    body.push_str(&format!(
        "event: {}\ndata: {done}\n\n",
        done["type"].as_str().unwrap()
    ));
    Response::stream(body)
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
pub fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// A finished `function_call` for `name` with `arguments`.
pub fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
}

/// An `openai-responses` stream answering `text`: what a scripted
/// reviewer verdict reads as.
pub fn text_reply(text: &str) -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
#[allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "a test helper; a failure is the test's"
)]
pub fn hello_inline_completed() -> Response {
    let events = [
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// An `openai-responses` stream answering `Hello.`.
pub fn hello_single_delta() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hello."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// An `openai-responses` stream answering `Hello.`.
pub fn hello_item_done() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
    }})])
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
