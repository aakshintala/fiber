//! The `anthropic-messages` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): Anthropic, OpenCode, OpenRouter, Databricks, muse, AWS
//! Bedrock, Google Vertex and Azure Foundry. A request is a `POST` to
//! `<base_url>/messages`; the reply is a server-sent event stream that ends
//! with `message_stop`, which always follows the `message_delta` carrying
//! the stop reason (`research/anthropic-messages-probe`). A model that
//! declares `compat.bedrock_invoke` is Claude on AWS Bedrock's Invoke API
//! instead: a `POST` to `<base_url>/model/<id>/invoke-with-response-stream`,
//! whose reply carries the same events in AWS event-stream framing
//! (`research/bedrock-invoke-probe`).
//! One that declares `compat.vertex_raw_predict` is Claude on Google Vertex:
//! a `POST` to `<base_url>/models/<id>:streamRawPredict` whose reply is the
//! same event stream (`docs/model-routing.md`, "Google Vertex wire facts").

use std::collections::BTreeMap;

use contract::ActionId;
use contract::diag::PrefixHashes;
use contract::events::CacheLifetime;
use contract::provider::{
    Input, Media, ModelCall, ModelRequest, Provider, TOOL_SEARCH, ToolDefinition,
};
use serde_json::{Map, Value, json};

#[cfg(any(test, feature = "test-hooks"))]
pub use crate::anthropic_messages_decode::decode;
use crate::anthropic_messages_decode::{decode_bedrock_tracked, decode_tracked};
pub use crate::call::Call;
use crate::{Endpoint, Error, strict};

/// The wire version every request declares (`research/anthropic-messages-probe`).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The wire version a Bedrock Invoke request declares in its body, where
/// Bedrock reads it in place of the `anthropic-version` header.
const BEDROCK_VERSION: &str = "bedrock-2023-05-31";

/// The media type a Bedrock Invoke reply streams in.
const EVENT_STREAM: &str = "application/vnd.amazon.eventstream";

/// The wire version a Vertex request declares in its body, where Vertex
/// reads it (`docs/model-routing.md`, "Google Vertex wire facts").
const VERTEX_VERSION: &str = "vertex-2023-10-16";

/// One model reached over `anthropic-messages`.
#[derive(Debug, Clone)]
pub struct Messages {
    endpoint: Endpoint,
    cache_key_header: Option<String>,
    deferred_tools: bool,
}

impl Messages {
    /// The protocol for one model of one provider.
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            cache_key_header: None,
            deferred_tools: false,
        }
    }

    /// Whether the model defers tools: its provider data says `defer_loading`
    /// works (`docs/tools.md`, "Deferral is a property of the model"). Off,
    /// every tool is declared in full. On, a deferred tool is declared with
    /// `defer_loading` and `tool_search` results go back as `tool_reference`
    /// blocks (`research/tool-search`).
    #[must_use]
    pub fn deferred_tools(mut self, on: bool) -> Self {
        self.deferred_tools = on;
        self
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
    #[allow(
        clippy::result_large_err,
        reason = "the decode carries its partial alongside the error for the call's usage"
    )]
    pub fn request(&self, request: &ModelRequest) -> Call {
        let endpoint = &self.endpoint;
        let bedrock = endpoint.compat.bedrock_invoke;
        // A Bedrock API key is a bearer token (`docs/model-routing.md`,
        // "Protocols and providers").
        let auth = endpoint.key.as_ref().map(|key| {
            if endpoint.compat.bearer || bedrock {
                ("authorization", format!("Bearer {}", key.expose()))
            } else {
                ("x-api-key", key.expose().to_owned())
            }
        });
        let base_url = endpoint.base_url.trim_end_matches('/');
        let (accept, fixed, url, decode): (&str, &[(&str, &str)], String, crate::call::Decode) =
            if bedrock {
                (
                    EVENT_STREAM,
                    &[],
                    format!(
                        "{base_url}/model/{}/invoke-with-response-stream",
                        crate::wire::path_segment(&endpoint.model)
                    ),
                    |stream, _, sink| decode_bedrock_tracked(stream, sink),
                )
            } else if endpoint.compat.vertex_raw_predict {
                // Vertex names the model in the path, not the body.
                (
                    "text/event-stream",
                    &[("anthropic-version", ANTHROPIC_VERSION)],
                    format!("{base_url}/models/{}:streamRawPredict", endpoint.model),
                    |stream, _, sink| decode_tracked(stream, sink),
                )
            } else {
                (
                    "text/event-stream",
                    &[("anthropic-version", ANTHROPIC_VERSION)],
                    format!("{base_url}/messages"),
                    |stream, _, sink| decode_tracked(stream, sink),
                )
            };
        let headers = crate::call::base_headers(
            endpoint,
            accept,
            fixed,
            auth,
            self.cache_key_header.as_deref(),
            &request.cache_key,
        );
        let body = body(endpoint, request, self.deferred_tools);
        let call = Call::new(endpoint, request, url, headers, body, decode);
        if endpoint.compat.vertex_raw_predict {
            call.on_error(unlisted)
        } else {
            call
        }
    }
    /// Builds the observed call for `request`: as the plain request, writing
    /// one `provider_request` line when the call was sent. At `info` the
    /// call runs exactly as an unobserved one does, with no model cloned.
    pub fn call_observed(
        &self,
        request: &ModelRequest,
        observation: crate::RequestObservation,
    ) -> Box<dyn ModelCall> {
        let mut call = self.request(request);
        if !observation.debugging() {
            return Box::new(call);
        }
        // The digests cost a second encoding, so they run only with the
        // setting on; the hot path with it off is unchanged.
        if observation.cache_diagnostics() {
            call = call.with_hashes(hashes(&self.endpoint, request, self.deferred_tools));
        }
        Box::new(call.observed(observation.with_model(self.endpoint.model.clone())))
    }
}

