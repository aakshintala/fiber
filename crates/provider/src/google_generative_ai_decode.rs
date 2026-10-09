//! Decodes Google Generative AI server-sent events into provider replies.

use std::collections::BTreeMap;
use std::io::BufRead;

use contract::ProviderCallId;
use contract::events::{
    CallStatus, ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta,
    ToolCallRequested,
};
use contract::provider::{CallUsage, Delta, Finish, HostedCall, InputSize, Reply, ReplyAction};
use contract::shapes::{ContentPart, Tokens};
use serde_json::{Value, json};

use crate::google_generative_ai::str_at;
use crate::google_generative_ai_request::with;
use crate::unfinished::named;
use crate::{Error, sse};

/// Reads a reply stream, passing each fragment to `sink` as it arrives, and
/// returns the reply once the stream ends after a `finishReason`. A stream
/// that fails keeps nothing it streamed, finished tool calls included.
pub fn decode(stream: impl BufRead, sink: &mut dyn FnMut(Delta)) -> Result<Reply, Error> {
    decode_tracked(stream, sink).map_err(|(error, _)| error)
}

/// As [`decode`], also carrying what the stream had seen when it failed:
/// the generation and its usage once a chunk named them, else none. A
/// count the decoder cannot represent is `0`, and only that count.
#[allow(
    clippy::result_large_err,
    reason = "the decode carries its partial alongside the error for the call's usage"
)]
pub(crate) fn decode_tracked(
    stream: impl BufRead,
    sink: &mut dyn FnMut(Delta),
) -> Result<Reply, (Error, Option<CallUsage>)> {
    let mut reply = Decoder::default();
    let read = sse::read(stream, |data| {
        let chunk: Value = serde_json::from_str(data)
            .map_err(|e| Error::StreamIncomplete(format!("an event is not JSON ({e})")))?;
        reply.chunk(&chunk, sink)?;
        Ok(false)
    });
    if let Err(error) = read {
        return Err((error, Some(reply.partial())));
    }
    let partial = reply.partial();
    reply.finish().map_err(|error| (error, Some(partial)))
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
    /// Unsigned text fragments joined into the open run.
    run: String,
    actions: Vec<ReplyAction>,
    thought: Option<Thought>,
    /// A `toolCall` part waiting for the `toolResponse` after it: at most
    /// one waits, and only the part directly after it can pair with it.
    waiting: Option<Value>,
    /// The candidate's last `groundingMetadata`, placed on the reply's
    /// last hosted completion at the end of the stream.
    grounding: Option<Value>,
    calls: u32,
    finish_reason: Option<String>,
    finish_message: Option<String>,
    usage: Value,
}

impl Decoder {
    /// What the stream had seen: the generation once a chunk named it, and
    /// the usage so far. A count the decoder cannot represent is `0`, and
    /// only that count.
    fn partial(&self) -> CallUsage {
        CallUsage {
            generation_id: named(self.id.clone()),
            tokens: partial_tokens(&self.usage),
            web_searches: None,
            input_size: InputSize::default(),
        }
    }

