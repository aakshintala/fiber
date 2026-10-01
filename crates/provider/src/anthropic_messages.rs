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
use crate::{Endpoint, Error};

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
/// ponytail: `max_tokens`, which Anthropic requires on every call, is sent
/// only when `endpoint.extra_body` carries it. The seam has no per-model
/// output-token limit yet (`docs/errors.md`, "Output tokens"); a provider
/// package sets `max_tokens` in its model data until it does.
///
/// ponytail: deferred tools are sent in full; `defer_loading` needs tool
/// search, which is not built yet (`crates/provider/src/openai_responses.rs`,
/// same note; #326).
fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
    let mut tools: Vec<_> = request.tools.iter().collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let tools: Vec<Value> = tools
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
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
    Value::Object(body).to_string().into_bytes()
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

    // Each conversation index's block, as (message, index within that
    // message's blocks), skipped where the input produced no block.
    let mut groups: Vec<(&'static str, Vec<Value>)> = Vec::new();
    let mut positions: Vec<Option<(usize, usize)>> = Vec::with_capacity(request.conversation.len());
    for input in &request.conversation {
        let next_role = role_of(input);
        if groups.last().map(|(role, _)| *role) != Some(next_role) {
            groups.push((next_role, Vec::new()));
        }
        let last = groups.len() - 1;
        #[allow(clippy::indexing_slicing, reason = "just pushed, so it is in bounds")]
        let group = &mut groups[last];
        match block_of(input, &reference, &call_ids) {
            Some(block) => {
                let block_index = group.1.len();
                group.1.push(block);
                positions.push(Some((last, block_index)));
            }
            None => positions.push(None),
        }
    }

    // Groups that produced no block (a turn that was all drops) are left
    // out; `final_message` maps a surviving group to its position in `out`.
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    let mut final_message: Vec<Option<usize>> = Vec::with_capacity(groups.len());
    for group in groups {
        if group.1.is_empty() {
            final_message.push(None);
        } else {
            final_message.push(Some(out.len()));
            out.push(group);
        }
    }
    let position = |conversation_index: usize| {
        positions
            .get(conversation_index)
            .copied()
            .flatten()
            .and_then(|(group, block)| {
                final_message
                    .get(group)
                    .copied()
                    .flatten()
                    .map(|m| (m, block))
            })
    };

    if let Some(previous_end) = request.previous_end {
        // The last block the previous request ended on: the nearest input at
        // or before `previous_end` that produced one, since the boundary
        // itself may land on a drop.
        if let Some((message, block)) = (0..previous_end).rev().find_map(position) {
            #[allow(
                clippy::indexing_slicing,
                reason = "position() only returns in-range indices"
            )]
            mark(&mut out[message].1[block], &request.cache_lifetime);
        }
    }
    if let Some((role, blocks)) = out.last_mut() {
        let _ = role;
        if let Some(last) = blocks.last_mut() {
            mark(last, &request.cache_lifetime);
        }
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
    usage: Value,
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
            }
            "content_block_start" => self.start(event),
            "content_block_delta" => self.delta(event, sink),
            "content_block_stop" => self.stop(index(event)),
            "message_delta" => {
                self.stop_reason = event
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.usage = event.get("usage").cloned().unwrap_or(Value::Null);
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
            Some(Block::Other) | None => {}
        }
    }

    /// The reply, once `message_stop` has arrived.
    fn finish(&mut self) -> Result<Reply, Error> {
        let finish = match self.stop_reason.as_deref() {
            Some("end_turn" | "tool_use" | "stop_sequence") => Finish::Completed,
            Some("max_tokens") => Finish::OutputLimit,
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
            tokens: tokens(&self.usage),
        })
    }
}

/// `usage` as `tokens`. Anthropic reports `input_tokens` already excluding
/// cache reads and writes, unlike `openai-responses` (`docs/events.md`).
///
/// ponytail: `cache_write` is always empty, as `openai-responses` left it
/// (`crates/provider/src/openai_responses.rs`, "Not done here"); no probed
/// recording wrote to the cache.
fn tokens(usage: &Value) -> Tokens {
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Tokens {
        input: count("input_tokens"),
        cache_read: count("cache_read_input_tokens"),
        cache_write: BTreeMap::new(),
        output: count("output_tokens"),
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
