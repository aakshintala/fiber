//! The `google-generative-ai` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): the Gemini API, Google Vertex (Gemini) and OpenCode Zen's
//! Gemini models, Gemini 3 and later. A request is a `POST` to
//! `<base_url>/models/<model>:streamGenerateContent?alt=sse`; the reply is a
//! server-sent event stream of `GenerateContentResponse` objects, the last
//! carrying the candidate's `finishReason`
//! (`research/google-generative-ai-probe`).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::Arc;

use contract::events::{ReasoningCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested};
use contract::provider::{
    CallError, Delta, Finish, Input, ModelCall, ModelRequest, Provider, Reply, ReplyAction,
};
use contract::shapes::Tokens;
use contract::{ActionId, GenerationId, ProviderCallId};
use serde_json::{Map, Value, json};

use crate::http::{self, Cancel};
use crate::{Endpoint, Error, sse, strict};

/// One model reached over `google-generative-ai`.
#[derive(Debug, Clone)]
pub struct Gemini {
    endpoint: Endpoint,
    cache_key_header: Option<String>,
}

impl Gemini {
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
            (
                "user-agent".to_owned(),
                concat!("fiber/", env!("CARGO_PKG_VERSION")).to_owned(),
            ),
        ];
        // The header and `?key=` both authenticate
        // (`docs/model-routing.md`, "Google Generative AI wire facts"); the
        // header keeps the key out of the URL.
        if let Some(key) = &endpoint.key {
            headers.push(("x-goog-api-key".to_owned(), key.clone()));
        }
        headers.extend(endpoint.headers.iter().cloned());
        if let Some(name) = &self.cache_key_header {
            headers.push((name.clone(), request.cache_key.clone()));
        }
        Call {
            url: format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                endpoint.base_url.trim_end_matches('/'),
                endpoint.model
            ),
            headers,
            body: body(endpoint, request),
            provider: endpoint.provider.clone(),
            cancel: Arc::default(),
        }
    }
}

impl Provider for Gemini {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }
}

/// One `google-generative-ai` call, ready to send.
#[derive(Debug)]
pub struct Call {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    provider: String,
    cancel: Arc<Cancel>,
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let (reply, should_retry) =
            match http::post(&self.url, &self.headers, &self.body, &self.cancel) {
                Ok((stream, should_retry)) => (decode(BufReader::new(stream), sink), should_retry),
                Err(e) => {
                    let should_retry = e.should_retry();
                    (Err(retry_info(e)), should_retry)
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

/// The wait a failed response asks for: `Retry-After`, or else the error
/// body's `RetryInfo.retryDelay`, a protobuf duration such as `"37s"`
/// (`docs/model-routing.md`, "When a model call fails"; the shape is
/// `google.rpc.RetryInfo` in googleapis' `google/rpc/error_details.proto`).
fn retry_info(error: Error) -> Error {
    match error {
        Error::Status {
            status,
            body,
            retry_after: None,
            should_retry,
        } => {
            let retry_after = serde_json::from_str::<Value>(&body).ok().and_then(|v| {
                v.pointer("/error/details")?
                    .as_array()?
                    .iter()
                    .find(|d| str_at(d, "@type") == "type.googleapis.com/google.rpc.RetryInfo")?
                    .get("retryDelay")?
                    .as_str()?
                    .strip_suffix('s')?
                    .parse::<f64>()
                    .ok()
            });
            Error::Status {
                status,
                body,
                retry_after,
                should_retry,
            }
        }
        other @ (Error::Status { .. }
        | Error::Connection(_)
        | Error::StreamIncomplete(_)
        | Error::ReplyFailed { .. }
        | Error::UnknownStopReason(_)
        | Error::ContextOverflow(_)
        | Error::Refused(_)) => other,
    }
}

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes"). Gemini caches implicitly only, so the body carries no cache
/// marker or key (`docs/prompt-cache.md`, "Cache markers and keys").
fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
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
        // `parametersJsonSchema` takes the schema as written, `$ref` and
        // `anyOf` included; `parameters` rejects `$ref`
        // (`docs/model-routing.md`, "Google Generative AI wire facts").
        // Nothing rewrites a schema.
        let declarations: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parametersJsonSchema": tool.input_schema,
                })
            })
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
    if let Some(effort) = &request.effort {
        // `thinkingLevel` is Gemini 3's dialect (`MINIMAL`, `LOW`,
        // `MEDIUM`, `HIGH`); Fiber targets Gemini 3 and later.
        thinking.insert("thinkingLevel".into(), json!(effort.to_ascii_uppercase()));
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
    if let (Some(limit), Some(Value::Object(generation))) =
        (endpoint.max_output_tokens, body.get_mut("generationConfig"))
    {
        let max = generation
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map_or(limit, |n| n.min(limit));
        generation.insert("maxOutputTokens".into(), json!(max));
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
    // Each call's name and the id the model gave it, for its response.
    let calls: BTreeMap<&ActionId, &ToolCallRequested> = request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolCall { action_id, call } => Some((action_id, call)),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolResult { .. } => None,
        })
        .collect();

    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    // The open reply's reasoning and calls, in the order they were logged.
    // A reply's `assistant_message_completed` follows its actions, so its
    // text closes it. A user message, a tool result or the end of the
    // conversation closes a reply whose text was never logged.
    let mut logged: Vec<&Input> = Vec::new();
    for input in &request.conversation {
        match input {
            Input::User { text } => {
                close_reply(&mut out, &mut logged, "");
                push(&mut out, "user", json!({"text": text}));
            }
            Input::Assistant { text } => close_reply(&mut out, &mut logged, text),
            // Reasoning goes back unchanged, only to the model that
            // produced it, and never as plain text.
            Input::Reasoning {
                model,
                provider_item: Some(_),
                ..
            } if *model == reference => logged.push(input),
            Input::Reasoning { .. } => {}
            Input::ToolCall { .. } => logged.push(input),
            Input::ToolResult { action_id, text } => {
                close_reply(&mut out, &mut logged, "");
                let call = calls.get(action_id);
                let response = with(
                    json!({
                        "name": call.map_or("", |c| c.name.as_str()),
                        "response": {"output": text},
                    }),
                    call.and_then(|c| c.provider_id.as_ref())
                        .map(|id| json!({"id": id.0})),
                );
                push(&mut out, "user", json!({ "functionResponse": response }));
            }
        }
    }
    close_reply(&mut out, &mut logged, "");
    out.into_iter()
        .map(|(role, parts)| json!({"role": role, "parts": parts}))
        .collect()
}