    /// Takes one `GenerateContentResponse`.
    fn chunk(&mut self, chunk: &Value, sink: &mut dyn FnMut(Delta)) -> Result<(), Error> {
        if let Some(error) = chunk.get("error") {
            return Err(crate::error::stream_failure(
                error,
                error
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ));
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
        // The search's sources arrive only here, on the last chunk: the
        // last one seen wins, and it is logged, never sent.
        if let Some(grounding) = candidate.get("groundingMetadata") {
            self.grounding = Some(grounding.clone());
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
        // A hosted search's call waits for the part after it; its result
        // pairs only with that part, when their ids match.
        if part.get("toolCall").is_some() {
            self.close_thought();
            self.close_run();
            self.flush_waiting();
            self.waiting = Some(part.clone());
            return;
        }
        if part.get("toolResponse").is_some() {
            self.close_thought();
            self.close_run();
            self.respond(part);
            return;
        }
        // Any other part first logs a waiting call as raw reasoning: the
        // response that would pair with it did not come directly after.
        self.flush_waiting();
        let signature = part
            .get("thoughtSignature")
            .map(|s| json!({ "thoughtSignature": s }));
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            self.close_run();
            let piece = str_at(part, "text");
            let thought = self.thought.get_or_insert(Thought {
                text: String::new(),
                signature: None,
            });
            thought.text.push_str(piece);
            let signed = signature.is_some();
            if signed {
                thought.signature = signature;
            }
            if !piece.is_empty() {
                sink(Delta::Reasoning(TextDelta {
                    text: piece.to_owned(),
                }));
            }
            // A signature ends its thought: the next thought part is its own.
            if signed {
                self.close_thought();
            }
            return;
        }
        self.close_thought();
        if let Some(call) = part.get("functionCall") {
            self.close_run();
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
                ran_by: None,
                provider_item: None,
            }));
            return;
        }
        self.text_fragment(part, sink);
    }

    /// Logs the waiting `toolCall` part as raw reasoning: the response
    /// that would pair with it never came directly after it. A missing
    /// part is not refused.
    fn flush_waiting(&mut self) {
        if let Some(call) = self.waiting.take() {
            self.actions
                .push(ReplyAction::Reasoning(ReasoningCompleted {
                    text: String::new(),
                    provider_item: Some(call),
                }));
        }
    }

    /// A `toolResponse` part: with the waiting `toolCall` of the same id,
    /// the hosted search they are; otherwise each is raw reasoning, the
    /// waiting call first. A hosted search streams no arguments.
    fn respond(&mut self, part: &Value) {
        match self.waiting.take() {
            Some(call) if tool_id(&call) == response_id(part) => {
                self.actions
                    .push(ReplyAction::Hosted(hosted_pair(&call, part)));
            }
            waiting => {
                if let Some(call) = waiting {
                    self.actions
                        .push(ReplyAction::Reasoning(ReasoningCompleted {
                            text: String::new(),
                            provider_item: Some(call),
                        }));
                }
                self.actions
                    .push(ReplyAction::Reasoning(ReasoningCompleted {
                        text: String::new(),
                        provider_item: Some(part.clone()),
                    }));
            }
        }
    }

    /// A text fragment: no `thought: true` and no `functionCall`. Unsigned
    /// fragments join one run. A signed fragment is its own part, and an
    /// empty signed fragment's signature rides on the run before it.
    fn text_fragment(&mut self, part: &Value, sink: &mut dyn FnMut(Delta)) {
        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
        let signed = part.get("thoughtSignature").is_some();
        if text.is_empty() && !signed {
            return;
        }
        if signed && text.is_empty() {
            if self.run.is_empty() {
                self.push_text(String::new(), Some(part.clone()));
            } else {
                let run = std::mem::take(&mut self.run);
                let mut item = part.clone();
                if let Some(map) = item.as_object_mut() {
                    map.insert("text".into(), Value::String(run.clone()));
                }
                self.push_text(run, Some(item));
            }
            return;
        }
        if signed {
            self.close_run();
            self.push_text(text.to_owned(), Some(part.clone()));
            sink(Delta::Text(TextDelta {
                text: text.to_owned(),
            }));
            return;
        }
        self.run.push_str(text);
        sink(Delta::Text(TextDelta {
            text: text.to_owned(),
        }));
    }

    /// Logs one text part.
    fn push_text(&mut self, text: String, provider_item: Option<Value>) {
        self.actions.push(ReplyAction::Text(TextCompleted {
            text,
            provider_item,
        }));
    }

    /// Logs the open unsigned run, when it holds any text.
    fn close_run(&mut self) {
        if self.run.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.run);
        self.push_text(text, None);
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
        self.close_run();
        // A call at the end of the stream never met its response.
        self.flush_waiting();
        if let Some(grounding) = self.grounding.take() {
            place_grounding(&mut self.actions, &grounding);
        }
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
            actions: self.actions,
            finish,
            generation_id: named(self.id),
            tokens: tokens(&self.usage)?,
            web_searches: None,
            cost: None,
            input_size: InputSize::default(),
        })
    }
}

