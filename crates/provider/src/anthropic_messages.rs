//! The `anthropic-messages` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): Anthropic, OpenCode, OpenRouter, Databricks, muse, AWS
//! Bedrock, Google Vertex and Azure Foundry. A request is a `POST` to
//! `<base_url>/messages`; the reply is a server-sent event stream that ends
//! with `message_stop`, which always follows the `message_delta` carrying
//! the stop reason (`research/anthropic-messages-probe`).

use std::collections::BTreeMap;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;

use contract::ActionId;
use contract::events::CacheLifetime;
use contract::provider::{
    CallError, Delta, Input, InputSize, ModelCall, ModelRequest, Provider, Reply, ToolDefinition,
};
use serde_json::{Map, Value, json};

pub use crate::anthropic_messages_decode::decode;
use crate::http::{self, Cancel};
use crate::redact::Secrets;
use crate::{Endpoint, Error, strict};

/// The wire version every request declares (`research/anthropic-messages-probe`).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One model reached over `anthropic-messages`.
#[derive(Debug, Clone)]
pub struct Messages {
    endpoint: Endpoint,
    cache_key_header: Option<String>,
}

impl Messages {
    /// The protocol for one model of one provider.
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            cache_key_header: None,
        }
    }

    /// Also sends each request's cache key in the header `name`, for a
    /// provider that routes by it, such as OpenCode's `x-opencode-session`
    /// (`docs/prompt-cache.md`, "Cache markers and keys").
    #[must_use]
    pub fn cache_key_header(mut self, name: impl Into<String>) -> Self {
        self.cache_key_header = Some(name.into());
        self
    }

    /// Builds the call for `request`. Two calls built from the same inputs
    /// send the same bytes (`docs/prompt-cache.md`, "Bytes").
    pub fn request(&self, request: &ModelRequest) -> Call {
        let endpoint = &self.endpoint;
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("accept".to_owned(), "text/event-stream".to_owned()),
            ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
            (
                "user-agent".to_owned(),
                concat!("fiber/", env!("CARGO_PKG_VERSION")).to_owned(),
            ),
        ];
        if let Some(key) = &endpoint.key {
            headers.push(("x-api-key".to_owned(), key.expose().to_owned()));
        }
        headers.extend(endpoint.headers.iter().cloned());
        if let Some(name) = &self.cache_key_header {
            headers.push((name.clone(), request.cache_key.clone()));
        }
        let body = body(endpoint, request);
        Call {
            url: format!("{}/messages", endpoint.base_url.trim_end_matches('/')),
            headers,
            input_size: crate::images::input_size(&body, request, endpoint.text_only),
            body,
            provider: endpoint.provider.clone(),
            signer: endpoint.signer.clone(),
            direct: endpoint.direct,
            cancel: Arc::default(),
            secrets: endpoint.secrets(),
        }
    }
}

impl Provider for Messages {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        wire_tools(tools)
    }
}

/// One `anthropic-messages` call, ready to send.
pub struct Call {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    input_size: InputSize,
    provider: String,
    signer: Option<Arc<dyn contract::signing::Signer>>,
    direct: bool,
    cancel: Arc<Cancel>,
    secrets: Secrets,
}

