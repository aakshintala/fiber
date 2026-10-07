//! Decodes Google Generative AI server-sent events into provider replies.

use std::collections::BTreeMap;
use std::io::BufRead;

use contract::events::{
    ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested,
};
use contract::provider::{Delta, Finish, Reply, ReplyAction};
use contract::shapes::Tokens;
use contract::{GenerationId, ProviderCallId};
use serde_json::{Value, json};

use crate::google_generative_ai::str_at;
use crate::google_generative_ai_request::with;
use crate::{Error, sse};

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
    /// Unsigned text fragments joined into the open run.
    run: String,
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
            self.close_run();
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
            generation_id: GenerationId(self.id),
            tokens: tokens(&self.usage)?,
            web_searches: None,
            cost: None,
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
/// debt: `toolUsePromptTokenCount` (a hosted tool's prompt) is not
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
