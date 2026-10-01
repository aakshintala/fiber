//! The `openai-completions` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): OpenCode, OpenRouter, Databricks, muse and Azure. A
//! request is a `POST` to `<base_url>/chat/completions`; the reply is a
//! server-sent event stream of chunks that ends with `data: [DONE]`. The
//! finish reason can arrive twice, and the usage chunk after it
//! (`docs/model-routing.md`, "openai-completions facts").

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::Arc;

use contract::events::{
    CacheLifetime, ReasoningCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested,
};
use contract::provider::{
    CallError, Delta, Finish, Input, ModelCall, ModelRequest, Provider, Reply, ReplyAction,
};
use contract::shapes::Tokens;
use contract::{ActionId, GenerationId, ProviderCallId};
use serde_json::{Map, Value, json};

use crate::http::{self, Cancel};
use crate::{Endpoint, Error, sse, strict};

/// One model reached over `openai-completions`.
#[derive(Debug, Clone)]
pub struct Completions {
    endpoint: Endpoint,
}

impl Completions {
    /// The protocol for one model of one provider.
    pub fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }

    /// Builds the call for `request`. Two calls built from the same inputs
    /// send the same bytes (`docs/prompt-cache.md`, "Bytes").
    pub fn request(&self, request: &ModelRequest) -> Call {
        let endpoint = &self.endpoint;
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("accept".to_owned(), "text/event-stream".to_owned()),
            (
                "user-agent".to_owned(),
                concat!("fiber/", env!("CARGO_PKG_VERSION")).to_owned(),
            ),
        ];
        if let Some(key) = &endpoint.key {
            headers.push(("authorization".to_owned(), format!("Bearer {key}")));
        }
        headers.extend(endpoint.headers.iter().cloned());
        Call {
            url: format!(
                "{}/chat/completions",
                endpoint.base_url.trim_end_matches('/')
            ),
            headers,
            body: body(endpoint, request),
            provider: endpoint.provider.clone(),
            lifetime: request.cache_lifetime,
            cancel: Arc::default(),
        }
    }
}

impl Provider for Completions {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }
}

/// One `openai-completions` call, ready to send.
#[derive(Debug)]
pub struct Call {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    provider: String,
    /// The request's cache lifetime, which a reported cache write is
    /// counted under.
    lifetime: CacheLifetime,
    cancel: Arc<Cancel>,
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let (reply, should_retry) =
            match http::post(&self.url, &self.headers, &self.body, &self.cancel) {
                Ok((stream, should_retry)) => (
                    decode(BufReader::new(stream), &self.lifetime, sink),
                    should_retry,
                ),
                Err(e) => {
                    let should_retry = e.should_retry();
                    (Err(e), should_retry)
                }
            };
        // Whatever a cancelled call returns, the cancel ended it.
        if self.cancel.is_cancelled() {
            return Err(CallError::Cancelled);
        }
        reply.map_err(|e| CallError::Failed {
            failure: e.failure(&self.provider),
            should_retry,
        })
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// The most cache markers Anthropic takes in one request
/// (`docs/prompt-cache.md`, "Cache markers and keys").
const MAX_MARKERS: usize = 4;

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes").
fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
    let mut tools: Vec<_> = request.tools.iter().collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    // ponytail: deferred tools are sent in full until tool search is built
    // (docs/tools.md "Tool search"); see #326.
    let tools: Vec<Value> = tools
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                    "strict": strict::fits(&tool.input_schema),
                },
            })
        })
        .collect();
    let compat = &endpoint.compat;
    let mut body = Map::new();
    body.insert("model".into(), json!(endpoint.model));
    body.insert("messages".into(), Value::Array(messages(endpoint, request)));
    // OpenAI and OpenRouter (which falls back to it for sticky routing)
    // route the cache by this key (`docs/prompt-cache.md`, "Cache markers
    // and keys"; `research/prompt-cache/README.md`).
    body.insert("prompt_cache_key".into(), json!(request.cache_key));
    // OpenAI sends no usage without it (`docs/model-routing.md`,
    // "openai-completions facts").
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    // OpenAI rejects `tool_choice` without `tools`.
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
        body.insert("tool_choice".into(), tool_choice(&request.tool_choice));
    }
    if let Some(effort) = &request.effort {
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
    if let Some(limit) = endpoint.max_output_tokens {
        let max = body
            .get(field)
            .and_then(Value::as_u64)
            .map_or(limit, |n| n.min(limit));
        body.insert(field.into(), json!(max));
    }
    let mut body = Value::Object(body);
    cap_markers(&mut body, &mut 0);
    body.to_string().into_bytes()
}

