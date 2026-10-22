//! The `openai-completions` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): OpenCode, OpenRouter, Databricks, muse and Azure. A
//! request is a `POST` to `<base_url>/chat/completions`; the reply is a
//! server-sent event stream of chunks that ends with `data: [DONE]`. The
//! finish reason can arrive twice, and the usage chunk after it
//! (`docs/model-routing.md`, "openai-completions facts").

use std::collections::BTreeMap;
use std::io::BufRead;

use contract::ProviderCallId;
use contract::diag::PrefixHashes;
use contract::events::{
    CacheLifetime, ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta,
    ToolCallRequested,
};
use contract::provider::{
    CallUsage, Delta, Finish, HostedCall, InputSize, ModelCall, ModelRequest, Provider, Reply,
    ReplyAction, ToolDefinition,
};
use contract::shapes::Tokens;
use serde_json::{Map, Value, json};

pub use crate::call::Call;
use crate::openai_completions_messages::messages;
use crate::unfinished::named;
use crate::wire::{args_value, str_at};
use crate::{Endpoint, Error, sse};

/// One model reached over `openai-completions`.
#[derive(Debug, Clone)]
pub struct Completions {
    endpoint: Endpoint,
    cache_key_header: Option<String>,
}

impl Completions {
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
        let body = body(endpoint, request);
        Call::new(
            endpoint,
            request,
            format!(
                "{}/chat/completions",
                endpoint.base_url.trim_end_matches('/')
            ),
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
            call = call.with_hashes(hashes(&self.endpoint, request));
        }
        Box::new(call.observed(observation.with_model(self.endpoint.model.clone())))
    }
}

impl Provider for Completions {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        crate::openai_completions_tools::wire_tools(&self.endpoint, tools)
    }
}

/// The tools as sent: the rewound build verbatim, else the tools Fiber
/// builds. Shared by the body and the digests, so both hash the same
/// bytes.
fn wired_tools(endpoint: &Endpoint, request: &ModelRequest) -> Vec<Value> {
    request
        .sent_tools
        .clone()
        .unwrap_or_else(|| crate::openai_completions_tools::wire_tools(endpoint, &request.tools))
        .into_iter()
        .map(Value::Object)
        .collect()
}

/// The redacted prefix digests of the encoded segments: the system
/// message, the tools and each cumulative messages prefix as sent. The
/// system message is the first message the encoder built, exactly as
/// sent, including any provider marker; absent when the request sends
/// none. Tools ride the body only beside a tools list, so their digest
/// is absent without one.
fn hashes(endpoint: &Endpoint, request: &ModelRequest) -> PrefixHashes {
    let sent = messages(endpoint, request);
    // The system message the encoder built first, exactly as sent:
    // `messages` pushes it before the conversation when the prompt is
    // non-empty, and nothing else shapes it.
    let system = if request.system_prompt.is_empty() {
        None
    } else {
        sent.first().map(crate::cache_diagnostics::segment)
    };
    let tools = wired_tools(endpoint, request);
    PrefixHashes {
        system,
        tools: (!tools.is_empty()).then(|| crate::cache_diagnostics::segment(&Value::Array(tools))),
        messages: crate::cache_diagnostics::cumulative(&sent),
    }
}

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes").
fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
    // A rewound session's first request sends its parent's logged build
    // verbatim, so it matches the parent's bytes (`docs/events.md`,
    // "Rewind").
    let tools = wired_tools(endpoint, request);
    let compat = &endpoint.compat;
    let mut body = Map::new();
    body.insert("model".into(), json!(endpoint.model));
    body.insert("messages".into(), Value::Array(messages(endpoint, request)));
    // OpenAI and OpenRouter (which falls back to it for sticky routing)
    // route the cache by this key (`docs/prompt-cache.md`, "Cache markers
    // and keys"; `research/prompt-cache/README.md`).
    body.insert("prompt_cache_key".into(), json!(request.cache_key));
    // OpenRouter takes `session_id` as a top-level body field
    // (openrouter.ai/docs/guides/best-practices/prompt-caching).
    if let Some(field) = &compat.cache_key_field {
        body.insert(field.clone(), json!(request.cache_key));
    }
    // OpenAI sends no usage without it (`docs/model-routing.md`,
    // "openai-completions facts").
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    // OpenAI rejects `tool_choice` without `tools`.
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
        body.insert("tool_choice".into(), tool_choice(&request.tool_choice));
    }
    if let Some(level) = &request.thinking {
        let effort = if *level == contract::ThinkingLevel::Off {
            "none"
        } else {
            level.as_str()
        };
        if compat.reasoning_object {
            body.insert("reasoning".into(), json!({ "effort": effort }));
        } else {
            body.insert("reasoning_effort".into(), json!(effort));
        }
    }
    if let Some(store) = compat.store {
        body.insert("store".into(), json!(store));
    }
    body.extend(endpoint.extra_body.clone());
    // The model's limit, or the model data's own when that is lower
    // (`docs/errors.md`, "Output tokens").
    let field = if compat.max_tokens {
        "max_tokens"
    } else {
        "max_completion_tokens"
    };
    if let Some(limit) = endpoint.output_limit(request.max_output_tokens) {
        let max = body
            .get(field)
            .and_then(Value::as_u64)
            .map_or(limit, |n| n.min(limit));
        body.insert(field.into(), json!(max));
    }
    Value::Object(body).to_string().into_bytes()
}

