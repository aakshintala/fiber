//! The `openai-responses` protocol (`docs/model-routing.md`, "Protocols and
//! providers"): OpenAI, ChatGPT/codex, OpenCode and others. A request is a
//! `POST` to `<base_url>/responses`; the reply is a server-sent event stream
//! that ends with `response.completed`, `response.incomplete` or
//! `response.failed`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

use contract::diag::PrefixHashes;
use contract::events::{
    ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested,
};
use contract::provider::{
    CallUsage, Delta, Finish, HostedCall, Input, InputSize, ModelCall, ModelRequest, Provider,
    Reply, ReplyAction, TOOL_SEARCH, ToolDefinition,
};
use contract::shapes::Tokens;
use contract::{ActionId, ProviderCallId};
use serde_json::{Map, Value, json};

pub use crate::call::Call;
use crate::unfinished::named;
use crate::wire::str_at;
use crate::{Endpoint, Error, sse, strict};

/// One model reached over `openai-responses`.
#[derive(Debug, Clone)]
pub struct Responses {
    endpoint: Endpoint,
    cache_key_header: Option<String>,
    deferred_tools: bool,
}

impl Responses {
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
    /// every tool is declared in full. On, a deferred function is declared
    /// with `defer_loading`, `tool_search` is declared natively, and a
    /// search's result goes back as `tool_search_output`
    /// (`research/tool-search`).
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
        let headers = crate::call::base_headers(
            endpoint,
            "text/event-stream",
            &[],
            endpoint
                .key
                .as_ref()
                .map(|key| ("authorization", format!("Bearer {}", key.expose()))),
            self.cache_key_header.as_deref(),
            &request.cache_key,
        );
        let body = body(endpoint, request, self.deferred_tools);
        Call::new(
            endpoint,
            request,
            format!("{}/responses", endpoint.base_url.trim_end_matches('/')),
            headers,
            body,
            |stream, _, sink| decode_tracked(stream, sink),
        )
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

impl Provider for Responses {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        wire_tools(tools, self.deferred_tools)
    }

    fn defers(&self) -> bool {
        self.deferred_tools
    }
}

