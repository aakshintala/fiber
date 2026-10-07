//! Builds Google Generative AI request bodies and conversation contents.

use std::collections::BTreeMap;

use contract::ActionId;
use contract::events::ToolCallRequested;
use contract::provider::{Input, ModelRequest, ToolDefinition};
use serde_json::{Map, Value, json};

use crate::{Endpoint, strict};

/// Each tool in Gemini's shape, in name order.
/// `parametersJsonSchema` takes the schema as written, `$ref` and
/// `anyOf` included; `parameters` rejects `$ref`
/// (`docs/model-routing.md`, "Google Generative AI wire facts").
/// Nothing rewrites a schema. This is the tools Fiber builds.
pub(crate) fn wire_tools(tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
    let mut sorted: Vec<&ToolDefinition> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parametersJsonSchema": tool.input_schema,
            })
            .as_object()
            .cloned()
            .unwrap_or_default()
        })
        .collect()
}

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes"). Gemini caches implicitly only, so the body carries no cache
/// marker or key (`docs/prompt-cache.md`, "Cache markers and keys").
pub(crate) fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
    let mut tools: Vec<_> = request.tools.iter().collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let mut body = Map::new();
    if !request.system_prompt.is_empty() {
        // `role` left out: accepted and obeyed (`docs/model-routing.md`,
        // "Google Generative AI wire facts").
        body.insert(
            "systemInstruction".into(),
            json!({"parts": [{"text": request.system_prompt}]}),
        );
    }
    body.insert("contents".into(), Value::Array(contents(endpoint, request)));
    if !tools.is_empty() {
        let declarations: Vec<Value> = wire_tools(&request.tools)
            .into_iter()
            .map(Value::Object)
            .collect();
        body.insert(
            "tools".into(),
            json!([{ "functionDeclarations": declarations }]),
        );
        let strict = tools.iter().all(|tool| strict::fits(&tool.input_schema));
        body.insert(
            "toolConfig".into(),
            json!({ "functionCallingConfig": function_calling(&request.tool_choice, strict) }),
        );
    }
    let mut generation = Map::new();
    let mut thinking = Map::new();
    // Thought summaries come back as `thought` parts, the readable
    // reasoning (ai.google.dev/api/generate-content, `ThinkingConfig`:
    // "thoughts are returned only when available").
    thinking.insert("includeThoughts".into(), json!(true));
    // `thinkingLevel` is Gemini 3's dialect; `Off` sends a zero budget with
    // no level.
    if let Some(level) = &request.thinking {
        let (field, value) = if *level == contract::ThinkingLevel::Off {
            ("thinkingBudget", json!(0))
        } else {
            ("thinkingLevel", json!(level.as_str().to_ascii_uppercase()))
        };
        thinking.insert(field.into(), value);
    }
    generation.insert("thinkingConfig".into(), Value::Object(thinking));
    body.insert("generationConfig".into(), Value::Object(generation));

    // Extra fields, added last. A declared `generationConfig` is laid over
    // Fiber's own key by key rather than replacing it.
    for (key, value) in &endpoint.extra_body {
        match (body.get_mut(key), value) {
            (Some(Value::Object(ours)), Value::Object(theirs)) if key == "generationConfig" => {
                ours.extend(theirs.clone());
            }
            _ => {
                body.insert(key.clone(), value.clone());
            }
        }
    }
    // The output limit is the model's, or the model data's own when lower
    // (`docs/errors.md`, "Output tokens").
    if let Some(limit) = endpoint.output_limit(request.max_output_tokens) {
        let generation = body
            .entry("generationConfig")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(generation) = generation {
            let max = generation
                .get("maxOutputTokens")
                .and_then(Value::as_u64)
                .map_or(limit, |n| n.min(limit));
            generation.insert("maxOutputTokens".into(), json!(max));
        }
    }
    Value::Object(body).to_string().into_bytes()
}

/// `functionCallingConfig` for Fiber's tool choice. `auto` is `VALIDATED`,
/// constrained decoding that still lets the model answer in text
/// (ai.google.dev/api/caching, `FunctionCallingConfig`; measured in
/// `docs/model-routing.md`, "Google Generative AI wire facts"), when every
/// tool's schema fits the strict subset; otherwise `AUTO`. Any other string
/// names the one tool to force.
fn function_calling(choice: &str, strict: bool) -> Value {
    match choice {
        "auto" if strict => json!({"mode": "VALIDATED"}),
        "auto" => json!({"mode": "AUTO"}),
        "none" => json!({"mode": "NONE"}),
        "any" => json!({"mode": "ANY"}),
        name => json!({"mode": "ANY", "allowedFunctionNames": [name]}),
    }
}

