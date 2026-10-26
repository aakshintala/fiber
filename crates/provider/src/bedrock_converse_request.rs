//! The `bedrock-converse` request body (`bedrock_converse`): the system
//! prompt, the conversation as Converse's content blocks, the tools, the
//! output limit and a thinking level (`docs/model-routing.md`, "Bedrock
//! Converse wire facts").

use std::collections::{BTreeMap, BTreeSet};

use contract::diag::PrefixHashes;
use contract::provider::{Input, Media, ModelRequest, ToolDefinition};
use contract::{ActionId, ThinkingLevel};
use serde_json::{Map, Value, json};

use crate::images::{Encoded, PdfForm};
use crate::{Endpoint, ReasoningField};

/// The request fields `extra_body` is laid over key by key rather than
/// replacing: Fiber's own `maxTokens` and thinking field sit inside them.
const MERGED: [&str; 2] = ["inferenceConfig", "additionalModelRequestFields"];

/// Each tool as a Converse `toolSpec`, in name order, with no `strict`:
/// Nova refused the field, `true` or `false` (`docs/model-routing.md`,
/// "Bedrock Converse wire facts"). An empty description is left out, since
/// Converse documents a description of at least one character. This is the
/// tools Fiber builds.
pub(crate) fn wire_tools(tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
    crate::wire::sorted_by_name(tools)
        .into_iter()
        .map(|tool| {
            let mut spec = Map::new();
            spec.insert("name".into(), json!(tool.name));
            if !tool.description.is_empty() {
                spec.insert("description".into(), json!(tool.description));
            }
            spec.insert(
                "inputSchema".into(),
                json!({"json": tool.input_schema.clone()}),
            );
            crate::wire::object(json!({"toolSpec": spec}))
        })
        .collect()
}

/// Whether the request carries `maxTokens`: always, except on a model
/// whose thinking goes in `reasoningConfig` at `high`, which Nova 2 Lite
/// refuses beside `maxTokens` (`docs/model-routing.md`, "Thinking").
pub(crate) fn sends_output_limit(endpoint: &Endpoint, request: &ModelRequest) -> bool {
    !(endpoint.compat.reasoning_field == Some(ReasoningField::ReasoningConfig)
        && request.thinking == Some(ThinkingLevel::High))
}

/// The tools as sent: the rewound build verbatim, else the tools Fiber
/// builds. Shared by the body and the digests, so both hash the same
/// bytes.
fn wired_tools(request: &ModelRequest) -> Vec<Value> {
    request
        .sent_tools
        .clone()
        .unwrap_or_else(|| wire_tools(&request.tools))
        .into_iter()
        .map(Value::Object)
        .collect()
}

/// The `system` blocks as sent, with a cache point after the text on a
/// model that takes one: absent when the request sends none. Shared by the
/// body and the digests, so both hash the same bytes.
fn system(endpoint: &Endpoint, request: &ModelRequest) -> Option<Value> {
    if request.system_prompt.is_empty() {
        return None;
    }
    let mut system = vec![json!({"text": request.system_prompt})];
    if endpoint.compat.cache_point {
        system.push(cache_point());
    }
    Some(Value::Array(system))
}

/// The redacted prefix digests of the encoded segments: the `system`
/// blocks, the `toolConfig` tools and each cumulative `messages` prefix as
/// sent (`docs/prompt-cache.md`, "Cache diagnostics"). Either of the first
/// two is absent when the request sends none.
pub(crate) fn hashes(endpoint: &Endpoint, request: &ModelRequest) -> PrefixHashes {
    let tools = wired_tools(request);
    PrefixHashes {
        system: system(endpoint, request).map(|system| crate::cache_diagnostics::segment(&system)),
        tools: (!tools.is_empty()).then(|| crate::cache_diagnostics::segment(&Value::Array(tools))),
        messages: crate::cache_diagnostics::cumulative(&messages(endpoint, request)),
    }
}

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes").
pub(crate) fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
    // A rewound session's first request sends its parent's logged build
    // verbatim, so it matches the parent's bytes (`docs/events.md`,
    // "Rewind").
    let tools = wired_tools(request);
    let mut body = Map::new();
    if let Some(system) = system(endpoint, request) {
        body.insert("system".into(), system);
    }
    body.insert("messages".into(), Value::Array(messages(endpoint, request)));
    if !tools.is_empty() {
        let mut config = Map::new();
        config.insert("tools".into(), Value::Array(tools));
        if let Some(choice) = tool_choice(&request.tool_choice) {
            config.insert("toolChoice".into(), choice);
        }
        body.insert("toolConfig".into(), Value::Object(config));
    }
    if let (Some(field), Some(level)) = (endpoint.compat.reasoning_field, request.thinking)
        && level != ThinkingLevel::Off
    {
        body.insert(
            "additionalModelRequestFields".into(),
            reasoning(field, level),
        );
    }
    for (key, value) in &endpoint.extra_body {
        match (body.get_mut(key), value) {
            (Some(Value::Object(ours)), Value::Object(theirs))
                if MERGED.contains(&key.as_str()) =>
            {
                ours.extend(theirs.clone());
            }
            _ => {
                body.insert(key.clone(), value.clone());
            }
        }
    }
    // The output limit is the model's, or the model data's own when lower
    // (`docs/errors.md`, "Output tokens"); a request that may not carry one
    // sends none, whatever the data says.
    let sends = sends_output_limit(endpoint, request);
    if let Value::Object(config) = body
        .entry("inferenceConfig")
        .or_insert_with(|| Value::Object(Map::new()))
    {
        if !sends {
            config.remove("maxTokens");
        } else if let Some(limit) = endpoint.output_limit(request.max_output_tokens) {
            let max = config
                .get("maxTokens")
                .and_then(Value::as_u64)
                .map_or(limit, |n| n.min(limit));
            config.insert("maxTokens".into(), json!(max));
        }
    }
    if body
        .get("inferenceConfig")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        body.remove("inferenceConfig");
    }
    Value::Object(body).to_string().into_bytes()
}