/// Each tool in Responses' shape, in name order: a hosted tool is the
/// vendor's own type alone, with no name or schema (`docs/tools.md`,
/// "Hosted by the provider"). This is the tools Fiber builds.
///
/// With `deferring`, a deferred function also carries `"defer_loading":
/// true`. `tool_search` and a hosted tool are never deferred: the first is
/// what loads the rest, the second has no definition to load. While any
/// function is deferred, `tool_search` is declared as OpenAI's client-executed
/// tool search, with no name and no `strict`; otherwise it is an ordinary
/// function (`research/tool-search`, probes B1 and B2).
fn wire_tools(tools: &[ToolDefinition], deferring: bool) -> Vec<Map<String, Value>> {
    let defers = |tool: &ToolDefinition| {
        deferring && tool.deferred && tool.name != TOOL_SEARCH && tool.hosted.is_none()
    };
    let native_search = tools.iter().any(defers);
    let sorted = crate::wire::sorted_by_name(tools);
    sorted
        .into_iter()
        .map(|tool| {
            if let Some(kind) = &tool.hosted {
                return crate::wire::object(json!({"type": kind}));
            }
            if native_search && tool.name == TOOL_SEARCH {
                return crate::wire::object(json!({
                    "type": "tool_search",
                    "execution": "client",
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }));
            }
            let mut wired = crate::wire::object(json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "strict": strict::fits(&tool.input_schema),
            }));
            if defers(tool) {
                wired.insert("defer_loading".into(), json!(true));
            }
            wired
        })
        .collect()
}

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes").
fn body(endpoint: &Endpoint, request: &ModelRequest, deferring: bool) -> Vec<u8> {
    // A rewound session's first request sends its parent's logged build
    // verbatim, so it matches the parent's bytes (`docs/events.md`,
    // "Rewind").
    let tools = wired_tools(request, deferring);
    let mut body = Map::new();
    body.insert("model".into(), json!(endpoint.model));
    body.insert("instructions".into(), json!(request.system_prompt));
    body.insert(
        "input".into(),
        Value::Array(input(endpoint, request, deferring)),
    );
    body.insert("tools".into(), Value::Array(tools));
    body.insert("tool_choice".into(), json!(request.tool_choice));
    body.insert("prompt_cache_key".into(), json!(request.cache_key));
    // OpenAI offers one cache lifetime, so `cache_lifetime` is not sent, and
    // its cache has no markers (`docs/prompt-cache.md`, "Cache lifetime").
    body.insert("stream".into(), json!(true));
    // Fiber keeps no state at the vendor, so it asks for the encrypted
    // reasoning to send back (`docs/loop.md`, "What the model is sent").
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    if let Some(level) = &request.thinking {
        let effort = if *level == contract::ThinkingLevel::Off {
            "none"
        } else {
            level.as_str()
        };
        body.insert("reasoning".into(), json!({ "effort": effort }));
    }
    if let Some(store) = endpoint.compat.store {
        body.insert("store".into(), json!(store));
    }
    body.extend(endpoint.extra_body.clone());
    if let Some(limit) = request
        .max_output_tokens
        .and_then(|request_limit| endpoint.output_limit(Some(request_limit)))
    {
        // The smaller of the two, as on the other protocols: a model's
        // `extra_body` limit may be the lower (`docs/errors.md`, "Output
        // tokens"). The Responses API rejects values below 16.
        let limit = body
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .map_or(limit, |n| n.min(limit));
        body.insert("max_output_tokens".into(), json!(limit.max(16)));
    }
    Value::Object(body).to_string().into_bytes()
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

/// The redacted prefix digests of the encoded segments: the instructions,
/// the tools and each cumulative input prefix as sent. Responses always
/// sends instructions and a tools list, so both digests are always
/// present.
fn hashes(endpoint: &Endpoint, request: &ModelRequest, deferring: bool) -> PrefixHashes {
    PrefixHashes {
        system: Some(crate::cache_diagnostics::segment(&json!(
            request.system_prompt
        ))),
        tools: Some(crate::cache_diagnostics::segment(&Value::Array(
            wired_tools(request, deferring),
        ))),
        messages: crate::cache_diagnostics::cumulative(&input(endpoint, request, deferring)),
    }
}

/// The conversation as Responses input items.
///
/// A `tool_search` call whose result in the request is a `ToolSearchResult`
/// goes back as a `tool_search_call` and a `tool_search_output`; every other
/// call to it, whether answered by zero matches, a failure or text, stays a
/// `function_call` and its output (`research/tool-search`, probes B1, B2).
fn input(endpoint: &Endpoint, request: &ModelRequest, deferring: bool) -> Vec<Value> {
    let reference = endpoint.reference();
    let searched: BTreeSet<&ActionId> = request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolSearchResult { action_id, .. } => Some(action_id),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. } => None,
        })
        .collect();
    // Each output tool is the object the request's own `tools` holds, so the
    // two can never disagree.
    let tools = if searched.is_empty() {
        Vec::new()
    } else {
        wired_tools(request, deferring)
    };
    // A call's `call_id` is the provider's id for it, or its action id when
    // the reply carried none; its result names the same one.
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
    request
        .conversation
        .iter()
        .enumerate()
        .filter_map(|(index, input)| match input {
            Input::User { text, images } => {
                let prepared = crate::images::prepare(
                    text,
                    images,
                    &[],
                    &request.session_dir,
                    endpoint.text_only,
                    crate::images::PdfForm::Native,
                );
                Some(json!({ "role": "user", "content": output(prepared) }))
            }
            Input::Assistant {
                model,
                text,
                provider_item,
            } => {
                if *model == reference
                    && let Some(item) = provider_item
                {
                    // A hosted pair logs the same item twice, as two
                    // adjacent inputs, but the request carries it once
                    // (`docs/tools.md`, "Hosted by the provider").
                    if item.get("type").and_then(Value::as_str) == Some("web_search_call")
                        && let Some(Input::Assistant {
                            model: before_model,
                            provider_item: Some(before),
                            ..
                        }) = index
                            .checked_sub(1)
                            .and_then(|at| request.conversation.get(at))
                        && *before_model == reference
                        && *before == *item
                    {
                        return None;
                    }
                    Some(item.clone())
                } else if text.is_empty() {
                    None
                } else {
                    Some(json!({ "role": "assistant", "content": text }))
                }
            }
            // Reasoning goes back unchanged, only to the model that produced
            // it, and never as plain text.
            Input::Reasoning {
                model,
                provider_item,
                ..
            } if *model == reference => provider_item.clone(),
            Input::Reasoning { .. } => None,
            Input::ToolCall {
                action_id, call, ..
            } => {
                let call_id = call_ids
                    .get(action_id)
                    .copied()
                    .unwrap_or(action_id.0.as_str());
                if call.name == TOOL_SEARCH && searched.contains(action_id) {
                    Some(json!({
                        "type": "tool_search_call",
                        "execution": "client",
                        "call_id": call_id,
                        "status": "completed",
                        "arguments": call.arguments,
                    }))
                } else {
                    Some(json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": call.name,
                        "arguments": arguments_text(&call.arguments),
                    }))
                }
            }
            // A loaded name always names a deferred function the request
            // sends (`ModelRequest::conversation`), so none is skipped.
            Input::ToolSearchResult { action_id, loaded } => Some(json!({
                "type": "tool_search_output",
                "execution": "client",
                "call_id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
                "status": "completed",
                "tools": loaded
                    .iter()
                    .filter_map(|name| tools.iter().find(|tool| tool["name"] == *name))
                    .collect::<Vec<_>>(),
            })),
            // The flag is ignored: OpenAI's Responses API defines no error
            // field on a function-call output (openai-openapi
            // `FunctionCallOutputItemParam` / `FunctionToolCallOutput`:
            // `type, call_id, output, id, name, namespace, caller, status`,
            // where `status` is item progress, not tool failure), so a
            // failed result sends the same bytes as a success.
            Input::ToolResult {
                action_id,
                text,
                images,
                pdfs,
                ..
            } => {
                let prepared = crate::images::prepare(
                    text,
                    images,
                    pdfs,
                    &request.session_dir,
                    endpoint.text_only,
                    crate::images::PdfForm::Native,
                );
                Some(json!({
                    "type": "function_call_output",
                    "call_id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
                    "output": output(prepared),
                }))
            }
        })
        .collect()
}