impl Provider for Messages {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        wire_tools(tools, self.deferred_tools)
    }

    fn defers(&self) -> bool {
        self.deferred_tools
    }

    fn warms(&self, request: &ModelRequest) -> bool {
        // A refresh capped at one token would leave a token budget out,
        // changing the thinking parameters and paying for a rebuild
        // (`docs/prompt-cache.md`, "Warming while idle").
        !(self.endpoint.compat.thinking_budget
            && matches!(
                request.thinking,
                Some(level) if level != contract::ThinkingLevel::Off
            ))
    }
}

/// Each tool in Anthropic's shape, in name order.
/// `strict` per tool (`docs/model-routing.md`, "Protocols and
/// providers"). Anthropic's strict subset is wider than OpenAI's (it
/// takes optional properties, `anyOf` and `$ref`), so a schema that fits
/// `strict::fits` fits Anthropic's too (required nullable scalars probed in
/// `research/strict-nullable-probe/raw/P3_anthropic_messages.json`), except an enum with an object or
/// array value, which Anthropic excludes (platform.claude.com, "JSON
/// Schema limitations": "complex types in enums"). Anthropic refuses a request with
/// more than 20 strict tools (probed 2026-10-01 on `claude-sonnet-5-5`:
/// "The maximum number of strict tools supported is 20"), so past 20 the
/// rest are sent `strict: false`, in name order. This is the tools Fiber
/// builds.
///
/// With `deferring`, a deferred tool also carries `"defer_loading": true`,
/// except `tool_search` (Anthropic: "Never set `defer_loading: true` on the
/// tool search tool itself"), which an extension may mark deferred, and a
/// hosted tool, which has no definition to load (`research/tool-search`).
/// A deferred tool still spends a strict slot: the budget counts every
/// tool that fits.
fn wire_tools(tools: &[ToolDefinition], deferring: bool) -> Vec<Map<String, Value>> {
    let sorted = crate::wire::sorted_by_name(tools);
    let mut strict_left = MAX_STRICT_TOOLS;
    sorted
        .into_iter()
        .map(|tool| {
            // A hosted tool is the vendor's own: its type and name only, no
            // schema, and no strict slot.
            if let Some(kind) = &tool.hosted {
                return crate::wire::object(json!({"name": tool.name, "type": kind}));
            }
            let strict = strict_left > 0
                && strict::fits(&tool.input_schema)
                && !complex_enum(&tool.input_schema);
            if strict {
                strict_left -= 1;
            }
            let mut wired = crate::wire::object(json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
                "strict": strict,
            }));
            if deferring && tool.deferred && tool.name != TOOL_SEARCH {
                wired.insert("defer_loading".into(), json!(true));
            }
            wired
        })
        .collect()
}

