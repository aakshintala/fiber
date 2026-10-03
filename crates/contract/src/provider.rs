//! The provider seam (`docs/architecture.md`, "Provider seam"): "send this to
//! a model and stream back actions". The loop reaches every protocol through
//! [`Provider`] and [`ModelCall`], and never names one.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::{
    CacheLifetime, ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta,
    ToolCallRequested,
};
use crate::shapes::{Failure, Tokens};
use crate::{ActionId, GenerationId};

/// One model, reached through its protocol. A provider extension's model
/// becomes one of these.
pub trait Provider: Send + Sync {
    /// Prepares a call. Nothing is sent until [`ModelCall::run`].
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall>;
}

/// One model call. The loop runs it on its own thread, and any other thread
/// may cancel it while it runs (`docs/architecture.md`, "Cancellation").
pub trait ModelCall: Send + Sync {
    /// Sends the request and reads the reply, passing each fragment to `sink`
    /// as it arrives. Blocks until the reply ends, fails, or is cancelled.
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError>;

    /// Ends the call from another thread: closes its socket, so a `run`
    /// blocked on a read returns [`CallError::Cancelled`]. Before `run`, it
    /// makes `run` return at once.
    fn cancel(&self);
}

/// What a model is sent: the preamble's parts and the conversation
/// (`docs/prompt-cache.md`, "What a request is built from").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// The system prompt.
    pub system_prompt: String,
    /// The tools the model may call, in any order: the protocol sorts them by
    /// name.
    pub tools: Vec<ToolDefinition>,
    /// The reasoning effort, where the model takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The tool choice, as `preamble_built` records it. It does not change
    /// during a session (`docs/prompt-cache.md`, "Tools").
    pub tool_choice: String,
    /// The cache lifetime, sent only where the protocol offers a choice
    /// (`docs/prompt-cache.md`, "Cache lifetime").
    pub cache_lifetime: CacheLifetime,
    /// The cache key for providers that route by key: the root session's id,
    /// or for the reviewer the reviewed session's id plus `reviewer`
    /// (`docs/prompt-cache.md`, "Cache markers and keys" and "Rules for other
    /// areas"). The caller builds it.
    pub cache_key: String,
    /// The conversation, in log order.
    pub conversation: Vec<Input>,
    /// The index into `conversation` where the previous request in this
    /// session ended; `None` on the first request. Anthropic puts its second
    /// cache marker there (`docs/prompt-cache.md`, "Cache markers and keys").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_end: Option<usize>,
}

/// One tool as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// The tool's name.
    pub name: String,
    /// What it does.
    pub description: String,
    /// Its arguments' JSON Schema.
    pub input_schema: Value,
    /// Whether it is declared with `defer_loading`, outside the cached
    /// prefix (`docs/prompt-cache.md`, "Deferred tools"). Set only for a model
    /// whose provider data says deferral works (`docs/model-routing.md`,
    /// "What a provider extension declares").
    #[serde(default)]
    pub deferred: bool,
}

/// One piece of the conversation, rendered from the log's durable events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    /// A person's message.
    User {
        /// Its text.
        text: String,
    },
    /// One text part of a model's reply, from `text_completed`.
    Assistant {
        /// The model reference that produced it, `provider/model`. Its
        /// `provider_item` goes only to that model; its words go to every
        /// model (`docs/loop.md`, "What the model is sent").
        model: String,
        /// The part's text.
        text: String,
        /// The provider's own form of the part, sent back unchanged only to
        /// `model`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_item: Option<Value>,
    },
    /// A reasoning action, from `reasoning_completed`.
    Reasoning {
        /// The model reference that produced it, `provider/model`. A request
        /// to any other model reference leaves it out (`docs/loop.md`, "What
        /// the model is sent").
        model: String,
        /// Its readable text.
        text: String,
        /// The provider's item exactly as it arrived, sent back unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_item: Option<Value>,
    },
    /// A tool call, from `tool_call_requested`.
    ToolCall {
        /// The call's action.
        action_id: ActionId,
        /// The call as the model made it.
        call: ToolCallRequested,
    },
    /// A tool call's result, from `tool_call_completed`.
    ToolResult {
        /// The call's action.
        action_id: ActionId,
        /// The text the model is sent.
        text: String,
        /// Whether the call's `tool_call_completed` is `failed`. A protocol
        /// sends it as its error flag.
        // Carried, not yet sent: each protocol encodes it in #340
        // (`docs/tools.md`, "What a result carries").
        #[serde(default)]
        is_error: bool,
    },
}

/// A fragment of a reply, emitted as an ephemeral event as it arrives.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    /// `assistant_message_delta`.
    Text(TextDelta),
    /// `reasoning_delta`.
    Reasoning(TextDelta),
    /// `tool_call_arguments_delta`.
    ToolCallArguments(ToolCallArgumentsDelta),
}

/// A reply that reached its protocol's terminal event.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// Its text parts, reasoning and tool calls, in the order the model
    /// emitted them.
    pub actions: Vec<ReplyAction>,
    /// Why the reply ended.
    pub finish: Finish,
    /// The provider's id for the generation.
    pub generation_id: GenerationId,
    /// The call's tokens.
    pub tokens: Tokens,
}

impl Reply {
    /// The reply's text: its text parts concatenated, in order, with no
    /// separator.
    pub fn text(&self) -> String {
        self.actions
            .iter()
            .filter_map(|action| match action {
                ReplyAction::Text(part) => Some(part.text.as_str()),
                ReplyAction::Reasoning(_) | ReplyAction::ToolCall(_) => None,
            })
            .collect()
    }
}

/// A durable action in a reply.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyAction {
    /// `text_completed`.
    Text(TextCompleted),
    /// `reasoning_completed`.
    Reasoning(ReasoningCompleted),
    /// `tool_call_requested`.
    ToolCall(ToolCallRequested),
}

/// Why a reply ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// The model finished.
    Completed,
    /// The request's output-token limit cut it off (`docs/loop.md`, "A reply
    /// cut off by the output limit").
    OutputLimit,
}

/// A call that produced no reply.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    /// It failed (`docs/errors.md`, "A failed model call"). Nothing it
    /// streamed is kept.
    Failed {
        /// The failure, as the failed assistant message records it.
        failure: Failure,
        /// The response's `x-should-retry` header, which overrides whether
        /// the code is retried (`docs/model-routing.md`, "When a model call
        /// fails"); `None` when the response carried none.
        should_retry: Option<bool>,
    },
    /// [`ModelCall::cancel`] ended it.
    Cancelled,
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