/// A `function_call_output`'s `output`: the plain text when no image or PDF
/// is sent, otherwise an array holding one `input_text` part, when the text
/// is non-empty, then one `input_image` part per image, each carrying a data
/// URL, then one `input_file` part per PDF carrying its bytes as a data URL
/// (`docs/tools.md`, "read").
fn output(prepared: crate::images::Prepared) -> Value {
    if prepared.images.is_empty() && prepared.documents.is_empty() {
        return json!(prepared.text);
    }
    let mut parts = Vec::new();
    if !prepared.text.is_empty() {
        parts.push(json!({"type": "input_text", "text": prepared.text}));
    }
    for image in &prepared.images {
        parts.push(json!({"type": "input_image", "image_url": crate::images::data_url(image)}));
    }
    for document in &prepared.documents {
        parts.push(json!({
            "type": "input_file",
            "filename": document.filename,
            "file_data": format!("data:application/pdf;base64,{}", document.data),
        }));
    }
    Value::Array(parts)
}

/// A call's arguments from the text Responses carries: the object it
/// parses to, `{}` when empty, otherwise the raw text.
fn parsed_arguments(raw: &str) -> Value {
    match serde_json::from_str(raw) {
        Ok(Value::Object(map)) => Value::Object(map),
        _ if raw.is_empty() => Value::Object(Map::new()),
        Ok(_) | Err(_) => Value::String(raw.to_owned()),
    }
}