/// The thinking level in the field the model's data names.
fn reasoning(field: ReasoningField, level: ThinkingLevel) -> Value {
    match field {
        ReasoningField::ReasoningEffort => json!({"reasoning_effort": level.as_str()}),
        ReasoningField::ReasoningConfig => json!({
            "reasoningConfig": {"type": "enabled", "maxReasoningEffort": level.as_str()}
        }),
    }
}

/// `toolChoice` on the wire: left out for `auto`, Converse's default, and
/// for `none`, which Converse lacks; `any` is Converse's own; any other
/// string names the one tool to force.
fn tool_choice(choice: &str) -> Option<Value> {
    match choice {
        "auto" | "none" => None,
        "any" => Some(json!({"any": {}})),
        name => Some(json!({"tool": {"name": name}})),
    }
}

/// A cache marker: Converse's `cachePoint` block, with no `ttl`, which Nova
/// refuses (`docs/prompt-cache.md`, "Cache lifetime").
fn cache_point() -> Value {
    json!({"cachePoint": {"type": "default"}})
}

/// The conversation as Converse messages: consecutive inputs that belong to
/// the same role fold into one message, since a tool call and its result are
/// blocks inside an `assistant` or `user` message.
///
/// On a model with `compat.cache_point`, a `cachePoint` block follows the
/// block the previous request ended on and the last block of all, as
/// Anthropic's markers do, but only after a text block: Nova refused one
/// after a tool call or a tool result (`docs/prompt-cache.md`, "Cache markers
/// and keys").
fn messages(endpoint: &Endpoint, request: &ModelRequest) -> Vec<Value> {
    let reference = endpoint.reference();
    // A call's `toolUseId` is the provider's id for it, or its action id
    // when the reply carried none or one a Mistral model would refuse; its
    // result names the same one.
    let call_ids: BTreeMap<&ActionId, &str> = request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolCall {
                action_id, call, ..
            } => Some((
                action_id,
                call.provider_id
                    .as_ref()
                    .map(|id| id.0.as_str())
                    .filter(|id| ends_in_nine_letters_and_digits(id))
                    .unwrap_or(action_id.0.as_str()),
            )),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();

    // The messages, and each conversation index's last block as (message,
    // index within that message's blocks), `None` where the input produced
    // none.
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    let mut positions: Vec<Option<(usize, usize)>> = Vec::with_capacity(request.conversation.len());
    for input in request.conversation.iter() {
        let blocks = blocks_of(
            input,
            &reference,
            &call_ids,
            &request.media,
            endpoint.text_only,
        );
        if blocks.is_empty() {
            positions.push(None);
            continue;
        }
        let role = role_of(input);
        match out.last_mut() {
            Some((last, merged)) if *last == role => merged.extend(blocks),
            _ => out.push((role, blocks)),
        }
        let message = out.len() - 1;
        positions.push(out.last().map(|(_, blocks)| (message, blocks.len() - 1)));
    }

    if endpoint.compat.cache_point {
        let mut marks = BTreeSet::new();
        if let Some(previous_end) = request.previous_end
            && let Some(at) = (0..previous_end)
                .rev()
                .find_map(|i| positions.get(i).copied().flatten())
        {
            marks.insert(at);
        }
        if let Some((message, (_, blocks))) = out.iter().enumerate().next_back() {
            marks.insert((message, blocks.len() - 1));
        }
        // From the last mark back, so an inserted marker never moves one
        // still to place.
        for (message, block) in marks.into_iter().rev() {
            if let Some((_, blocks)) = out.get_mut(message)
                && blocks.get(block).is_some_and(|b| b.get("text").is_some())
            {
                blocks.insert(block + 1, cache_point());
            }
        }
    }

    out.into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect()
}