/// `tool_choice` on the wire: `auto`, `none` and `required` are OpenAI's own
/// values; any other string names the one tool to force.
fn tool_choice(choice: &str) -> Value {
    match choice {
        "auto" | "none" | "required" => json!(choice),
        name => json!({"type": "function", "function": {"name": name}}),
    }
}

/// Reads a reply stream, passing each fragment to `sink` as it arrives, and
/// returns the reply once `[DONE]` arrives. A cache write is counted under
/// `5m`, whatever the request's cache lifetime. A stream that fails keeps
/// nothing it streamed, finished tool calls included.
pub fn decode(
    stream: impl BufRead,
    _lifetime: &CacheLifetime,
    sink: &mut dyn FnMut(Delta),
) -> Result<Reply, Error> {
    // The lifetime stays on the public signature for the recorded-decode call
    // sites; the write is always keyed `5m`, so it is ignored.
    decode_tracked(stream, sink).map_err(|(error, _)| error)
}

/// As [`decode`], also carrying what the stream had seen when it failed:
/// the generation and its usage once a chunk named them, else none.
#[allow(
    clippy::result_large_err,
    reason = "the decode carries its partial alongside the error for the call's usage"
)]
pub(crate) fn decode_tracked(
    stream: impl BufRead,
    sink: &mut dyn FnMut(Delta),
) -> Result<Reply, (Error, Option<CallUsage>)> {
    let mut reply = Decoder::default();
    let mut done = false;
    let read = sse::read(stream, |data| {
        if data == "[DONE]" {
            done = true;
            return Ok(true);
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|e| Error::StreamIncomplete(format!("a chunk is not JSON ({e})")))?;
        reply.chunk(&chunk, sink)?;
        Ok(false)
    });
    if let Err(error) = read {
        return Err((error, Some(reply.partial())));
    }
    if !done {
        return Err((
            Error::StreamIncomplete("it ended before [DONE]".into()),
            Some(reply.partial()),
        ));
    }
    let partial = reply.partial();
    reply.finish().map_err(|error| (error, Some(partial)))
}

/// One tool call as it streams in.
#[derive(Default)]
struct StreamedCall {
    /// Its position among the reply's tool calls.
    position: u32,
    id: String,
    name: String,
    arguments: String,
}