/// Arguments as the text Responses carries: a raw string as it was, an
/// object serialised.
fn arguments_text(arguments: &Value) -> String {
    if let Value::String(raw) = arguments {
        raw.clone()
    } else {
        arguments.to_string()
    }
}

/// Reads a reply stream, passing each fragment to `sink` as it arrives, and
/// returns the reply once its terminal event arrives. A stream that fails
/// keeps nothing it streamed, finished tool calls included.
#[cfg(any(test, feature = "test-hooks"))]
pub fn decode(stream: impl BufRead, sink: &mut dyn FnMut(Delta)) -> Result<Reply, Error> {
    decode_tracked(stream, sink).map_err(|(error, _)| error)
}

/// As `decode`, also carrying what the stream had seen when it failed:
/// the generation and its usage once a response event named them, else none.
#[allow(
    clippy::result_large_err,
    reason = "the decode carries its partial alongside the error for the call's usage"
)]
pub(crate) fn decode_tracked(
    stream: impl BufRead,
    sink: &mut dyn FnMut(Delta),
) -> Result<Reply, (Error, Option<CallUsage>)> {
    let mut reply = Decoder::default();
    let mut end = None;
    let read = sse::read(stream, |data| {
        let event: Value = serde_json::from_str(data)
            .map_err(|e| Error::StreamIncomplete(format!("an event is not JSON ({e})")))?;
        end = reply.event(&event, sink)?;
        Ok(end.is_some())
    });
    if let Err(error) = read {
        return Err((error, Some(reply.partial())));
    }
    match end {
        Some(reply) => Ok(reply),
        None => Err((
            Error::StreamIncomplete("it ended before its terminal event".into()),
            Some(reply.partial()),
        )),
    }
}

/// What a reply has produced so far.
#[derive(Default)]
struct Decoder {
    /// What the model said when it refused on policy grounds.
    refusal: Option<String>,
    actions: Vec<ReplyAction>,
    /// Each tool call's index within the message and its name, by item id.
    calls: BTreeMap<String, (u32, Option<String>)>,
    /// The response's id, from `response.created` on.
    id: String,
    /// The response's last usage, from `response.created` on.
    usage: Value,
}

impl Decoder {
    /// What the stream had seen: the generation once a response event named
    /// it, and the usage so far, with a reported cache write counted under
    /// `5m`.
    fn partial(&self) -> CallUsage {
        CallUsage {
            generation_id: named(self.id.clone()),
            tokens: tokens(&self.usage),
            web_searches: None,
            input_size: InputSize::default(),
        }
    }

    /// Records the response's usage, and its id from the first event that
    /// names one: a later event's id never replaces it.
    fn note(&mut self, response: &Value) {
        if self.id.is_empty()
            && let Some(id) = response.get("id").and_then(Value::as_str)
        {
            id.clone_into(&mut self.id);
        }
        if response.get("usage").is_some() {
            self.usage = response["usage"].clone();
        }
    }

    /// Takes one event; returns the reply when it was the terminal one.
    fn event(
        &mut self,
        event: &Value,
        sink: &mut dyn FnMut(Delta),
    ) -> Result<Option<Reply>, Error> {
        let text = |key: &str| str_at(event, key).to_owned();
        match str_at(event, "type") {
            "response.output_item.added" => {
                let item = &event["item"];
                if matches!(str_at(item, "type"), "function_call" | "tool_search_call") {
                    self.call(str_at(item, "id"), item.get("name").and_then(Value::as_str));
                }
            }
            "response.output_text.delta" => {
                sink(Delta::Text(TextDelta {
                    text: text("delta"),
                }));
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                sink(Delta::Reasoning(TextDelta {
                    text: text("delta"),
                }));
            }
            "response.function_call_arguments.delta" => {
                let (index, name) = self.call(str_at(event, "item_id"), None);
                sink(Delta::ToolCallArguments(ToolCallArgumentsDelta {
                    index,
                    name,
                    text: text("delta"),
                }));
            }
            "response.output_item.done" => self.done(&event["item"]),
            "response.created" | "response.in_progress" => {
                self.note(&event["response"]);
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                self.note(&event["response"]);
                return self.finish(&event["response"]).map(Some);
            }
            "error" => {
                // A usage-limit error is `quota_exceeded`, never retried
                // (`docs/model-routing.md`, "Protocols and providers").
                if crate::error::usage_limit(event) {
                    return Err(Error::QuotaExceeded(text("message")));
                }
                return Err(Error::ReplyFailed {
                    code: event.get("code").and_then(Value::as_str).map(str::to_owned),
                    message: text("message"),
                });
            }
            _ => {}
        }
        Ok(None)
    }

