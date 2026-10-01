//! The `anthropic-messages` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): Anthropic, OpenCode, OpenRouter, Databricks, muse, AWS
//! Bedrock, Google Vertex and Azure Foundry. A request is a `POST` to
//! `<base_url>/messages`; the reply is a server-sent event stream that ends
//! with `message_stop`, which always follows the `message_delta` carrying
//! the stop reason (`research/anthropic-messages-probe`).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
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
use crate::{Endpoint, Error, strict};

/// The wire version every request declares (`research/anthropic-messages-probe`).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One model reached over `anthropic-messages`.
#[derive(Debug, Clone)]
pub struct Messages {
    endpoint: Endpoint,
}

impl Messages {
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
            ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
            (
                "user-agent".to_owned(),
                concat!("fiber/", env!("CARGO_PKG_VERSION")).to_owned(),
            ),
        ];
        if let Some(key) = &endpoint.key {
            headers.push(("x-api-key".to_owned(), key.clone()));
        }
        headers.extend(endpoint.headers.iter().cloned());
        Call {
            url: format!("{}/messages", endpoint.base_url.trim_end_matches('/')),
            headers,
            body: body(endpoint, request),
            provider: endpoint.provider.clone(),
            cancel: Arc::default(),
        }
    }
}

impl Provider for Messages {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }
}

/// One `anthropic-messages` call, ready to send.
#[derive(Debug)]
pub struct Call {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    provider: String,
    cancel: Arc<Cancel>,
}

impl Call {
    /// Sends the request and returns the reply's bytes, unread.
    pub fn open(&self) -> Result<impl Read + use<>, Error> {
        http::post(&self.url, &self.headers, &self.body, &self.cancel).map(|(body, _)| body)
    }
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let (reply, should_retry) =
            match http::post(&self.url, &self.headers, &self.body, &self.cancel) {
                Ok((stream, should_retry)) => (decode(BufReader::new(stream), sink), should_retry),
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

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes").
///
/// ponytail: deferred tools are sent in full; `defer_loading` needs tool
/// search, which is not built yet (`crates/provider/src/openai_responses.rs`,
/// same note; #326).
fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
    let mut tools: Vec<_> = request.tools.iter().collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    // `strict` per tool (`docs/model-routing.md`, "Protocols and
    // providers"). Anthropic's strict subset is wider than OpenAI's (it
    // takes optional properties, `anyOf` and `$ref`), so a schema that fits
    // `strict::fits` fits Anthropic's too. Anthropic refuses a request with
    // more than 20 strict tools (probed 2026-10-01 on `claude-sonnet-5-5`:
    // "The maximum number of strict tools supported is 20"), so past 20 the
    // rest are sent `strict: false`, in name order.
    let mut strict_left = MAX_STRICT_TOOLS;
    let tools: Vec<Value> = tools
        .into_iter()
        .map(|tool| {
            let strict = strict_left > 0 && strict::fits(&tool.input_schema);
            if strict {
                strict_left -= 1;
            }
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
                "strict": strict,
            })
        })
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
    if let Some(effort) = &request.effort {
        body.insert("thinking".into(), json!({"type": "adaptive"}));
        body.insert("output_config".into(), json!({"effort": effort}));
    }
    body.extend(endpoint.extra_body.clone());
    // Anthropic requires `max_tokens`. It is the model's limit, or the
    // model data's own `max_tokens` when that is lower (`docs/errors.md`,
    // "Output tokens").
    if let Some(limit) = endpoint.max_output_tokens {
        let max = body
            .get("max_tokens")
            .and_then(Value::as_u64)
            .map_or(limit, |n| n.min(limit));
        body.insert("max_tokens".into(), json!(max));
    }
    let mut body = Value::Object(body);
    cap_markers(&mut body);
    body.to_string().into_bytes()
}

/// The most tools Anthropic takes with `strict: true` in one request.
const MAX_STRICT_TOOLS: usize = 20;

/// The most cache markers Anthropic takes in one request
/// (`docs/prompt-cache.md`, "Cache markers and keys").
const MAX_MARKERS: usize = 4;

