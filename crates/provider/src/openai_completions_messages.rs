//! The `openai-completions` messages: the conversation as Chat Completions
//! messages, after the system prompt.

use std::collections::BTreeMap;

use contract::ActionId;
use contract::events::CacheLifetime;
use contract::provider::{Input, ModelRequest};
use serde_json::{Map, Value, json};

use crate::Endpoint;
use crate::anthropic_messages::cache_control;

/// The conversation as Chat Completions messages, after the system prompt.
/// An assistant's reasoning, text and tool calls fold into one message; each
/// tool result is a `tool` message of its own.
///
/// With [`crate::Compat::anthropic`], it carries the markers Anthropic
/// takes (`docs/prompt-cache.md`, "Cache markers and keys"): the end of the
/// system prompt, the point where the previous request ended, and the new
/// end.
pub(crate) fn messages(endpoint: &Endpoint, request: &ModelRequest) -> Vec<Value> {
    let reference = endpoint.reference();
    // A call's `id` is the provider's id for it, or its action id when the
    // reply carried none; its result names the same one.
    let call_ids: BTreeMap<&ActionId, &str> = request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolCall { action_id, call } => Some((
                action_id,
                call.provider_id
                    .as_ref()
                    .map_or(action_id.0.as_str(), |id| id.0.as_str()),
            )),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolResult { .. } => None,
        })
        .collect();
    let call_id = |action_id: &ActionId| {
        call_ids
            .get(action_id)
            .copied()
            .unwrap_or(action_id.0.as_str())
            .to_owned()
    };

    let mut out: Vec<Map<String, Value>> = Vec::new();
    let mut system = None;
    if !request.system_prompt.is_empty() {
        out.push(message(
            json!({"role": "system", "content": request.system_prompt}),
        ));
        system = Some(0);
    }
    // How many messages there are after each conversation input: the last
    // of them holds that input, or the nearest earlier one when the input
    // added nothing.
    let mut ends: Vec<usize> = Vec::with_capacity(request.conversation.len());
    for input in &request.conversation {
        match input {
            Input::User { text } => {
                out.push(message(json!({"role": "user", "content": text})));
            }
            // The flag is ignored: Chat Completions defines no error field
            // on a tool message (`ChatCompletionRequestToolMessage`: `role,
            // content, tool_call_id`), so a failed result sends the same
            // bytes as a success.
            Input::ToolResult {
                action_id, text, ..
            } => {
                out.push(message(json!({
                    "role": "tool",
                    "tool_call_id": call_id(action_id),
                    "content": text,
                })));
            }
            Input::Assistant { text, .. } if text.is_empty() => {}
            Input::Assistant { text, .. } => {
                let mut m = take_assistant(&mut out, &["content"]);
                m.insert("content".into(), json!(text));
                out.push(m);
            }
            // Reasoning goes back unchanged, only to the model that produced
            // it, and never as plain text.
            Input::Reasoning {
                model,
                provider_item: Some(Value::Object(fields)),
                ..
            } if *model == reference => {
                let keys: Vec<&str> = fields.keys().map(String::as_str).collect();
                let mut m = take_assistant(&mut out, &keys);
                m.extend(fields.clone());
                out.push(m);
            }
            Input::Reasoning { .. } => {}
            Input::ToolCall { action_id, call } => {
                let mut m = take_assistant(&mut out, &[]);
                let call = json!({
                    "id": call_id(action_id),
                    "type": "function",
                    "function": {"name": call.name, "arguments": arguments_text(&call.arguments)},
                });
                match m.get_mut("tool_calls") {
                    Some(Value::Array(calls)) => calls.push(call),
                    _ => {
                        m.insert("tool_calls".into(), json!([call]));
                    }
                }
                out.push(m);
            }
        }
        ends.push(out.len());
    }
    // An assistant message with neither text nor tool calls, only reasoning,
    // still needs its `content`.
    for m in &mut out {
        if m.get("role") == Some(&json!("assistant"))
            && !m.contains_key("content")
            && !m.contains_key("tool_calls")
        {
            m.insert("content".into(), json!(""));
        }
    }

    if endpoint.compat.anthropic {
        // The message the previous request ended on.
        let previous_end = request
            .previous_end
            .and_then(|end| ends.get(end.checked_sub(1)?)?.checked_sub(1));
        let last = out.len().checked_sub(1);
        // A message named twice is marked once: its content is no longer
        // a string the second time.
        for at in [system, previous_end, last].into_iter().flatten() {
            if let Some(m) = out.get_mut(at) {
                mark(m, &request.cache_lifetime);
            }
        }
    }
    out.into_iter().map(Value::Object).collect()
}

/// A message object from `value`, which is always one.
fn message(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Map::new()
        }
    }
}

/// The assistant message to fold into, taken off `out`: the last message
/// when it is an assistant's that has none of `keys` yet, else a new one.
/// The caller pushes it back.
fn take_assistant(out: &mut Vec<Map<String, Value>>, keys: &[&str]) -> Map<String, Value> {
    let fits = out.last().is_some_and(|m| {
        m.get("role") == Some(&json!("assistant")) && keys.iter().all(|k| !m.contains_key(*k))
    });
    match out.pop() {
        Some(m) if fits => m,
        last => {
            out.extend(last);
            message(json!({"role": "assistant"}))
        }
    }
}

/// Marks a message with a cache breakpoint: its text `content` becomes one
/// text part carrying `cache_control`, `{"type": "ephemeral"}` with `ttl`
/// for a 1-hour cache, which OpenRouter passes through to Anthropic
/// (`research/openai-completions-probe`).
///
/// debt: an assistant message holding only tool calls has no text part
/// to mark and is left unmarked; mark its last call if a session's cache
/// shows the gap.
fn mark(message: &mut Map<String, Value>, lifetime: &CacheLifetime) {
    let Some(Value::String(text)) = message.get("content") else {
        return;
    };
    let part = json!([{"type": "text", "text": text, "cache_control": cache_control(lifetime)}]);
    message.insert("content".into(), part);
}

/// Arguments as the text Chat Completions carries: a raw string as it was,
/// an object serialised.
fn arguments_text(arguments: &Value) -> String {
    if let Value::String(raw) = arguments {
        raw.clone()
    } else {
        arguments.to_string()
    }
}