    /// A tool call's index and name, numbering it on first sight.
    fn call(&mut self, id: &str, name: Option<&str>) -> (u32, Option<String>) {
        let next = u32::try_from(self.calls.len()).unwrap_or(u32::MAX);
        self.calls
            .entry(id.to_owned())
            .or_insert_with(|| (next, name.map(str::to_owned)))
            .clone()
    }

    /// An output item the model finished.
    fn done(&mut self, item: &Value) {
        let texts = |key: &str, kinds: &[&str]| -> Vec<String> {
            item.get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|part| kinds.contains(&str_at(part, "type")))
                .map(|part| {
                    let field = if str_at(part, "type") == "refusal" {
                        "refusal"
                    } else {
                        "text"
                    };
                    str_at(part, field).to_owned()
                })
                .collect()
        };
        match str_at(item, "type") {
            "message" => {
                let text = texts("content", &["output_text"]).concat();
                let refusal = texts("content", &["refusal"]);
                if !refusal.is_empty() {
                    self.refusal = Some(refusal.concat());
                } else {
                    self.actions.push(ReplyAction::Text(TextCompleted {
                        text,
                        provider_item: Some(item.clone()),
                    }));
                }
            }
            "reasoning" => {
                let mut parts = texts("summary", &["summary_text"]);
                parts.extend(texts("content", &["reasoning_text"]));
                self.actions
                    .push(ReplyAction::Reasoning(ReasoningCompleted {
                        text: parts.join("\n\n"),
                        provider_item: Some(item.clone()),
                    }));
            }
            "web_search_call" => {
                self.actions
                    .push(ReplyAction::Hosted(web_search_call(item)));
            }
            "function_call" => {
                self.actions.push(ReplyAction::ToolCall(ToolCallRequested {
                    name: str_at(item, "name").to_owned(),
                    arguments: parsed_arguments(str_at(item, "arguments")),
                    provider_id: item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .map(|id| ProviderCallId(id.to_owned())),
                    repair: None,
                    ran_by: None,
                    provider_item: None,
                }));
            }
            // The model's call to Fiber's own `tool_search`, declared as
            // OpenAI's client-executed tool (`research/tool-search`).
            "tool_search_call" => {
                // An object is kept; a string reads as a `function_call`'s does.
                let arguments = match item.get("arguments") {
                    Some(Value::Object(map)) => Value::Object(map.clone()),
                    Some(Value::String(raw)) => parsed_arguments(raw),
                    _ => Value::Object(Map::new()),
                };
                self.actions.push(ReplyAction::ToolCall(ToolCallRequested {
                    name: TOOL_SEARCH.to_owned(),
                    arguments,
                    provider_id: item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .map(|id| ProviderCallId(id.to_owned())),
                    repair: None,
                    ran_by: None,
                    provider_item: None,
                }));
            }
            // Items Fiber does not act on.
            _ => {}
        }
    }

    /// The reply, from the terminal event's response object.
    fn finish(&mut self, response: &Value) -> Result<Reply, Error> {
        let finish = match str_at(response, "status") {
            "completed" => Finish::Completed,
            "incomplete" => match response.pointer("/incomplete_details/reason") {
                Some(Value::String(reason)) if reason == "max_output_tokens" => Finish::OutputLimit,
                Some(Value::String(reason)) if reason == "content_filter" => {
                    return Err(Error::Refused(
                        "its content filter stopped the reply".into(),
                    ));
                }
                other => {
                    return Err(Error::UnknownStopReason(format!(
                        "incomplete: {}",
                        other.unwrap_or(&Value::Null)
                    )));
                }
            },
            "failed" => {
                let error = &response["error"];
                // A usage-limit failure is `quota_exceeded`, never retried
                // (`docs/model-routing.md`, "Protocols and providers").
                // The stream carries no `Date`, so there is no wait.
                if crate::error::usage_limit(error) {
                    return Err(Error::QuotaExceeded(
                        response
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    ));
                }
                return Err(Error::ReplyFailed {
                    code: response
                        .pointer("/error/code")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    message: response
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                });
            }
            status @ ("in_progress" | "queued") => {
                return Err(Error::StreamIncomplete(format!(
                    "its terminal event's status is `{status}`"
                )));
            }
            // The provider cancelled the response itself, not Fiber: a
            // failure inside a 200 stream that no other code matches.
            "cancelled" => {
                return Err(Error::StreamIncomplete(
                    "the provider cancelled the response".into(),
                ));
            }
            other => return Err(Error::UnknownStopReason(other.to_owned())),
        };
        if let Some(refusal) = self.refusal.take() {
            return Err(Error::Refused(format!("the model refused: {refusal}")));
        }
        Ok(Reply {
            actions: std::mem::take(&mut self.actions),
            finish,
            generation_id: named(std::mem::take(&mut self.id)),
            tokens: tokens(&response["usage"]),
            web_searches: None,
            cost: None,
            input_size: InputSize::default(),
        })
    }
}

