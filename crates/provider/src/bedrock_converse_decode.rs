//! The `bedrock-converse` reply decoder: ConverseStream's events, each one
//! AWS event-stream message, into a `Reply` (`bedrock_converse`;
//! `docs/model-routing.md`, "Bedrock Converse wire facts").

use std::collections::BTreeMap;
use std::io::BufRead;

use contract::ProviderCallId;
use contract::events::{
    ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested,
};
use contract::provider::{CallUsage, Delta, Finish, InputSize, Reply, ReplyAction};
use contract::shapes::Tokens;
use serde_json::{Map, Value, json};

use crate::Error;
use crate::wire::{args_value, str_at};

/// Reads a reply stream, passing each fragment to `sink` as it arrives, and
/// returns the reply once `metadata` arrives. A stream that fails keeps
/// nothing it streamed, finished tool calls included.
#[cfg(any(test, debug_assertions))]
pub fn decode(stream: impl BufRead, sink: &mut dyn FnMut(Delta)) -> Result<Reply, Error> {
    decode_tracked(stream, sink).map_err(|(error, _)| error)
}

/// As `decode`, also carrying what the stream had seen when it failed: its
/// usage so far, under no generation, since no event names one; the call
/// names it from the response's `x-amzn-requestid`.
#[allow(
    clippy::result_large_err,
    reason = "the decode carries its partial alongside the error for the call's usage"
)]
pub(crate) fn decode_tracked(
    stream: impl BufRead,
    sink: &mut dyn FnMut(Delta),
) -> Result<Reply, (Error, Option<CallUsage>)> {
    let mut decoder = Decoder::default();
    let mut end = None;
    let read = crate::event_stream::read(stream, |message| {
        if let Some(failure) = message.failure() {
            return Err(failure);
        }
        let Some(kind) = message.header(":event-type") else {
            return Ok(false);
        };
        let event: Value = serde_json::from_slice(&message.payload)
            .map_err(|e| Error::StreamIncomplete(format!("a `{kind}` event is not JSON ({e})")))?;
        end = decoder.event(kind, &event, sink)?;
        Ok(end.is_some())
    });
    if let Err(error) = read {
        return Err((error, Some(decoder.partial())));
    }
    match end {
        Some(reply) => Ok(reply),
        None => Err((
            Error::StreamIncomplete("it ended before metadata".into()),
            Some(decoder.partial()),
        )),
    }
}

/// One content block as it streams in.
enum Block {
    Text(String),
    Reasoning {
        text: String,
        signature: String,
        /// Reasoning the vendor encrypted, as base64, kept exactly as it came.
        redacted: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
}

/// What a reply has produced so far.
#[derive(Default)]
struct Decoder {
    actions: Vec<ReplyAction>,
    blocks: BTreeMap<u64, Block>,
    /// Each tool-use block's position among tool calls, by its index.
    call_order: BTreeMap<u64, u32>,
    stop_reason: Option<String>,
    usage: Map<String, Value>,
}

impl Decoder {
    /// What the stream had seen: its usage so far, which only `metadata`
    /// carries.
    fn partial(&self) -> CallUsage {
        CallUsage {
            generation_id: None,
            tokens: tokens(&self.usage),
            web_searches: None,
            input_size: InputSize::default(),
        }
    }