/// Removes cache markers past [`MAX_MARKERS`], counted across `tools`,
/// `system` and `messages` after `extra_body` is merged. The ones kept are
/// in the order `docs/prompt-cache.md` lists: the system prompt's, then the
/// previous end's (any marker in `messages` before the last block), then
/// the new end's, then any others (on tools); within one rank, the earlier
/// in the body first.
fn cap_markers(body: &mut Value) {
    let blocks = |value: Option<&Value>| -> Vec<usize> {
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, block)| block.get("cache_control").is_some())
            .map(|(i, _)| i)
            .collect()
    };
    // (rank, JSON pointer) for every marker.
    let mut found: Vec<(u8, String)> = Vec::new();
    for i in blocks(body.get("system")) {
        found.push((0, format!("/system/{i}")));
    }
    let messages = body.get("messages").and_then(Value::as_array);
    let count = messages.map_or(0, Vec::len);
    for (m, message) in messages.into_iter().flatten().enumerate() {
        let content = message.get("content");
        let last = content.and_then(Value::as_array).map_or(0, Vec::len);
        for b in blocks(content) {
            let rank = if m + 1 == count && b + 1 == last {
                2
            } else {
                1
            };
            found.push((rank, format!("/messages/{m}/content/{b}")));
        }
    }
    for i in blocks(body.get("tools")) {
        found.push((3, format!("/tools/{i}")));
    }
    found.sort_by_key(|(rank, _)| *rank);
    for (_, pointer) in found.iter().skip(MAX_MARKERS) {
        if let Some(block) = body.pointer_mut(pointer).and_then(Value::as_object_mut) {
            block.remove("cache_control");
        }
    }
}

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
fn cache_control(lifetime: &CacheLifetime) -> Value {
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

    // The messages, and each conversation index's block as (message, index
    // within that message's blocks), `None` where the input produced none.
    // An input with no block starts no message, so a turn that was all
    // drops leaves the messages either side of it as one.
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    let mut positions: Vec<Option<(usize, usize)>> = Vec::with_capacity(request.conversation.len());
    for input in &request.conversation {
        let Some(block) = block_of(input, &reference, &call_ids) else {
            positions.push(None);
            continue;
        };
        let role = role_of(input);
        match out.last_mut() {
            Some((last, blocks)) if *last == role => blocks.push(block),
            _ => out.push((role, vec![block])),
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

/// The content block an input renders as, or `None` for an input that adds
/// nothing to the request: an empty assistant text, or reasoning sent to a
/// different model reference (`docs/loop.md`, "What the model is sent").
fn block_of(input: &Input, reference: &str, call_ids: &BTreeMap<&ActionId, &str>) -> Option<Value> {
    match input {
        Input::User { text } => Some(json!({"type": "text", "text": text})),
        Input::Assistant { text } if text.is_empty() => None,
        Input::Assistant { text } => Some(json!({"type": "text", "text": text})),
        // Reasoning goes back unchanged, only to the model that produced it,
        // and never as plain text.
        Input::Reasoning {
            model,
            provider_item,
            ..
        } if model == reference => provider_item.clone(),
        Input::Reasoning { .. } => None,
        Input::ToolCall { action_id, call } => Some(json!({
            "type": "tool_use",
            "id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
            "name": call.name,
            "input": call.arguments,
        })),
        Input::ToolResult { action_id, text } => Some(json!({
            "type": "tool_result",
            "tool_use_id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
            "content": text,
        })),
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

/// Reads a reply stream, passing each fragment to `sink` as it arrives, and
/// returns the reply once `message_stop` arrives. A stream that fails keeps
/// nothing it streamed, finished tool calls included.
pub fn decode(stream: impl BufRead, sink: &mut dyn FnMut(Delta)) -> Result<Reply, Error> {
    let mut reply = Decoder::default();
    let mut end = None;
    crate::sse::read(stream, |data| {
        let event: Value = serde_json::from_str(data)
            .map_err(|e| Error::StreamIncomplete(format!("an event is not JSON ({e})")))?;
        end = reply.event(&event, sink)?;
        Ok(end.is_some())
    })?;
    end.ok_or_else(|| Error::StreamIncomplete("it ended before message_stop".into()))
}

/// One content block as it streams in.
enum Block {
    Text(String),
    Thinking {
        thinking: String,
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        arguments: String,
    },
    /// Reasoning Anthropic encrypted whole, kept exactly as it arrived.
    Redacted(Value),
    /// A block kind Fiber does not act on, such as a hosted tool's.
    Other,
}

/// What a reply has produced so far.
#[derive(Default)]
struct Decoder {
    id: String,
    text: String,
    actions: Vec<ReplyAction>,
    blocks: BTreeMap<u64, Block>,
    /// Each tool-use block's position among tool calls, by its index.
    call_order: BTreeMap<u64, u32>,
    stop_reason: Option<String>,
    /// The explanation a `refusal` stop carried.
    refusal: Option<String>,
    usage: Map<String, Value>,
}

impl Decoder {
    /// Takes one event; returns the reply once `message_stop` arrives.
    fn event(
        &mut self,
        event: &Value,
        sink: &mut dyn FnMut(Delta),
    ) -> Result<Option<Reply>, Error> {
        match str_at(event, "type") {
            "message_start" => {
                self.id = str_at(&event["message"], "id").to_owned();
                if let Some(Value::Object(usage)) = event.pointer("/message/usage") {
                    self.usage.clone_from(usage);
                }
            }
            "content_block_start" => self.start(event),
            "content_block_delta" => self.delta(event, sink),
            "content_block_stop" => self.stop(index(event)),
            "message_delta" => {
                self.stop_reason = event
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.refusal = event
                    .pointer("/delta/stop_details/explanation")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                // The final counts, laid over `message_start`'s: only
                // `message_start` splits cache writes by lifetime
                // (`research/anthropic-messages-probe`, every stream).
                if let Some(Value::Object(last)) = event.get("usage") {
                    self.usage.extend(last.clone());
                }
            }
            "message_stop" => return self.finish().map(Some),
            "error" => {
                return Err(Error::ReplyFailed {
                    code: event
                        .pointer("/error/type")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    message: str_at(&event["error"], "message").to_owned(),
                });
            }
            // `ping` and anything Fiber does not act on.
            _ => {}
        }
        Ok(None)
    }

    /// A content block Anthropic just opened, seeded from whatever it
    /// already carries: nothing for a live stream's placeholder, or the
    /// whole block for a non-streamed reply replayed as one shot
    /// (`research/anthropic-messages-probe`).
    fn start(&mut self, event: &Value) {
        let block = &event["content_block"];
        let block = match str_at(block, "type") {
            "text" => Block::Text(str_at(block, "text").to_owned()),
            "thinking" => Block::Thinking {
                thinking: str_at(block, "thinking").to_owned(),
                signature: str_at(block, "signature").to_owned(),
            },
            "tool_use" => {
                let index = index(event);
                let next = u32::try_from(self.call_order.len()).unwrap_or(u32::MAX);
                self.call_order.insert(index, next);
                // A live stream opens the block with `input: {}`, an empty
                // placeholder that `input_json_delta` fills in; a
                // non-streamed reply, replayed as one shot, carries the
                // whole input here and gets no deltas
                // (`research/anthropic-messages-probe`).
                let input = block.get("input").filter(|i| match i {
                    Value::Object(map) => !map.is_empty(),
                    Value::Array(items) => !items.is_empty(),
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
                });
                Block::ToolUse {
                    id: str_at(block, "id").to_owned(),
                    name: str_at(block, "name").to_owned(),
                    arguments: input.map_or_else(String::new, Value::to_string),
                }
            }
            "redacted_thinking" => Block::Redacted(block.clone()),
            _ => Block::Other,
        };
        self.blocks.insert(index(event), block);
    }

    /// A delta into an already-open content block.
    fn delta(&mut self, event: &Value, sink: &mut dyn FnMut(Delta)) {
        let i = index(event);
        let delta = &event["delta"];
        match (self.blocks.get_mut(&i), str_at(delta, "type")) {
            (Some(Block::Text(text)), "text_delta") => {
                let piece = str_at(delta, "text");
                text.push_str(piece);
                sink(Delta::Text(TextDelta {
                    text: piece.to_owned(),
                }));
            }
            (Some(Block::Thinking { thinking, .. }), "thinking_delta") => {
                let piece = str_at(delta, "thinking");
                thinking.push_str(piece);
                sink(Delta::Reasoning(TextDelta {
                    text: piece.to_owned(),
                }));
            }
            (Some(Block::Thinking { signature, .. }), "signature_delta") => {
                signature.push_str(str_at(delta, "signature"));
            }
            (
                Some(Block::ToolUse {
                    name, arguments, ..
                }),
                "input_json_delta",
            ) => {
                let piece = str_at(delta, "partial_json");
                arguments.push_str(piece);
                sink(Delta::ToolCallArguments(ToolCallArgumentsDelta {
                    index: self.call_order.get(&i).copied().unwrap_or(0),
                    name: Some(name.clone()),
                    text: piece.to_owned(),
                }));
            }
            _ => {}
        }
    }

    /// A content block Anthropic just closed, folded into the reply.
    fn stop(&mut self, index: u64) {
        match self.blocks.remove(&index) {
            Some(Block::Text(text)) => self.text.push_str(&text),
            Some(Block::Thinking {
                thinking,
                signature,
            }) => {
                self.actions
                    .push(ReplyAction::Reasoning(ReasoningCompleted {
                        text: thinking.clone(),
                        provider_item: Some(json!({
                            "type": "thinking",
                            "thinking": thinking,
                            "signature": signature,
                        })),
                    }));
            }
            Some(Block::ToolUse {
                id,
                name,
                arguments,
            }) => {
                let arguments = match serde_json::from_str(&arguments) {
                    Ok(value @ (Value::Object(_) | Value::Array(_))) => value,
                    Ok(_) | Err(_) => Value::String(arguments),
                };
                self.actions.push(ReplyAction::ToolCall(ToolCallRequested {
                    name,
                    arguments,
                    provider_id: Some(ProviderCallId(id)),
                    repair: None,
                }));
            }
            Some(Block::Redacted(item)) => {
                self.actions
                    .push(ReplyAction::Reasoning(ReasoningCompleted {
                        text: String::new(),
                        provider_item: Some(item),
                    }));
            }
            Some(Block::Other) | None => {}
        }
    }

    /// The reply, once `message_stop` has arrived.
    fn finish(&mut self) -> Result<Reply, Error> {
        let finish = match self.stop_reason.as_deref() {
            Some("end_turn" | "tool_use" | "stop_sequence") => Finish::Completed,
            Some("max_tokens") => Finish::OutputLimit,
            Some("refusal") => {
                return Err(Error::Refused(
                    self.refusal
                        .take()
                        .unwrap_or_else(|| "the model stopped with `refusal`".into()),
                ));
            }
            Some("model_context_window_exceeded") => {
                return Err(Error::ContextOverflow(
                    "the reply reached the end of the model's context window".into(),
                ));
            }
            // ponytail: `pause_turn` (a hosted tool's loop paused) stays
            // unknown: no doc says what Fiber does with it, and Fiber sends
            // no hosted tool yet.
            Some(other) => return Err(Error::UnknownStopReason(other.to_owned())),
            None => {
                return Err(Error::StreamIncomplete(
                    "message_delta never arrived".into(),
                ));
            }
        };
        Ok(Reply {
            text: std::mem::take(&mut self.text),
            actions: std::mem::take(&mut self.actions),
            finish,
            generation_id: GenerationId(std::mem::take(&mut self.id)),
            tokens: tokens(&Value::Object(std::mem::take(&mut self.usage))),
        })
    }
}

/// `usage` as `tokens`. Anthropic reports `input_tokens` already excluding
/// cache reads and writes, unlike `openai-responses` (`docs/events.md`), and
/// splits writes by lifetime in `cache_creation`.
///
/// ponytail: a usage with `cache_creation_input_tokens` but no
/// `cache_creation` split (no probed endpoint sent one) reports no write;
/// attribute it to the request's lifetime if such an endpoint turns up.
fn tokens(usage: &Value) -> Tokens {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let cache_write = [
        ("5m", "/cache_creation/ephemeral_5m_input_tokens"),
        ("1h", "/cache_creation/ephemeral_1h_input_tokens"),
    ]
    .into_iter()
    .map(|(lifetime, pointer)| (lifetime.to_owned(), count(pointer)))
    .filter(|(_, n)| *n > 0)
    .collect();
    Tokens {
        input: count("/input_tokens"),
        cache_read: count("/cache_read_input_tokens"),
        cache_write,
        output: count("/output_tokens"),
    }
}

/// The `index` field on a content-block event.
fn index(event: &Value) -> u64 {
    event.get("index").and_then(Value::as_u64).unwrap_or(0)
}

/// The string at `key`, or `""`.
fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