/// What a reply has produced so far.
#[derive(Default)]
struct Decoder {
    id: String,
    text: String,
    /// What the model said when it refused on policy grounds.
    refusal: String,
    reasoning: String,
    /// The field the reasoning text arrived in, such as `reasoning`.
    reasoning_field: Option<String>,
    /// `reasoning_details` entries, merged by their `index`.
    details: Vec<Value>,
    /// Tool calls by their wire `index`.
    calls: BTreeMap<u64, StreamedCall>,
    /// A hosted search's result entries, as `delta.annotations` carried
    /// them, appended in arrival order across chunks.
    annotations: Vec<Value>,
    finish: Option<String>,
    usage: Value,
}

impl Decoder {
    /// What the stream had seen: the generation once a chunk named it, and
    /// the usage so far.
    fn partial(&self) -> CallUsage {
        CallUsage {
            generation_id: named(self.id.clone()),
            tokens: tokens(&self.usage),
            web_searches: None,
            input_size: InputSize::default(),
        }
    }

    /// Takes one chunk.
    fn chunk(&mut self, chunk: &Value, sink: &mut dyn FnMut(Delta)) -> Result<(), Error> {
        // An error inside the 200 stream, as OpenRouter sends an upstream's
        // failure (`research/provider-errors`, "How an error arrives after
        // HTTP 200").
        if let Some(error) = chunk.get("error").filter(|e| e.is_object()) {
            // OpenRouter's `code` repeats the HTTP status as a number; the
            // upstream's own code is in `metadata`.
            let code = ["/metadata/provider_code", "/code"]
                .iter()
                .find_map(|p| error.pointer(p).and_then(Value::as_str))
                .map(str::to_owned);
            return Err(crate::error::stream_failure(error, code));
        }
        if self.id.is_empty() {
            str_at(chunk, "id").clone_into(&mut self.id);
        }
        if chunk.get("usage").is_some_and(Value::is_object) {
            self.usage = chunk["usage"].clone();
        }
        // The one candidate Fiber asks for: index 0.
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|c| c.get("index").and_then(Value::as_u64).unwrap_or(0) == 0)
        else {
            return Ok(());
        };
        let delta = &choice["delta"];
        let content = str_at(delta, "content");
        if !content.is_empty() {
            self.text.push_str(content);
            sink(Delta::Text(TextDelta {
                text: content.to_owned(),
            }));
        }
        self.refusal.push_str(str_at(delta, "refusal"));
        self.reasoning(delta, sink);
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.call(call, sink);
        }
        // A hosted search's results arrive as `delta.annotations` entries;
        // each chunk's entries append in order, never replace.
        if let Some(entries) = delta.get("annotations").and_then(Value::as_array) {
            self.annotations.extend(entries.iter().cloned());
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(reason.to_owned());
        }
        Ok(())
    }

    /// The reasoning in one delta: its text from the first field that
    /// carries any, and its `reasoning_details` entries kept for replay.
    fn reasoning(&mut self, delta: &Value, sink: &mut dyn FnMut(Delta)) {
        let field = ["reasoning_content", "reasoning", "reasoning_text"]
            .into_iter()
            .find(|f| !str_at(delta, f).is_empty());
        let mut piece = String::new();
        if let Some(field) = field {
            piece.push_str(str_at(delta, field));
            self.reasoning_field = Some(field.to_owned());
        }
        for entry in delta
            .get("reasoning_details")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if field.is_none() {
                piece.push_str(str_at(entry, "text"));
                piece.push_str(str_at(entry, "summary"));
            }
            self.detail(entry);
        }
        if !piece.is_empty() {
            self.reasoning.push_str(&piece);
            sink(Delta::Reasoning(TextDelta { text: piece }));
        }
    }

    /// Merges one streamed `reasoning_details` entry into the latest one of
    /// the same `index` and `type`: its text pieces appended, its other
    /// fields set.
    fn detail(&mut self, entry: &Value) {
        let Value::Object(entry) = entry else {
            return;
        };
        // An entry continues an earlier one only by a shared `index`, and
        // never across distinct ids; one without an index is kept as it
        // came (openrouter.ai/docs/guides/best-practices/reasoning-tokens,
        // "Preserving reasoning").
        let index = entry.get("index").filter(|i| !i.is_null());
        let id = entry.get("id").filter(|i| !i.is_null());
        let same = |kept: &Value| {
            index.is_some()
                && kept.get("index") == index
                && kept.get("type") == entry.get("type")
                && (id.is_none() || kept.get("id").is_none_or(|k| Some(k) == id))
        };
        let Some(Value::Object(kept)) = self.details.iter_mut().rev().find(|k| same(k)) else {
            self.details.push(Value::Object(entry.clone()));
            return;
        };
        for (key, value) in entry {
            match (kept.get_mut(key), value) {
                (Some(Value::String(old)), Value::String(new))
                    if matches!(key.as_str(), "text" | "summary" | "data") =>
                {
                    old.push_str(new);
                }
                (_, Value::Null) => {}
                _ => {
                    kept.insert(key.clone(), value.clone());
                }
            }
        }
    }

    /// One tool-call delta, keyed by its `index`. A delta without one
    /// starts a call when it carries a new id and continues the last call
    /// otherwise.
    fn call(&mut self, call: &Value, sink: &mut dyn FnMut(Delta)) {
        let id = str_at(call, "id");
        let key = call
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                let last = self.calls.keys().next_back().copied();
                match last {
                    Some(last) if id.is_empty() || self.calls.values().any(|c| c.id == id) => last,
                    Some(last) => last + 1,
                    None => 0,
                }
            });
        let position = u32::try_from(self.calls.len()).unwrap_or(u32::MAX);
        let slot = self.calls.entry(key).or_insert_with(|| StreamedCall {
            position,
            ..StreamedCall::default()
        });
        if slot.id.is_empty() {
            id.clone_into(&mut slot.id);
        }
        let function = &call["function"];
        if slot.name.is_empty() {
            str_at(function, "name").clone_into(&mut slot.name);
        }
        let piece = str_at(function, "arguments");
        if !piece.is_empty() {
            slot.arguments.push_str(piece);
            sink(Delta::ToolCallArguments(ToolCallArgumentsDelta {
                index: slot.position,
                name: Some(slot.name.clone()).filter(|n| !n.is_empty()),
                text: piece.to_owned(),
            }));
        }
    }

    /// The reply, once `[DONE]` has arrived.
    fn finish(self) -> Result<Reply, Error> {
        let finish = match self.finish.as_deref() {
            Some("stop" | "tool_calls" | "function_call") => Finish::Completed,
            Some("length") => Finish::OutputLimit,
            Some("content_filter") => {
                return Err(Error::Refused(
                    "its content filter stopped the reply".into(),
                ));
            }
            // OpenRouter's normalised `error`, here without the error body
            // that usually comes with it.
            Some("error") => {
                return Err(Error::ReplyFailed {
                    code: None,
                    message: "the reply finished with `error`".into(),
                });
            }
            Some(other) => return Err(Error::UnknownStopReason(other.to_owned())),
            None => {
                return Err(Error::StreamIncomplete(
                    "[DONE] arrived before any finish_reason".into(),
                ));
            }
        };
        if !self.refusal.is_empty() {
            return Err(Error::Refused(format!(
                "the model refused: {}",
                self.refusal
            )));
        }
        let mut actions = Vec::new();
        if !self.reasoning.is_empty() || !self.details.is_empty() {
            let provider_item = if self.details.is_empty() {
                self.reasoning_field
                    .map(|field| json!({ field: self.reasoning }))
            } else {
                Some(json!({"reasoning_details": self.details}))
            };
            actions.push(ReplyAction::Reasoning(ReasoningCompleted {
                text: self.reasoning,
                provider_item,
            }));
        }
        // A reply carrying search results holds one hosted call before its
        // text: the search the provider ran for it.
        if let Some(hosted) = hosted_search(&self.annotations) {
            actions.push(ReplyAction::Hosted(hosted));
        }
        if !self.text.is_empty() {
            actions.push(ReplyAction::Text(TextCompleted {
                text: self.text,
                provider_item: None,
            }));
        }
        let mut calls: Vec<StreamedCall> = self.calls.into_values().collect();
        calls.sort_by_key(|c| c.position);
        actions.extend(calls.into_iter().map(|call| {
            let arguments = args_value(call.arguments);
            ReplyAction::ToolCall(ToolCallRequested {
                name: call.name,
                arguments,
                provider_id: Some(ProviderCallId(call.id)).filter(|id| !id.0.is_empty()),
                repair: None,
                ran_by: None,
                provider_item: None,
            })
        }));
        Ok(Reply {
            actions,
            finish,
            generation_id: named(self.id),
            tokens: tokens(&self.usage),
            web_searches: web_searches(&self.usage),
            cost: cost(&self.usage),
            input_size: InputSize::default(),
        })
    }
}