    /// Takes one event; returns the reply once `metadata` arrives.
    fn event(
        &mut self,
        kind: &str,
        event: &Value,
        sink: &mut dyn FnMut(Delta),
    ) -> Result<Option<Reply>, Error> {
        match kind {
            "contentBlockStart" => self.start(event),
            "contentBlockDelta" => self.delta(event, sink),
            "contentBlockStop" => self.stop(index(event)),
            "messageStop" => {
                self.stop_reason = event
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "metadata" => {
                if let Some(Value::Object(usage)) = event.get("usage") {
                    self.usage.clone_from(usage);
                }
                return self.finish().map(Some);
            }
            // `messageStart` and anything Fiber does not act on.
            _ => {}
        }
        Ok(None)
    }

    /// A block Converse opened with a start: only a tool call's carries
    /// one, with its id and name.
    fn start(&mut self, event: &Value) {
        let Some(tool) = event.pointer("/start/toolUse") else {
            return;
        };
        let i = index(event);
        let next = u32::try_from(self.call_order.len()).unwrap_or(u32::MAX);
        self.call_order.insert(i, next);
        self.blocks.insert(
            i,
            Block::ToolUse {
                id: str_at(tool, "toolUseId").to_owned(),
                name: str_at(tool, "name").to_owned(),
                input: String::new(),
            },
        );
    }

    /// A delta into a block, opening a text or reasoning block on its first
    /// delta, since those have no start.
    fn delta(&mut self, event: &Value, sink: &mut dyn FnMut(Delta)) {
        let i = index(event);
        let Some(delta) = event.get("delta") else {
            return;
        };
        if let Some(piece) = delta.get("text").and_then(Value::as_str) {
            if let Block::Text(text) = self
                .blocks
                .entry(i)
                .or_insert_with(|| Block::Text(String::new()))
            {
                text.push_str(piece);
                if !piece.is_empty() {
                    sink(Delta::Text(TextDelta {
                        text: piece.to_owned(),
                    }));
                }
            }
        } else if let Some(reasoning) = delta.get("reasoningContent") {
            if let Block::Reasoning {
                text,
                signature,
                redacted,
            } = self.blocks.entry(i).or_insert_with(|| Block::Reasoning {
                text: String::new(),
                signature: String::new(),
                redacted: String::new(),
            }) {
                let piece = str_at(reasoning, "text");
                text.push_str(piece);
                signature.push_str(str_at(reasoning, "signature"));
                redacted.push_str(str_at(reasoning, "redactedContent"));
                if !piece.is_empty() {
                    sink(Delta::Reasoning(TextDelta {
                        text: piece.to_owned(),
                    }));
                }
            }
        } else if let Some(Value::String(piece)) = delta.pointer("/toolUse/input")
            && let Some(Block::ToolUse { name, input, .. }) = self.blocks.get_mut(&i)
        {
            input.push_str(piece);
            sink(Delta::ToolCallArguments(ToolCallArgumentsDelta {
                index: self.call_order.get(&i).copied().unwrap_or(0),
                name: Some(name.clone()),
                text: piece.clone(),
            }));
        }
    }

    /// A block Converse just closed, folded into the reply. An empty text,
    /// which most models open with, is left out.
    fn stop(&mut self, index: u64) {
        match self.blocks.remove(&index) {
            Some(Block::Text(text)) if !text.is_empty() => {
                self.actions.push(ReplyAction::Text(TextCompleted {
                    text,
                    provider_item: None,
                }));
            }
            Some(Block::Reasoning {
                text,
                signature,
                redacted,
            }) => {
                // Logged in the form Converse takes back
                // (`docs/model-routing.md`, "Bedrock Converse wire facts").
                let provider_item = if redacted.is_empty() {
                    let mut reasoning = json!({"text": text});
                    if !signature.is_empty()
                        && let Some(map) = reasoning.as_object_mut()
                    {
                        map.insert("signature".into(), json!(signature));
                    }
                    json!({"reasoningContent": {"reasoningText": reasoning}})
                } else {
                    json!({"reasoningContent": {"redactedContent": redacted}})
                };
                self.actions
                    .push(ReplyAction::Reasoning(ReasoningCompleted {
                        text,
                        provider_item: Some(provider_item),
                    }));
            }
            Some(Block::ToolUse { id, name, input }) => {
                self.actions.push(ReplyAction::ToolCall(ToolCallRequested {
                    name,
                    arguments: args_value(input),
                    provider_id: Some(ProviderCallId(id)),
                    repair: None,
                    ran_by: None,
                    provider_item: None,
                }));
            }
            Some(Block::Text(_)) | None => {}
        }
    }

    /// The reply, once `metadata` has arrived after `messageStop`.
    fn finish(&mut self) -> Result<Reply, Error> {
        let said = |reason: &str| format!("the model stopped with `{reason}`");
        let finish = match self.stop_reason.as_deref() {
            Some("end_turn" | "tool_use" | "stop_sequence") => Finish::Completed,
            Some("max_tokens") => Finish::OutputLimit,
            Some(reason @ ("guardrail_intervened" | "content_filtered")) => {
                return Err(Error::Refused(said(reason)));
            }
            Some("model_context_window_exceeded") => {
                return Err(Error::ContextOverflow(
                    "the reply reached the end of the model's context window".into(),
                ));
            }
            // A reply that went wrong inside the 200 stream: no other code
            // matches, so `stream_incomplete`, and the retry asks again.
            Some(reason @ ("malformed_model_output" | "malformed_tool_use")) => {
                return Err(Error::ReplyFailed {
                    code: Some(reason.to_owned()),
                    message: said(reason),
                });
            }
            Some(other) => return Err(Error::UnknownStopReason(other.to_owned())),
            None => {
                return Err(Error::StreamIncomplete(
                    "metadata arrived before messageStop".into(),
                ));
            }
        };
        Ok(Reply {
            actions: std::mem::take(&mut self.actions),
            finish,
            generation_id: None,
            tokens: tokens(&self.usage),
            web_searches: None,
            cost: None,
            input_size: InputSize::default(),
        })
    }
}

/// `usage` as `tokens`. `inputTokens` already excludes cache reads and
/// writes, and a write is a 5-minute one: Fiber's `cachePoint` carries no
/// `ttl` (`docs/model-routing.md`, "Bedrock Converse wire facts").
fn tokens(usage: &Map<String, Value>) -> Tokens {
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let write = count("cacheWriteInputTokens");
    Tokens {
        input: count("inputTokens"),
        cache_read: count("cacheReadInputTokens"),
        cache_write: (write > 0)
            .then(|| ("5m".to_owned(), write))
            .into_iter()
            .collect(),
        output: count("outputTokens"),
    }
}

/// The `contentBlockIndex` field on a content-block event.
fn index(event: &Value) -> u64 {
    event
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "bedrock_converse_decode_tests.rs"]
mod tests;