/// Whether an id ends in nine letters and digits, all a Mistral model reads
/// of a replayed `toolUseId` (`docs/model-routing.md`, "Bedrock Converse
/// wire facts"). Bedrock's own ids and Fiber's action ids do.
fn ends_in_nine_letters_and_digits(id: &str) -> bool {
    id.len() >= 9 && id.bytes().rev().take(9).all(|b| b.is_ascii_alphanumeric())
}

/// The Converse role an input belongs to.
fn role_of(input: &Input) -> &'static str {
    match input {
        Input::User { .. } | Input::ToolResult { .. } | Input::ToolSearchResult { .. } => "user",
        Input::Assistant { .. } | Input::Reasoning { .. } | Input::ToolCall { .. } => "assistant",
    }
}

/// One encoded image as a Converse `image` block.
fn image_block(image: &Encoded) -> Value {
    let format = image
        .mime_type
        .strip_prefix("image/")
        .unwrap_or(&image.mime_type);
    json!({"image": {"format": format, "source": {"bytes": image.data}}})
}

/// The content blocks an input renders as, empty for an input that adds
/// nothing to the request: an empty assistant text, or reasoning sent to a
/// different model reference (`docs/loop.md`, "What the model is sent").
fn blocks_of(
    input: &Input,
    reference: &str,
    call_ids: &BTreeMap<&ActionId, &str>,
    media: &Media,
    text_only: bool,
) -> Vec<Value> {
    match input {
        Input::User { text, images } => {
            let prepared =
                crate::images::prepare(text, images, &[], media, text_only, PdfForm::Pages);
            let mut blocks = Vec::new();
            if !prepared.text.is_empty() || prepared.images.is_empty() {
                blocks.push(json!({"text": prepared.text}));
            }
            blocks.extend(prepared.images.iter().map(image_block));
            blocks
        }
        Input::Assistant { text, .. } if text.is_empty() => Vec::new(),
        // Converse text carries nothing of its own, so a part goes back as
        // its words to every model.
        Input::Assistant { text, .. } => vec![json!({"text": text})],
        // Reasoning goes back unchanged, in the form the decoder logged it,
        // only to the model that produced it.
        Input::Reasoning {
            model,
            provider_item,
            ..
        } if model == reference => provider_item.clone().into_iter().collect(),
        Input::Reasoning { .. } => Vec::new(),
        Input::ToolCall {
            action_id, call, ..
        } => vec![json!({"toolUse": {
            "toolUseId": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
            "name": call.name,
            // A tool call's `input` is an object: one whose arguments are
            // not an object replays as an empty object, so the provider
            // accepts the request and the model corrects itself.
            "input": crate::wire::object(call.arguments.clone()),
        }})],
        Input::ToolSearchResult { action_id, loaded } => result_blocks(
            call_ids
                .get(action_id)
                .copied()
                .unwrap_or(action_id.0.as_str()),
            &contract::provider::loaded_text(loaded),
            false,
            &[],
            &[],
            media,
            text_only,
        ),
        Input::ToolResult {
            action_id,
            text,
            is_error,
            images,
            pdfs,
        } => result_blocks(
            call_ids
                .get(action_id)
                .copied()
                .unwrap_or(action_id.0.as_str()),
            text,
            *is_error,
            images,
            pdfs,
            media,
            text_only,
        ),
    }
}

/// A tool result: its `toolResult` block, with `status: "error"` for a
/// failed call, then its images and a PDF's rendered pages as `image`
/// blocks beside it, which Mistral and Qwen models take where they refuse
/// one inside it (`docs/model-routing.md`, "Bedrock Converse wire facts").
fn result_blocks(
    id: &str,
    text: &str,
    is_error: bool,
    images: &[contract::provider::ImageRef],
    pdfs: &[contract::provider::PdfRef],
    media: &Media,
    text_only: bool,
) -> Vec<Value> {
    let prepared = crate::images::prepare(text, images, pdfs, media, text_only, PdfForm::Pages);
    let mut result = json!({"toolUseId": id, "content": [{"text": prepared.text}]});
    if is_error && let Some(map) = result.as_object_mut() {
        map.insert("status".into(), json!("error"));
    }
    let mut blocks = vec![json!({"toolResult": result})];
    blocks.extend(prepared.images.iter().map(image_block));
    blocks
}