/// One hosted search from the reply's annotation entries, or `None` when no
/// `url_citation` URL arrived. The call names
/// `web_search` with empty arguments, since the stream never shows the
/// query; the completion carries the cited URLs, one per line, in the order
/// each first arrived, duplicates dropped, with the raw entries as the
/// `provider_item`. Only string URLs count.
fn hosted_search(annotations: &[Value]) -> Option<HostedCall> {
    let mut urls: Vec<&str> = Vec::new();
    for entry in annotations {
        if entry.get("type").and_then(Value::as_str) != Some("url_citation") {
            continue;
        }
        if let Some(url) = entry.pointer("/url_citation/url").and_then(Value::as_str)
            && !url.is_empty()
            && !urls.contains(&url)
        {
            urls.push(url);
        }
    }
    let item = Value::Array(annotations.to_vec());
    if urls.is_empty() {
        return None;
    }
    Some(HostedCall {
        call: ToolCallRequested {
            name: "web_search".to_owned(),
            arguments: json!({}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: Some(item.clone()),
        },
        completed: crate::hosted::completed(item, &urls),
    })
}

/// Hosted searches the usage reports; `None` when it reports none.
fn web_searches(usage: &Value) -> Option<u64> {
    usage
        .pointer("/server_tool_use_details/web_search_requests")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
}

/// `usage` as `tokens`. OpenAI and OpenRouter count cache reads
/// (`cached_tokens`) and OpenRouter's cache writes (`cache_write_tokens`)
/// inside `prompt_tokens`, and `tokens.input` excludes both
/// (`docs/model-routing.md`, "openai-completions facts"). The vendor does
/// not say which lifetime a write was, so every write is counted under
/// `5m`: OpenAI keeps one 30-minute lifetime, priced at the 5-minute rate.
fn tokens(usage: &Value) -> Tokens {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let read = count("/prompt_tokens_details/cached_tokens");
    let write = count("/prompt_tokens_details/cache_write_tokens");
    Tokens {
        input: count("/prompt_tokens")
            .saturating_sub(read)
            .saturating_sub(write),
        cache_read: read,
        cache_write: if write > 0 {
            BTreeMap::from([("5m".to_owned(), write)])
        } else {
            BTreeMap::new()
        },
        output: count("/completion_tokens"),
    }
}

/// The vendor's own figure for the call, in US dollars, where `usage`
/// reports one (`docs/events.md`, "Usage and notices"): OpenRouter's
/// `usage.cost`. Missing, `null`, a string or a negative number reports
/// none; a cost of `0` is reported, so `0.0`.
fn cost(usage: &Value) -> Option<f64> {
    usage
        .get("cost")
        .and_then(Value::as_f64)
        .filter(|cost| *cost >= 0.0)
}