/// The conversation as Gemini `contents`: consecutive inputs of one role
/// fold into one content, so a turn's function responses share one `user`
/// content.
fn contents(endpoint: &Endpoint, request: &ModelRequest) -> Vec<Value> {
    let reference = endpoint.reference();
    // Each call with the model reference that produced it. An orphan result
    // keeps today's native rendering.
    let calls: BTreeMap<&ActionId, (&ToolCallRequested, &String)> = request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolCall {
                action_id,
                call,
                model,
            } => Some((action_id, (call, model))),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolResult { .. } => None,
        })
        .collect();

    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    // A signature on a `functionCall`, waiting for that call. It never
    // rides onto text or into the next reply.
    let mut signature = None;
    for input in &request.conversation {
        match input {
            Input::User { text, .. } => {
                park(&mut out, &mut signature);
                push(&mut out, "user", json!({"text": text}));
            }
            Input::Assistant {
                model,
                text,
                provider_item,
            } => {
                if *model == reference
                    && let Some(item) = provider_item
                {
                    push(&mut out, "model", item.clone());
                } else if !text.is_empty() {
                    push(&mut out, "model", json!({"text": text}));
                }
            }
            // Reasoning goes back unchanged, only to the model that
            // produced it, and never as plain text. A bare signature waits
            // for its call.
            Input::Reasoning {
                model,
                provider_item: Some(item),
                ..
            } if *model == reference => {
                if let Some(sig) = waiting_signature(item) {
                    if let Some(earlier) = signature.replace(sig) {
                        push(&mut out, "model", carrier(earlier));
                    }
                } else {
                    push(&mut out, "model", item.clone());
                }
            }
            Input::Reasoning { .. } => {}
            Input::ToolCall { call, model, .. } => {
                // A call another model reference made goes as plain text:
                // it carries no thought signature (`docs/loop.md`, "What
                // the model is sent"). Parked first, so a waiting signature
                // never rides onto a later own call.
                if *model != reference {
                    park(&mut out, &mut signature);
                    let args = call.arguments.to_string();
                    push(
                        &mut out,
                        "user",
                        json!({"text": format!("{model} called the tool {} with arguments {args}", call.name)}),
                    );
                    continue;
                }
                // Only an id the model emitted is sent back
                // (`docs/model-routing.md`, "Google Generative AI wire facts").
                let function = with(
                    json!({"name": call.name, "args": call.arguments}),
                    call.provider_id.as_ref().map(|id| json!({"id": id.0})),
                );
                push(
                    &mut out,
                    "model",
                    with(json!({ "functionCall": function }), signature.take()),
                );
            }
            Input::ToolResult {
                action_id,
                text,
                is_error,
                images,
            } => {
                park(&mut out, &mut signature);
                let found = calls.get(action_id).copied();
                let call = found.map(|(call, _)| call);
                let prepared =
                    crate::images::prepare(text, images, &request.session_dir, endpoint.text_only);
                // A result whose call another model reference made goes as
                // plain text (`docs/model-routing.md`, "Google Generative
                // AI wire facts"). An orphan keeps today's native rendering.
                if let Some((made, made_model)) = found
                    && *made_model != reference
                {
                    let line = if *is_error { "failed" } else { "returned" };
                    push(
                        &mut out,
                        "user",
                        json!({"text": format!("The tool {} {line}:\n{}", made.name, prepared.text)}),
                    );
                    for image in &prepared.images {
                        push(
                            &mut out,
                            "user",
                            json!({"inlineData": {"mimeType": image.mime_type, "data": image.data}}),
                        );
                    }
                    continue;
                }
                // A failed call sends the documented `error` key in place
                // of `output` (googleapis
                // `google/ai/generativelanguage/v1beta/content.proto`,
                // `FunctionResponse.response`: "if the function call failed
                // to execute, the response can have an \"error\" key").
                let result = if *is_error {
                    json!({"error": prepared.text})
                } else {
                    json!({"output": prepared.text})
                };
                let mut response = json!({
                    "name": call.map_or("", |c| c.name.as_str()),
                    "response": result,
                });
                // An image rides in `parts`, beside the `response` the
                // model read (`docs/model-routing.md`, "Google Generative
                // AI wire facts").
                if !prepared.images.is_empty() {
                    let parts: Vec<Value> = prepared.images.iter().map(|image| {
                        json!({"inlineData": {"mimeType": image.mime_type, "data": image.data}})
                    }).collect();
                    if let Some(map) = response.as_object_mut() {
                        map.insert("parts".into(), Value::Array(parts));
                    }
                }
                let response = with(
                    response,
                    call.and_then(|c| c.provider_id.as_ref())
                        .map(|id| json!({"id": id.0})),
                );
                push(&mut out, "user", json!({ "functionResponse": response }));
            }
        }
    }
    park(&mut out, &mut signature);
    out.into_iter()
        .map(|(role, parts)| json!({"role": role, "parts": parts}))
        .collect()
}

/// Sends a call signature that never met its call, on an empty text part,
/// and clears it so it cannot ride onto the next part.
fn park(out: &mut Vec<(&'static str, Vec<Value>)>, signature: &mut Option<Value>) {
    if let Some(sig) = signature.take() {
        push(out, "model", carrier(sig));
    }
}

/// Adds `part` to the last content when it has `role`, or opens one.
fn push(out: &mut Vec<(&'static str, Vec<Value>)>, role: &'static str, part: Value) {
    match out.last_mut() {
        Some((last, parts)) if *last == role => parts.push(part),
        _ => out.push((role, vec![part])),
    }
}

/// A bare `{"thoughtSignature": ...}` waiting for the `functionCall` it
/// arrived on. A text part's signature is the part itself, not this.
fn waiting_signature(item: &Value) -> Option<Value> {
    let map = item.as_object()?;
    let sig = map.get("thoughtSignature")?;
    if map.len() == 1 {
        return Some(json!({ "thoughtSignature": sig }));
    }
    None
}

/// `object` with the fields of `extra` added, such as a signature that
/// arrived on it or the id the model gave a call.
pub(crate) fn with(mut object: Value, extra: Option<Value>) -> Value {
    if let (Some(Value::Object(extra)), Some(map)) = (extra, object.as_object_mut()) {
        map.extend(extra);
    }
    object
}

/// A signature with no part left to ride on, sent on an empty text part, as
/// a stream's last part carries one (`research/google-generative-ai-probe`,
/// `raw/sse2-stream-ok.json`).
fn carrier(signature: Value) -> Value {
    with(json!({"text": ""}), Some(signature))
}