impl Call {
    /// Sends the request and returns the reply's bytes, unread.
    pub fn open(&self) -> Result<impl Read + use<>, Error> {
        let mut secrets = self.secrets.clone();
        http::post_signed(
            &self.url,
            &self.headers,
            &self.body,
            self.signer.as_deref(),
            self.direct,
            &self.cancel,
            &mut secrets,
        )
        .map(|(body, _)| body)
    }
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let mut secrets = self.secrets.clone();
        let (reply, should_retry) = match http::post_signed(
            &self.url,
            &self.headers,
            &self.body,
            self.signer.as_deref(),
            self.direct,
            &self.cancel,
            &mut secrets,
        ) {
            Ok((stream, should_retry)) => (decode(BufReader::new(stream), sink), should_retry),
            Err(e) => {
                let should_retry = e.should_retry();
                (Err(e), should_retry)
            }
        };
        // Whatever a cancelled call returns, the cancel ended it.
        if self.cancel.is_cancelled() {
            return Err(CallError::Cancelled { usage: None });
        }
        reply
            .map(|reply| Reply {
                input_size: self.input_size,
                ..reply
            })
            .map_err(|e| CallError::Failed {
                failure: e.failure(&self.provider, &secrets),
                should_retry,
                usage: None,
            })
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Each tool in Anthropic's shape, in name order.
/// `strict` per tool (`docs/model-routing.md`, "Protocols and
/// providers"). Anthropic's strict subset is wider than OpenAI's (it
/// takes optional properties, `anyOf` and `$ref`), so a schema that fits
/// `strict::fits` fits Anthropic's too, except an enum with an object or
/// array value, which Anthropic excludes (platform.claude.com, "JSON
/// Schema limitations": "complex types in enums"). Anthropic refuses a request with
/// more than 20 strict tools (probed 2026-10-01 on `claude-sonnet-5-5`:
/// "The maximum number of strict tools supported is 20"), so past 20 the
/// rest are sent `strict: false`, in name order. This is the tools Fiber
/// builds.
fn wire_tools(tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
    let mut sorted: Vec<&ToolDefinition> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut strict_left = MAX_STRICT_TOOLS;
    sorted
        .into_iter()
        .map(|tool| {
            // A hosted tool is the vendor's own: its type and name only, no
            // schema, and no strict slot.
            if let Some(kind) = &tool.hosted {
                return json!({"name": tool.name, "type": kind})
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
            }
            let strict = strict_left > 0
                && strict::fits(&tool.input_schema)
                && !complex_enum(&tool.input_schema);
            if strict {
                strict_left -= 1;
            }
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
                "strict": strict,
            })
            .as_object()
            .cloned()
            .unwrap_or_default()
        })
        .collect()
}

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes").
///
/// debt: deferred tools are sent in full, without defer_loading, until tool
/// search is built (#368); nothing defers a tool yet.
fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
    let tools: Vec<Value> = wire_tools(&request.tools)
        .into_iter()
        .map(Value::Object)
        .collect();
    let mut body = Map::new();
    body.insert("model".into(), json!(endpoint.model));
    if !request.system_prompt.is_empty() {
        let mut system = json!({"type": "text", "text": request.system_prompt});
        mark(&mut system, &request.cache_lifetime);
        body.insert("system".into(), Value::Array(vec![system]));
    }
    body.insert("messages".into(), Value::Array(messages(endpoint, request)));
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    body.insert("tool_choice".into(), tool_choice(&request.tool_choice));
    body.insert("stream".into(), json!(true));
    // Anthropic takes adaptive thinking plus an effort, not a token budget
    // (`research/anthropic-messages-probe`: "Use thinking.type.adaptive and
    // output_config.effort").
    if let Some(level) = &request.thinking
        && *level != contract::ThinkingLevel::Off
    {
        body.insert("thinking".into(), json!({"type": "adaptive"}));
        body.insert("output_config".into(), json!({"effort": level.as_str()}));
    }
    body.extend(endpoint.extra_body.clone());
    // Anthropic requires `max_tokens`. It is the model's limit, or the
    // model data's own `max_tokens` when that is lower (`docs/errors.md`,
    // "Output tokens").
    if let Some(limit) = endpoint.output_limit(request.max_output_tokens) {
        let max = body
            .get("max_tokens")
            .and_then(Value::as_u64)
            .map_or(limit, |n| n.min(limit));
        body.insert("max_tokens".into(), json!(max));
    }
    Value::Object(body).to_string().into_bytes()
}

/// Whether any `enum` in `schema` has an object or array value.
pub(crate) fn complex_enum(schema: &Value) -> bool {
    match schema {
        Value::Object(map) => map.iter().any(|(key, value)| {
            (key == "enum"
                && value
                    .as_array()
                    .is_some_and(|values| values.iter().any(|v| v.is_object() || v.is_array())))
                || complex_enum(value)
        }),
        Value::Array(items) => items.iter().any(complex_enum),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

/// The most tools Anthropic takes with `strict: true` in one request.
pub(crate) const MAX_STRICT_TOOLS: usize = 20;

/// `tool_choice` on the wire: `auto`, `none` and `any` are Anthropic's own
/// values; any other string names the one tool to force
/// (`docs/prompt-cache.md`, "Tools"; unprobed past `auto` and `none`,
/// `research/anthropic-messages-probe`, "`tool_choice` with no `tools`").
fn tool_choice(choice: &str) -> Value {
    match choice {
        "auto" | "none" | "any" => json!({"type": choice}),
        name => json!({"type": "tool", "name": name}),
    }
}

/// `{"type": "ephemeral"}`, with `ttl` added for a 1-hour cache
/// (`docs/prompt-cache.md`, "Cache lifetime"; no beta header is needed,
/// `research/provider-harvest/anthropic-messages.md`, "TTL values").
pub(crate) fn cache_control(lifetime: &CacheLifetime) -> Value {
    match lifetime {
        CacheLifetime::FiveMinutes => json!({"type": "ephemeral"}),
        CacheLifetime::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
    }
}

/// Marks `block` with a cache breakpoint, when it is an object (every block
/// Fiber builds is).
fn mark(block: &mut Value, lifetime: &CacheLifetime) {
    if let Some(map) = block.as_object_mut() {
        map.insert("cache_control".into(), cache_control(lifetime));
    }
}

/// The conversation as Anthropic messages: consecutive inputs that belong to
/// the same role are folded into one message, because Anthropic's tool
/// results and tool calls are blocks inside a `user` or `assistant` message,
/// not items of their own.
///
/// Carries up to three cache markers (`docs/prompt-cache.md`, "Cache markers
/// and keys"): the end of the system prompt (in `body`, not here), the point
/// where the previous request ended (`request.previous_end`, when this is
/// not the first request), and the new end, the last block of all. Fiber's
/// own design has no marker on the last tool definition, unlike pi and rig
/// (`research/provider-harvest/anthropic-messages.md`, "Prompt-cache
/// markers").
fn messages(endpoint: &Endpoint, request: &ModelRequest) -> Vec<Value> {
    let reference = endpoint.reference();
    // A call's `id` is the provider's id for it, or its action id when the
    // reply carried none; its result names the same one.
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
                    .map_or(action_id.0.as_str(), |id| id.0.as_str()),
            )),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolResult { .. } => None,
        })
        .collect();

    // The messages, and each conversation index's block as (message, index
    // within that message's blocks), `None` where the input produced none.
    // An input with no block starts no message, so a turn that was all
    // drops leaves the messages either side of it as one.
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    let mut positions: Vec<Option<(usize, usize)>> = Vec::with_capacity(request.conversation.len());
    for input in &request.conversation {
        let blocks = block_of(
            input,
            &reference,
            &call_ids,
            &request.session_dir,
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

    if let Some(previous_end) = request.previous_end {
        // The last block the previous request ended on: the nearest input at
        // or before `previous_end` that produced one, since the boundary
        // itself may land on a drop.
        if let Some((message, block)) = (0..previous_end)
            .rev()
            .find_map(|i| positions.get(i).copied().flatten())
        {
            #[allow(
                clippy::indexing_slicing,
                reason = "positions holds only in-range indices"
            )]
            mark(&mut out[message].1[block], &request.cache_lifetime);
        }
    }
    if let Some(last) = out.last_mut().and_then(|(_, blocks)| blocks.last_mut()) {
        mark(last, &request.cache_lifetime);
    }

    out.into_iter()
        .map(|(role, blocks)| message(role, blocks))
        .collect()
}