/// A hosted search from its `toolCall` and `toolResponse` parts: the
/// call names `web_search`, with the call's `args` as its arguments (`{}`
/// when absent or not an object); the result is completed with empty
/// text, or failed when the response carries an `error`.
fn hosted_pair(call: &Value, response: &Value) -> HostedCall {
    let tool = call.get("toolCall").unwrap_or(&Value::Null);
    let arguments = tool
        .get("args")
        .filter(|args| args.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let completed = match tool_error(response) {
        // The shape is unprobed: no probe produced a failed search.
        Some(code) => crate::hosted::failed(response.clone(), code),
        None => crate::hosted::completed(response.clone(), &[]),
    };
    HostedCall {
        call: ToolCallRequested {
            name: "web_search".to_owned(),
            arguments,
            provider_id: match str_at(tool, "id") {
                "" => None,
                id => Some(ProviderCallId(id.to_owned())),
            },
            repair: None,
            ran_by: None,
            provider_item: Some(call.clone()),
        },
        completed,
    }
}

/// The `toolCall` part's id, read as `""` when absent.
fn tool_id(call: &Value) -> &str {
    call.get("toolCall")
        .map(|tool| str_at(tool, "id"))
        .unwrap_or("")
}

/// The `toolResponse` part's id, read as `""` when absent.
fn response_id(part: &Value) -> &str {
    part.get("toolResponse")
        .map(|tool| str_at(tool, "id"))
        .unwrap_or("")
}

/// A `toolResponse`'s error code: its `response.error.status` when a
/// string, else `unknown`.
fn tool_error(response: &Value) -> Option<&str> {
    response
        .get("toolResponse")
        .and_then(|tool| tool.get("response"))
        .and_then(|response| response.get("error"))
        .map(|error| {
            error
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        })
}

/// Logs the candidate's `groundingMetadata` on the reply's last hosted
/// completion: its `details`, shown to clients and never sent to the
/// model. A completed search carries the grounding's result URLs, one per
/// line; earlier searches keep their empty text, and a failed search keeps
/// its failure text. Grounding with no hosted pair logs nothing.
fn place_grounding(actions: &mut [ReplyAction], grounding: &Value) {
    let Some(hosted) = actions.iter_mut().rev().find_map(|action| match action {
        ReplyAction::Hosted(hosted) => Some(hosted),
        ReplyAction::Reasoning(_) | ReplyAction::Text(_) | ReplyAction::ToolCall(_) => None,
    }) else {
        return;
    };
    hosted.completed.details = Some(grounding.clone());
    if hosted.completed.status == CallStatus::Completed {
        let urls: Vec<&str> = grounding
            .get("groundingChunks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|chunk| chunk.pointer("/web/uri").and_then(Value::as_str))
            .collect();
        hosted.completed.content = vec![ContentPart::Text {
            text: urls.join("\n"),
        }];
    }
}

/// `usageMetadata` as `tokens`. `promptTokenCount` includes the cached
/// tokens, which `tokens.input` excludes (`docs/events.md`); output is the
/// candidates' tokens plus the thoughts' (ai.google.dev/api/generate-content,
/// `UsageMetadata`). Gemini reports no cache writes. The counts are the
/// provider's, so an overflowing sum fails the reply instead of panicking
/// (`docs/code-quality.md`, "Panics").
///
/// debt: `toolUsePromptTokenCount` (a hosted tool's prompt) is not
/// counted; no probe's `usageMetadata` reported that field.
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

/// `usageMetadata` as a partial's tokens: as [`tokens`], except an
/// overflowing output is `0` while the readable input and cache stay.
fn partial_tokens(usage: &Value) -> Tokens {
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let cached = count("cachedContentTokenCount");
    Tokens {
        input: count("promptTokenCount").saturating_sub(cached),
        cache_read: cached,
        cache_write: BTreeMap::new(),
        output: count("candidatesTokenCount")
            .checked_add(count("thoughtsTokenCount"))
            .unwrap_or(0),
    }
}