/// A hosted search's call and result from its done item: the call names
/// `web_search`, with the item's `action` as its arguments less any
/// `sources`; the completion carries the result URLs, one per line: the
/// first non-empty of the item's `results` URLs, the action's `sources`
/// URLs, and the action's own `url`, such as an `open_page` action's
/// (`docs/tools.md`, "Hosted by the provider"). Only string URLs count.
fn web_search_call(item: &Value) -> HostedCall {
    let arguments = match item.get("action") {
        Some(Value::Object(action)) => {
            let mut action = action.clone();
            action.remove("sources");
            Value::Object(action)
        }
        _ => Value::Object(Map::new()),
    };
    let results: Vec<&str> = item
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|result| result.get("url").and_then(Value::as_str))
        .collect();
    let sources: Vec<&str> = item
        .pointer("/action/sources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|source| source.get("url").and_then(Value::as_str))
        .collect();
    let urls = if !results.is_empty() {
        results
    } else if !sources.is_empty() {
        sources
    } else {
        item.pointer("/action/url")
            .and_then(Value::as_str)
            .into_iter()
            .collect()
    };
    let completed = match item.get("status").and_then(Value::as_str) {
        Some("completed") => crate::hosted::completed(item.clone(), &urls),
        status => crate::hosted::failed(item.clone(), status.unwrap_or("unknown")),
    };
    HostedCall {
        call: ToolCallRequested {
            name: "web_search".to_owned(),
            arguments,
            provider_id: item
                .get("id")
                .and_then(Value::as_str)
                .map(|id| ProviderCallId(id.to_owned())),
            repair: None,
            ran_by: None,
            provider_item: Some(item.clone()),
        },
        completed,
    }
}

/// `usage` as `tokens`: Responses counts cache reads inside
/// `input_tokens`, and `tokens.input` excludes both reads and writes. A
/// cache write is always counted under `5m`: OpenAI keeps one 30-minute
/// lifetime, priced at the 5-minute rate.
fn tokens(usage: &Value) -> Tokens {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let cached = count("/input_tokens_details/cached_tokens");
    let write = count("/input_tokens_details/cache_write_tokens");
    Tokens {
        input: count("/input_tokens")
            .saturating_sub(cached)
            .saturating_sub(write),
        cache_read: cached,
        cache_write: if write > 0 {
            BTreeMap::from([("5m".to_owned(), write)])
        } else {
            BTreeMap::new()
        },
        output: count("/output_tokens"),
    }
}

#[cfg(test)]
#[path = "openai_responses_tests.rs"]
mod tests;