/// The Anthropic role an input belongs to: tool results are a `user` block,
/// like a person's message, and a tool call is an `assistant` block, like
/// the model's own text.
fn role_of(input: &Input) -> &'static str {
    match input {
        Input::User { .. } | Input::ToolResult { .. } => "user",
        Input::Assistant { .. } | Input::Reasoning { .. } | Input::ToolCall { .. } => "assistant",
    }
}

/// The content blocks an input renders as, empty for an input that adds
/// nothing to the request: an empty assistant text, or reasoning sent to a
/// different model reference (`docs/loop.md`, "What the model is sent").
/// A person's blocks stay contiguous in one message: its text block, when
/// the prepared text is non-empty, then one `image` block per image.
fn block_of(
    input: &Input,
    reference: &str,
    call_ids: &BTreeMap<&ActionId, &str>,
    session_dir: &Path,
    text_only: bool,
) -> Vec<Value> {
    match input {
        Input::User { text, images } => {
            let prepared = crate::images::prepare(text, images, session_dir, text_only);
            if prepared.images.is_empty() {
                return vec![json!({"type": "text", "text": prepared.text})];
            }
            let mut blocks = Vec::new();
            if !prepared.text.is_empty() {
                blocks.push(json!({"type": "text", "text": prepared.text}));
            }
            for image in &prepared.images {
                blocks.push(json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": image.mime_type, "data": image.data},
                }));
            }
            blocks
        }
        // A part's own form (a hosted call or result, a text with citations)
        // goes back unchanged, only to the model that produced it.
        Input::Assistant {
            model,
            provider_item: Some(item),
            ..
        } if model == reference => vec![item.clone()],
        Input::Assistant { text, .. } if text.is_empty() => Vec::new(),
        Input::Assistant { text, .. } => vec![json!({"type": "text", "text": text})],
        // Reasoning goes back unchanged, only to the model that produced it,
        // and never as plain text.
        Input::Reasoning {
            model,
            provider_item,
            ..
        } if model == reference => provider_item.clone().into_iter().collect(),
        Input::Reasoning { .. } => Vec::new(),
        Input::ToolCall {
            action_id, call, ..
        } => vec![json!({
            "type": "tool_use",
            "id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
            "name": call.name,
            "input": call.arguments,
        })],
        Input::ToolResult {
            action_id,
            text,
            is_error,
            images,
        } => {
            // A failed tool result sends Anthropic's `is_error` flag; a
            // success sends no such key
            // (platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls:
            // "`is_error` (optional): Set to `true` if the tool execution
            // resulted in an error.").
            let mut result = json!({
                "type": "tool_result",
                "tool_use_id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
                "content": crate::images::anthropic_content(crate::images::prepare(text, images, session_dir, text_only)),
            });
            if *is_error && let Some(map) = result.as_object_mut() {
                map.insert("is_error".into(), json!(true));
            }
            vec![result]
        }
    }
}

/// One message of `blocks`, collapsing an unmarked single text block to a
/// plain string (`research/anthropic-messages-probe`: every simple request
/// sent `content` as a string, never a one-element array). A cache marker
/// adds a second key, so a marked block keeps its array form.
fn message(role: &'static str, blocks: Vec<Value>) -> Value {
    let content = match blocks.as_slice() {
        [one] if one.as_object().is_some_and(|o| o.len() == 2) && one["type"] == "text" => {
            one["text"].clone()
        }
        _ => Value::Array(blocks),
    };
    json!({"role": role, "content": content})
}