/// Removes cache markers past [`MAX_MARKERS`], counted in body order:
/// `messages` (the system prompt's, the previous end's, the new end's, as
/// `docs/prompt-cache.md` orders them), then any in `tools` that model data
/// added.
fn cap_markers(value: &mut Value, kept: &mut usize) {
    match value {
        Value::Object(map) => {
            if map.contains_key("cache_control") {
                if *kept < MAX_MARKERS {
                    *kept += 1;
                } else {
                    map.remove("cache_control");
                }
            }
            map.values_mut().for_each(|v| cap_markers(v, kept));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| cap_markers(v, kept)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// `tool_choice` on the wire: `auto`, `none` and `required` are OpenAI's own
/// values; any other string names the one tool to force.
fn tool_choice(choice: &str) -> Value {
    match choice {
        "auto" | "none" | "required" => json!(choice),
        name => json!({"type": "function", "function": {"name": name}}),
    }
}

/// The conversation as Chat Completions messages, after the system prompt.
/// An assistant's reasoning, text and tool calls fold into one message; each
/// tool result is a `tool` message of its own.
///
/// With [`crate::Compat::cache_control`], it carries the markers Anthropic
/// takes (`docs/prompt-cache.md`, "Cache markers and keys"): the end of the
/// system prompt, the point where the previous request ended, and the new
/// end.
fn messages(endpoint: &Endpoint, request: &ModelRequest) -> Vec<Value> {
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
    // Each conversation input's message, `None` for an input that adds
    // nothing.
    let mut positions: Vec<Option<usize>> = Vec::with_capacity(request.conversation.len());
    for input in &request.conversation {
        let at = match input {
            Input::User { text } => {
                out.push(message(json!({"role": "user", "content": text})));
                Some(out.len() - 1)
            }
            Input::ToolResult { action_id, text } => {
                out.push(message(json!({
                    "role": "tool",
                    "tool_call_id": call_id(action_id),
                    "content": text,
                })));
                Some(out.len() - 1)
            }
            Input::Assistant { text } if text.is_empty() => None,
            Input::Assistant { text } => {
                let mut m = take_assistant(&mut out, &["content"]);
                m.insert("content".into(), json!(text));
                out.push(m);
                Some(out.len() - 1)
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
                Some(out.len() - 1)
            }
            Input::Reasoning { .. } => None,
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
                Some(out.len() - 1)
            }
        };
        positions.push(at);
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

    if endpoint.compat.cache_control {
        // The last message the previous request ended on: the nearest input
        // before `previous_end` that produced one.
        let previous_end = request.previous_end.and_then(|end| {
            (0..end)
                .rev()
                .find_map(|i| positions.get(i).copied().flatten())
        });
        let last = out.len().checked_sub(1);
        let mut marked: Vec<usize> = [system, previous_end, last].into_iter().flatten().collect();
        marked.dedup();
        for at in marked {
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
/// ponytail: an assistant message holding only tool calls has no text part
/// to mark and is left unmarked; mark its last call if a session's cache
/// shows the gap.
fn mark(message: &mut Map<String, Value>, lifetime: &CacheLifetime) {
    let Some(Value::String(text)) = message.get("content") else {
        return;
    };
    let marker = match lifetime {
        CacheLifetime::FiveMinutes => json!({"type": "ephemeral"}),
        CacheLifetime::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
    };
    let part = json!([{"type": "text", "text": text, "cache_control": marker}]);
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

/// Reads a reply stream, passing each fragment to `sink` as it arrives, and
/// returns the reply once `[DONE]` arrives. A cache write is counted under
/// `lifetime`, the request's. A stream that fails keeps nothing it
/// streamed, finished tool calls included.
pub fn decode(
    stream: impl BufRead,
    lifetime: &CacheLifetime,
    sink: &mut dyn FnMut(Delta),
) -> Result<Reply, Error> {
    let mut reply = Decoder::default();
    let mut done = false;
    sse::read(stream, |data| {
        if data == "[DONE]" {
            done = true;
            return Ok(true);
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|e| Error::StreamIncomplete(format!("a chunk is not JSON ({e})")))?;
        reply.chunk(&chunk, sink)?;
        Ok(false)
    })?;
    if !done {
        return Err(Error::StreamIncomplete("it ended before [DONE]".into()));
    }
    reply.finish(lifetime)
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
    finish: Option<String>,
    usage: Value,
}

impl Decoder {
    /// Takes one chunk.
    fn chunk(&mut self, chunk: &Value, sink: &mut dyn FnMut(Delta)) -> Result<(), Error> {
        // An error inside the 200 stream, as OpenRouter sends an upstream's
        // failure (`research/provider-errors`, "How an error arrives after
        // HTTP 200").
        if let Some(error) = chunk.get("error").filter(|e| e.is_object()) {
            let code =
                ["/metadata/provider_code", "/code"]
                    .iter()
                    .find_map(|p| match error.pointer(p) {
                        Some(Value::String(code)) => Some(code.clone()),
                        Some(Value::Number(code)) => Some(code.to_string()),
                        _ => None,
                    });
            return Err(Error::ReplyFailed {
                code,
                message: str_at(error, "message").to_owned(),
            });
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

    /// Merges one streamed `reasoning_details` entry into the one of the
    /// same `index` and `type`: its text pieces appended, its other fields
    /// set.
    fn detail(&mut self, entry: &Value) {
        let Value::Object(entry) = entry else {
            return;
        };
        let same = |kept: &Value| {
            kept.get("index") == entry.get("index") && kept.get("type") == entry.get("type")
        };
        let Some(Value::Object(kept)) = self.details.iter_mut().find(|k| same(k)) else {
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
    fn finish(self, lifetime: &CacheLifetime) -> Result<Reply, Error> {
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
        let mut calls: Vec<StreamedCall> = self.calls.into_values().collect();
        calls.sort_by_key(|c| c.position);
        actions.extend(calls.into_iter().map(|call| {
            let arguments = match serde_json::from_str(&call.arguments) {
                Ok(Value::Object(map)) => Value::Object(map),
                Ok(_) | Err(_) => Value::String(call.arguments),
            };
            ReplyAction::ToolCall(ToolCallRequested {
                name: call.name,
                arguments,
                provider_id: Some(ProviderCallId(call.id)).filter(|id| !id.0.is_empty()),
                repair: None,
            })
        }));
        Ok(Reply {
            text: self.text,
            actions,
            finish,
            generation_id: GenerationId(self.id),
            tokens: tokens(&self.usage, lifetime),
        })
    }
}

/// `usage` as `tokens`. OpenAI and OpenRouter count cache reads
/// (`cached_tokens`) and OpenRouter's cache writes (`cache_write_tokens`)
/// inside `prompt_tokens`, and `tokens.input` excludes both
/// (`docs/model-routing.md`, "openai-completions facts"). The vendor does
/// not say which lifetime a write was, so it is the request's.
fn tokens(usage: &Value, lifetime: &CacheLifetime) -> Tokens {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let read = count("/prompt_tokens_details/cached_tokens");
    let write = count("/prompt_tokens_details/cache_write_tokens");
    let key = match lifetime {
        CacheLifetime::FiveMinutes => "5m",
        CacheLifetime::OneHour => "1h",
    };
    Tokens {
        input: count("/prompt_tokens")
            .saturating_sub(read)
            .saturating_sub(write),
        cache_read: read,
        cache_write: if write > 0 {
            BTreeMap::from([(key.to_owned(), write)])
        } else {
            BTreeMap::new()
        },
        output: count("/completion_tokens"),
    }
}

/// The string at `key`, or `""`.
fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