/// The tools as sent: the rewound build verbatim, else the tools Fiber
/// builds. Shared by the body and the digests, so both hash the same
/// bytes.
fn wired_tools(request: &ModelRequest, deferring: bool) -> Vec<Value> {
    request
        .sent_tools
        .clone()
        .unwrap_or_else(|| wire_tools(&request.tools, deferring))
        .into_iter()
        .map(Value::Object)
        .collect()
}

/// The system prompt as sent, with its cache breakpoint: absent when the
/// request sends none. Shared by the body and the digests, so both hash
/// the same bytes.
fn system(request: &ModelRequest) -> Option<Value> {
    if request.system_prompt.is_empty() {
        return None;
    }
    let mut block = json!({"type": "text", "text": request.system_prompt});
    mark(&mut block, &request.cache_lifetime);
    Some(Value::Array(vec![block]))
}

/// The redacted prefix digests of the encoded segments: the system, the
/// tools and each cumulative messages prefix as sent. Tools are absent
/// from the body when the request sends none, so their digest is absent
/// too.
fn hashes(endpoint: &Endpoint, request: &ModelRequest, deferring: bool) -> PrefixHashes {
    let tools = wired_tools(request, deferring);
    PrefixHashes {
        system: system(request).map(|system| crate::cache_diagnostics::segment(&system)),
        tools: (!tools.is_empty()).then(|| crate::cache_diagnostics::segment(&Value::Array(tools))),
        messages: crate::cache_diagnostics::cumulative(&messages(endpoint, request)),
    }
}

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes").
fn body(endpoint: &Endpoint, request: &ModelRequest, deferring: bool) -> Vec<u8> {
    // A rewound session's first request sends its parent's logged build
    // verbatim, so it matches the parent's bytes (`docs/events.md`,
    // "Rewind").
    let tools = wired_tools(request, deferring);
    let bedrock = endpoint.compat.bedrock_invoke;
    let mut body = Map::new();
    // Bedrock's Invoke API names the model in the path and streams by the
    // path it is sent to, so its body carries neither, and declares its
    // version where the header would.
    if bedrock {
        body.insert("anthropic_version".into(), json!(BEDROCK_VERSION));
    } else if endpoint.compat.vertex_raw_predict {
        // Vertex reads the model from the path and the version from the body.
        body.insert("anthropic_version".into(), json!(VERTEX_VERSION));
    } else {
        body.insert("model".into(), json!(endpoint.model));
    }
    if let Some(system) = system(request) {
        body.insert("system".into(), system);
    }
    body.insert("messages".into(), Value::Array(messages(endpoint, request)));
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    body.insert("tool_choice".into(), tool_choice(&request.tool_choice));
    if !bedrock {
        body.insert("stream".into(), json!(true));
    }
    // The `max_tokens` the body ends up sending, resolved once so the
    // thinking budget and the insert below cannot disagree.
    let sent = sent_max_tokens(
        endpoint.output_limit(request.max_output_tokens),
        endpoint.extra_body.get("max_tokens"),
    );
    // Without `thinking_budget`, Anthropic takes adaptive thinking plus an
    // effort, not a token budget (`research/anthropic-messages-probe`:
    // "Use thinking.type.adaptive and output_config.effort"). With it,
    // the level goes in `thinking: {type: "enabled", budget_tokens}`
    // measured against the sent `max_tokens`, with no `output_config`.
    if endpoint.compat.thinking_budget {
        if let Some(level) = &request.thinking
            && let Some(budget) = budget_tokens(level, sent)
        {
            body.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget}),
            );
        }
    } else if let Some(level) = &request.thinking
        && *level != contract::ThinkingLevel::Off
    {
        body.insert("thinking".into(), json!({"type": "adaptive"}));
        body.insert("output_config".into(), json!({"effort": level.as_str()}));
    }
    body.extend(endpoint.extra_body.clone());
    // Anthropic requires `max_tokens`. It is the model's limit, or the
    // model data's own `max_tokens` when that is lower (`docs/errors.md`,
    // "Output tokens").
    if let Some(max) = sent {
        body.insert("max_tokens".into(), json!(max));
    }
    Value::Object(body).to_string().into_bytes()
}