/// Emits the open reply's model parts, from its logged items and `text`,
/// and drops the items, so nothing carries into the next reply.
fn close_reply(out: &mut Vec<(&'static str, Vec<Value>)>, logged: &mut Vec<&Input>, text: &str) {
    for part in replay_reply(logged, text) {
        push(out, "model", part);
    }
    logged.clear();
}

/// One reply's parts, from its own logged items, in the order they were
/// logged. Each text segment once: the reply's text is those segments in
/// order, so the words before a signed segment go out before that part,
/// and the part keeps the signature it arrived with. A signature that
/// arrived on a call rides on the call. Nothing in here is kept for the
/// next reply (`docs/loop.md`, "What the model is sent").
fn replay_reply(logged: &[&Input], text: &str) -> Vec<Value> {
    let mut parts = Vec::new();
    let mut cursor = 0;
    let mut signature = None;
    for input in logged {
        match input {
            Input::Reasoning {
                provider_item: Some(item),
                ..
            } => {
                if let Some(sig) = waiting_signature(item) {
                    if let Some(earlier) = signature.replace(sig) {
                        parts.push(carrier(earlier));
                    }
                } else if stored_text(item) {
                    cursor = lay_text(&mut parts, text, cursor, item);
                } else {
                    parts.push(item.clone());
                }
            }
            Input::ToolCall { call, .. } => {
                // Only an id the model emitted is sent back
                // (`docs/model-routing.md`, "Google Generative AI wire facts").
                let function = with(
                    json!({"name": call.name, "args": call.arguments}),
                    call.provider_id.as_ref().map(|id| json!({"id": id.0})),
                );
                parts.push(with(json!({ "functionCall": function }), signature.take()));
            }
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolResult { .. } => {}
        }
    }
    if let Some(rest) = text.get(cursor..).filter(|rest| !rest.is_empty()) {
        parts.push(with(json!({"text": rest}), signature.take()));
    } else if let Some(sig) = signature {
        parts.push(carrier(sig));
    }
    parts
}

/// The signed text `item`, after the unsigned words that precede it in
/// this reply's text. `cursor` is how much of that text is already placed.
fn lay_text(parts: &mut Vec<Value>, text: &str, mut cursor: usize, item: &Value) -> usize {
    if let Some(segment) = item.get("text").and_then(Value::as_str)
        && let Some(rest) = text.get(cursor..)
        && let Some(at) = rest.find(segment)
    {
        if let Some(prefix) = rest.get(..at).filter(|prefix| !prefix.is_empty()) {
            parts.push(json!({"text": prefix}));
        }
        cursor += at + segment.len();
    }
    parts.push(item.clone());
    cursor
}

/// Adds `part` to the last content when it has `role`, or opens one.
fn push(out: &mut Vec<(&'static str, Vec<Value>)>, role: &'static str, part: Value) {
    match out.last_mut() {
        Some((last, parts)) if *last == role => parts.push(part),
        _ => out.push((role, vec![part])),
    }
}

/// The bare signature waiting for its part: off a `functionCall` part,
/// or on an empty text part that holds no words of its own. Only the
/// signature waits; an empty text must never overwrite the reply's text.
/// A text part with words of its own is not waiting (`stored_text`): that
/// part is replayed unchanged, in its place in the reply.
fn waiting_signature(item: &Value) -> Option<Value> {
    let map = item.as_object()?;
    let sig = map.get("thoughtSignature")?;
    if map.len() == 1 || map.get("text").and_then(Value::as_str) == Some("") {
        return Some(json!({ "thoughtSignature": sig }));
    }
    None
}

/// Whether a stored item is a text part with words of its own: it
/// replays verbatim, in place. A `thought` part is not text, even with a
/// signature; an explicit `"thought": false` still marks plain text.
/// What the decoder stores decides the rest: a text part is stored whole
/// only with a signature, and never with a `functionCall`.
fn stored_text(item: &Value) -> bool {
    item.as_object().is_some_and(|map| {
        map.get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
            && map.get("thought").and_then(Value::as_bool) != Some(true)
    })
}

/// `object` with the fields of `extra` added, such as a signature that
/// arrived on it or the id the model gave a call.
fn with(mut object: Value, extra: Option<Value>) -> Value {
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

/// Reads a reply stream, passing each fragment to `sink` as it arrives, and
/// returns the reply once the stream ends after a `finishReason`. A stream
/// that fails keeps nothing it streamed, finished tool calls included.
pub fn decode(stream: impl BufRead, sink: &mut dyn FnMut(Delta)) -> Result<Reply, Error> {
    let mut reply = Decoder::default();
    sse::read(stream, |data| {
        let chunk: Value = serde_json::from_str(data)
            .map_err(|e| Error::StreamIncomplete(format!("an event is not JSON ({e})")))?;
        reply.chunk(&chunk, sink)?;
        Ok(false)
    })?;
    reply.finish()
}

/// Thought text as it streams in, until a part that is not a thought.
struct Thought {
    text: String,
    signature: Option<Value>,
}

/// What a reply has produced so far.
#[derive(Default)]
struct Decoder {
    id: String,
    text: String,
    actions: Vec<ReplyAction>,
    thought: Option<Thought>,
    calls: u32,
    finish_reason: Option<String>,
    finish_message: Option<String>,
    usage: Value,
}

impl Decoder {
    /// Takes one `GenerateContentResponse`.
    fn chunk(&mut self, chunk: &Value, sink: &mut dyn FnMut(Delta)) -> Result<(), Error> {
        if let Some(error) = chunk.get("error") {
            return Err(Error::ReplyFailed {
                code: error
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: str_at(error, "message").to_owned(),
            });
        }
        if self.id.is_empty() {
            str_at(chunk, "responseId").clone_into(&mut self.id);
        }
        if let Some(usage) = chunk.get("usageMetadata") {
            self.usage = usage.clone();
        }
        if let Some(reason) = chunk
            .pointer("/promptFeedback/blockReason")
            .and_then(Value::as_str)
        {
            return Err(Error::Refused(format!(
                "it blocked the prompt (`{reason}`)"
            )));
        }
        let candidate = chunk.pointer("/candidates/0").unwrap_or(&Value::Null);
        for part in candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.part(part, sink);
        }
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_owned());
            self.finish_message = candidate
                .get("finishMessage")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        Ok(())
    }

    /// One part of the candidate's content.
    fn part(&mut self, part: &Value, sink: &mut dyn FnMut(Delta)) {
        let signature = part
            .get("thoughtSignature")
            .map(|s| json!({ "thoughtSignature": s }));
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            let piece = str_at(part, "text");
            let thought = self.thought.get_or_insert(Thought {
                text: String::new(),
                signature: None,
            });
            thought.text.push_str(piece);
            if signature.is_some() {
                thought.signature = signature;
            }
            if !piece.is_empty() {
                sink(Delta::Reasoning(TextDelta {
                    text: piece.to_owned(),
                }));
            }
            return;
        }
        self.close_thought();
        if let Some(call) = part.get("functionCall") {
            // A signature on a `functionCall` part is kept as reasoning,
            // in place, and sent back on that call (`docs/loop.md`,
            // "What the model is sent").
            if let Some(signature) = signature {
                self.actions
                    .push(ReplyAction::Reasoning(ReasoningCompleted {
                        text: String::new(),
                        provider_item: Some(signature),
                    }));
            }
            let name = str_at(call, "name").to_owned();
            let arguments = call.get("args").cloned().unwrap_or_else(|| json!({}));
            sink(Delta::ToolCallArguments(ToolCallArgumentsDelta {
                index: self.calls,
                name: Some(name.clone()),
                text: arguments.to_string(),
            }));
            self.calls = self.calls.saturating_add(1);
            self.actions.push(ReplyAction::ToolCall(ToolCallRequested {
                name,
                arguments,
                // A call without an `id` is logged with no `provider_id`
                // (`docs/model-routing.md`, "Google Generative AI wire facts").
                provider_id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(|id| ProviderCallId(id.to_owned())),
                repair: None,
            }));
        } else {
            // A signature on a text part is kept with the part, and sent
            // back on the reply's text, never on a following
            // `functionCall` (`docs/loop.md`, "What the model is sent").
            if signature.is_some() {
                self.actions
                    .push(ReplyAction::Reasoning(ReasoningCompleted {
                        text: String::new(),
                        provider_item: Some(part.clone()),
                    }));
            }
            if let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                self.text.push_str(text);
                sink(Delta::Text(TextDelta {
                    text: text.to_owned(),
                }));
            }
        }
    }

    /// Folds the open thought, if any, into the reply as one `thought` part.
    fn close_thought(&mut self) {
        if let Some(Thought { text, signature }) = self.thought.take() {
            let item = with(json!({"text": text, "thought": true}), signature);
            self.actions
                .push(ReplyAction::Reasoning(ReasoningCompleted {
                    text,
                    provider_item: Some(item),
                }));
        }
    }

    /// The reply, once the stream has ended. Every `FinishReason`
    /// ai.google.dev/api/generate-content documents is mapped
    /// (`docs/errors.md`, "A failed model call").
    fn finish(mut self) -> Result<Reply, Error> {
        self.close_thought();
        let Some(reason) = self.finish_reason.take() else {
            return Err(Error::StreamIncomplete(
                "it ended before a finishReason".into(),
            ));
        };
        let said = || {
            self.finish_message
                .clone()
                .unwrap_or_else(|| format!("the model stopped with `{reason}`"))
        };
        let finish = match reason.as_str() {
            "STOP" => Finish::Completed,
            "MAX_TOKENS" => Finish::OutputLimit,
            // Safety, policy and recitation stops: `refused`.
            "SAFETY"
            | "RECITATION"
            | "LANGUAGE"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
            | "IMAGE_RECITATION"
            | "ESCALATION"
            | "PUP_LIMITED_DISABLED" => return Err(Error::Refused(said())),
            // A reply that went wrong inside the 200 stream: no other code
            // matches, so `stream_incomplete`, and the retry asks again.
            "MALFORMED_FUNCTION_CALL"
            | "UNEXPECTED_TOOL_CALL"
            | "TOO_MANY_TOOL_CALLS"
            | "MISSING_THOUGHT_SIGNATURE"
            | "MALFORMED_RESPONSE"
            | "OTHER"
            | "IMAGE_OTHER"
            | "NO_IMAGE" => {
                return Err(Error::ReplyFailed {
                    code: Some(reason.clone()),
                    message: said(),
                });
            }
            // `FINISH_REASON_UNSPECIFIED` is documented as unused.
            _ => return Err(Error::UnknownStopReason(reason)),
        };
        Ok(Reply {
            text: self.text,
            actions: self.actions,
            finish,
            generation_id: GenerationId(self.id),
            tokens: tokens(&self.usage)?,
        })
    }
}

/// `usageMetadata` as `tokens`. `promptTokenCount` includes the cached
/// tokens, which `tokens.input` excludes (`docs/events.md`); output is the
/// candidates' tokens plus the thoughts' (ai.google.dev/api/generate-content,
/// `UsageMetadata`). Gemini reports no cache writes. The counts are the
/// provider's, so an overflowing sum fails the reply instead of panicking
/// (`docs/code-quality.md`, "Panics").
///
/// ponytail: `toolUsePromptTokenCount` (a hosted tool's prompt) is not
/// counted; Fiber sends no hosted tool on this protocol yet.
fn tokens(usage: &Value) -> Result<Tokens, Error> {
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let cached = count("cachedContentTokenCount");
    let output = count("candidatesTokenCount")
        .checked_add(count("thoughtsTokenCount"))
        .ok_or_else(|| Error::StreamIncomplete("its usageMetadata token counts overflow".into()))?;
    Ok(Tokens {
        input: count("promptTokenCount").saturating_sub(cached),
        cache_read: cached,
        cache_write: BTreeMap::new(),
        output,
    })
}

/// The string at `key`, or `""`.
fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
