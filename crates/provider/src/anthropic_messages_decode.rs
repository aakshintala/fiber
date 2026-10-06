//! The `anthropic-messages` reply decoder: the server-sent event stream
//! into a `Reply` (`anthropic_messages`).

use std::collections::BTreeMap;
use std::io::BufRead;

use contract::events::{
    ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested,
};
use contract::provider::{Delta, Finish, Reply, ReplyAction};
use contract::shapes::Tokens;
use contract::{GenerationId, ProviderCallId};
use serde_json::{Map, Value, json};

use crate::Error;

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
            Some(Block::Text(text)) if !text.is_empty() => {
                self.actions.push(ReplyAction::Text(TextCompleted {
                    text,
                    provider_item: None,
                }));
            }
            Some(Block::Text(_)) => {}
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
                    ran_by: None,
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
            // debt: pause_turn (a hosted tool's loop paused) stays an unknown
            // stop reason while Fiber sends no hosted tool; #372 continues it.
            Some(other) => return Err(Error::UnknownStopReason(other.to_owned())),
            None => {
                return Err(Error::StreamIncomplete(
                    "message_delta never arrived".into(),
                ));
            }
        };
        let web_searches = self
            .usage
            .get("server_tool_use")
            .and_then(|use_| use_.get("web_search_requests"))
            .and_then(serde_json::Value::as_u64)
            .filter(|n| *n > 0);
        Ok(Reply {
            actions: std::mem::take(&mut self.actions),
            finish,
            generation_id: GenerationId(std::mem::take(&mut self.id)),
            tokens: tokens(&Value::Object(std::mem::take(&mut self.usage))),
            web_searches,
        })
    }
}

/// `usage` as `tokens`. Anthropic reports `input_tokens` already excluding
/// cache reads and writes, unlike `openai-responses` (`docs/events.md`), and
/// splits writes by lifetime in `cache_creation`.
///
/// debt: a usage with `cache_creation_input_tokens` but no
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
