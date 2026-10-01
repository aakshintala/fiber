//! The `openai-responses` protocol (`docs/model-routing.md`, "Protocols and
//! providers"): OpenAI, ChatGPT/codex, OpenCode and others. A request is a
//! `POST` to `<base_url>/responses`; the reply is a server-sent event stream
//! that ends with `response.completed`, `response.incomplete` or
//! `response.failed`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
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

/// One model reached over `openai-responses`.
#[derive(Debug, Clone)]
pub struct Responses {
    endpoint: Endpoint,
}

impl Responses {
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
            url: format!("{}/responses", endpoint.base_url.trim_end_matches('/')),
            headers,
            body: body(endpoint, request),
            provider: endpoint.provider.clone(),
            cancel: Arc::default(),
        }
    }
}

impl Provider for Responses {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }
}

/// One `openai-responses` call, ready to send.
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
        http::post(&self.url, &self.headers, &self.body, &self.cancel)
    }
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let reply = self
            .open()
            .and_then(|stream| decode(BufReader::new(stream), sink));
        // Whatever a cancelled call returns, the cancel ended it.
        if self.cancel.is_cancelled() {
            return Err(CallError::Cancelled);
        }
        reply.map_err(|e| CallError::Failed {
            failure: e.failure(&self.provider),
            should_retry: e.should_retry(),
        })
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// The request body. Its objects serialise with their keys sorted, because
/// serde_json's `preserve_order` is never on (`docs/prompt-cache.md`,
/// "Bytes").
fn body(endpoint: &Endpoint, request: &ModelRequest) -> Vec<u8> {
    let mut tools: Vec<_> = request.tools.iter().collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let tools: Vec<Value> = tools
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "strict": strict::fits(&tool.input_schema),
            })
        })
        .collect();
    let mut body = Map::new();
    body.insert("model".into(), json!(endpoint.model));
    body.insert("instructions".into(), json!(request.system_prompt));
    body.insert("input".into(), Value::Array(input(endpoint, request)));
    body.insert("tools".into(), Value::Array(tools));
    body.insert("stream".into(), json!(true));
    // Fiber keeps no state at the vendor, so it asks for the encrypted
    // reasoning to send back (`docs/loop.md`, "What the model is sent").
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    if let Some(effort) = &request.effort {
        body.insert("reasoning".into(), json!({ "effort": effort }));
    }
    if let Some(store) = endpoint.compat.store {
        body.insert("store".into(), json!(store));
    }
    body.extend(endpoint.extra_body.clone());
    Value::Object(body).to_string().into_bytes()
}

/// The conversation as Responses input items.
fn input(endpoint: &Endpoint, request: &ModelRequest) -> Vec<Value> {
    let reference = endpoint.reference();
    // A call's `call_id` is the provider's id for it, or its action id when
    // the reply carried none; its result names the same one.
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
    request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text } => Some(json!({ "role": "user", "content": text })),
            Input::Assistant { text } if text.is_empty() => None,
            Input::Assistant { text } => Some(json!({ "role": "assistant", "content": text })),
            // Reasoning goes back unchanged, only to the model that produced
            // it, and never as plain text.
            Input::Reasoning {
                model,
                provider_item,
                ..
            } if *model == reference => provider_item.clone(),
            Input::Reasoning { .. } => None,
            Input::ToolCall { action_id, call } => Some(json!({
                "type": "function_call",
                "call_id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
                "name": call.name,
                "arguments": arguments_text(&call.arguments),
            })),
            Input::ToolResult { action_id, text } => Some(json!({
                "type": "function_call_output",
                "call_id": call_ids.get(action_id).copied().unwrap_or(action_id.0.as_str()),
                "output": text,
            })),
        })
        .collect()
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
pub fn decode(stream: impl BufRead, sink: &mut dyn FnMut(Delta)) -> Result<Reply, Error> {
    let mut reply = Decoder::default();
    let mut end = None;
    sse::read(stream, |data| {
        let event: Value = serde_json::from_str(data)
            .map_err(|e| Error::StreamIncomplete(format!("an event is not JSON ({e})")))?;
        end = reply.event(&event, sink)?;
        Ok(end.is_some())
    })?;
    end.ok_or_else(|| Error::StreamIncomplete("it ended before its terminal event".into()))
}

/// What a reply has produced so far.
#[derive(Default)]
struct Decoder {
    text: String,
    /// What the model said when it refused on policy grounds.
    refusal: Option<String>,
    actions: Vec<ReplyAction>,
    /// Each tool call's index within the message and its name, by item id.
    calls: BTreeMap<String, (u32, Option<String>)>,
}

impl Decoder {
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
                if str_at(item, "type") == "function_call" {
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
            "response.completed" | "response.incomplete" | "response.failed" => {
                return self.finish(&event["response"]).map(Some);
            }
            "error" => {
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
                self.text
                    .push_str(&texts("content", &["output_text"]).concat());
                let refusal = texts("content", &["refusal"]);
                if !refusal.is_empty() {
                    self.refusal = Some(refusal.concat());
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
            "function_call" => {
                let raw = str_at(item, "arguments");
                let arguments = match serde_json::from_str(raw) {
                    Ok(Value::Object(map)) => Value::Object(map),
                    Ok(_) | Err(_) => Value::String(raw.to_owned()),
                };
                self.actions.push(ReplyAction::ToolCall(ToolCallRequested {
                    name: str_at(item, "name").to_owned(),
                    arguments,
                    provider_id: item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .map(|id| ProviderCallId(id.to_owned())),
                    repair: None,
                }));
            }
            // Items Fiber does not act on, such as a hosted tool's.
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
            text: std::mem::take(&mut self.text),
            actions: std::mem::take(&mut self.actions),
            finish,
            generation_id: GenerationId(str_at(response, "id").to_owned()),
            tokens: tokens(&response["usage"]),
        })
    }
}

/// `usage` as `tokens`: Responses counts cached tokens inside
/// `input_tokens`, and `tokens.input` excludes them (`docs/events.md`).
fn tokens(usage: &Value) -> Tokens {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let cached = count("/input_tokens_details/cached_tokens");
    Tokens {
        input: count("/input_tokens").saturating_sub(cached),
        cache_read: cached,
        cache_write: BTreeMap::new(),
        output: count("/output_tokens"),
    }
}

/// The string at `key`, or `""`.
fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