/// The `max_tokens` the body ends up sending: the limit capped by an
/// integer `extra_body.max_tokens`, or that value alone with no limit.
/// `None` when the body sends no `max_tokens`, or sends one that is not
/// an unsigned integer, which the insert keeps as sent.
fn sent_max_tokens(limit: Option<u64>, extra: Option<&Value>) -> Option<u64> {
    match limit {
        Some(limit) => Some(
            extra
                .and_then(Value::as_u64)
                .map_or(limit, |n| n.min(limit)),
        ),
        None => extra.and_then(Value::as_u64),
    }
}

/// The token budget for `level` measured against the sent `max_tokens`
/// `m`: the level's budget with no `m`; no `thinking` key when no budget
/// is both at least 1024 and below `m`; otherwise the budget capped so at
/// least 1024 tokens stay for the reply. `None` means no `thinking` key.
fn budget_tokens(level: &contract::ThinkingLevel, max_tokens: Option<u64>) -> Option<u64> {
    use contract::ThinkingLevel::{High, Low, Max, Medium, Minimal, Off, Xhigh};
    let budget = match level {
        Off => return None,
        Minimal => 1024,
        Low => 2048,
        Medium => 8192,
        High | Xhigh | Max => 16384,
    };
    match max_tokens {
        None => Some(budget),
        Some(m) if m <= 1024 => None,
        Some(m) => Some(1024.max(budget.min(m - 1024))),
    }
}

/// A failure whose body is a one-element list holding an object, as
/// Vertex's `streamRawPredict` sends one, with that object as its body, so
/// the error codes read it as any provider's (`docs/model-routing.md`,
/// "Google Vertex wire facts"). Any other failure is kept as it came.
fn unlisted(mut error: Error) -> Error {
    if let Error::Status { body, .. } = &mut error
        && let Ok(Value::Array(items)) = serde_json::from_str::<Value>(body)
        && let [inner @ Value::Object(_)] = items.as_slice()
    {
        *body = inner.to_string();
    }
    error
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
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();

    // The messages, and each conversation index's block as (message, index
    // within that message's blocks), `None` where the input produced none.
    // An input with no block starts no message, so a turn that was all
    // drops leaves the messages either side of it as one.
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    let mut positions: Vec<Option<(usize, usize)>> = Vec::with_capacity(request.conversation.len());
    for input in request.conversation.iter() {
        let blocks = block_of(
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
    // A request that ends in an assistant message resends a paused reply:
    // no cache marker on it, so its raw blocks go back unchanged (and a
    // `thinking` last block may not carry one).
    let trailing_assistant = out.last().is_some_and(|(role, _)| *role == "assistant");
    if !trailing_assistant
        && let Some(last) = out.last_mut().and_then(|(_, blocks)| blocks.last_mut())
    {
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
        Input::User { .. } | Input::ToolResult { .. } | Input::ToolSearchResult { .. } => "user",
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
    media: &Media,
    text_only: bool,
) -> Vec<Value> {
    match input {
        Input::User { text, images } => {
            let prepared = crate::images::prepare(
                text,
                images,
                &[],
                media,
                text_only,
                crate::images::PdfForm::Native,
            );
            if prepared.images.is_empty() && prepared.documents.is_empty() {
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
            for document in &prepared.documents {
                blocks.push(json!({
                    "type": "document",
                    "source": {"type": "base64", "media_type": "application/pdf", "data": document.data},
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
        // The names the search loaded, as `tool_reference` blocks in
        // `loaded` order (`research/tool-search`, probe A1).
        Input::ToolSearchResult { action_id, loaded } => vec![json!({
            "type": "tool_result",
            "tool_use_id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
            "content": loaded
                .iter()
                .map(|name| json!({"type": "tool_reference", "tool_name": name}))
                .collect::<Vec<_>>(),
        })],
        Input::ToolResult {
            action_id,
            text,
            is_error,
            images,
            pdfs,
        } => {
            // A failed tool result sends Anthropic's `is_error` flag; a
            // success sends no such key
            // (platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls:
            // "`is_error` (optional): Set to `true` if the tool execution
            // resulted in an error.").
            let mut result = json!({
                "type": "tool_result",
                "tool_use_id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
                "content": crate::images::anthropic_content(crate::images::prepare(text, images, pdfs, media, text_only, crate::images::PdfForm::Native)),
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
