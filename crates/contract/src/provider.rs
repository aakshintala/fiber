//! The provider seam (`docs/architecture.md`, "Provider seam"): "send this to
//! a model and stream back actions". The loop reaches every protocol through
//! [`Provider`] and [`ModelCall`], and never names one.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::events::{
    CacheLifetime, ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta,
    ToolCallCompleted, ToolCallRequested,
};
use crate::shapes::{Failure, Tokens};
use crate::{ActionId, GenerationId, ThinkingLevel};

/// One model, reached through its protocol. A provider extension's model
/// becomes one of these.
pub trait Provider: Send + Sync {
    /// Prepares a call. Nothing is sent until [`ModelCall::run`].
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall>;

    /// Each tool in the protocol's own shape, in name order: exactly what
    /// the protocol puts in its request for that tool. The strict-tool
    /// budget is spent in name order, so it depends on the whole set.
    /// The default is each definition as a JSON object, for fakes.
    /// This is the tools Fiber builds.
    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        let mut sorted: Vec<&ToolDefinition> = tools.iter().collect();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        sorted
            .into_iter()
            .filter_map(|tool| serde_json::to_value(tool).ok()?.as_object().cloned())
            .collect()
    }

    /// The provider's generation lookup, when its package declares `cost()`
    /// (`docs/model-routing.md`, "Cost"); `None` for every other provider.
    fn cost_lookup(&self) -> Option<Arc<dyn CostLookup>> {
        None
    }

    /// Whether `request`, resent with its output capped at one token,
    /// differs from it only in that cap (`docs/prompt-cache.md`,
    /// "Warming while idle"). A provider whose request bytes would
    /// change anything else answers `false`, and no refresh is sent.
    fn warms(&self, _request: &ModelRequest) -> bool {
        true
    }
}

/// A provider's lookup of a generation's cost, for a call that ended without
/// the vendor's own figure (`docs/model-routing.md`, "Cost").
pub trait CostLookup: Send + Sync {
    /// One lookup, which blocks up to the provider's declared timeout. `Some`
    /// is a finite cost at or above 0, in US dollars. `None` covers nothing
    /// returned, an error and a timeout.
    fn cost(&self, generation_id: &GenerationId) -> Option<f64>;
}

/// One model call. The loop runs it on its own thread, and any other thread
/// may cancel it while it runs (`docs/architecture.md`, "Cancellation").
pub trait ModelCall: Send + Sync {
    /// Sends the request and reads the reply, passing each fragment to `sink`
    /// as it arrives. Blocks until the reply ends, fails, or is cancelled.
    #[allow(
        clippy::result_large_err,
        reason = "the seam returns the call's error by value; the partial usage is boxed"
    )]
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
    /// The session's one reasoning setting (`docs/model-routing.md`,
    /// "Thinking").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingLevel>,
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
    /// The tools as a rewound session's parent sent them, in that order:
    /// a rewound session's first request sends these verbatim, so it
    /// matches its parent's bytes (`docs/events.md`, "Rewind"). `None`
    /// sends what `tools` wires. Any later build clears it back to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_tools: Option<Vec<Map<String, Value>>>,
    /// The index into `conversation` where the previous request in this
    /// session ended; `None` on the first request. Anthropic puts its second
    /// cache marker there (`docs/prompt-cache.md`, "Cache markers and keys").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_end: Option<usize>,
    /// The request's own output limit; never above the model's
    /// (`docs/errors.md`, "Output tokens").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// The session directory, which an [`ImageRef`]'s path is relative to. A
    /// protocol reads the stored image from here when it builds a request.
    /// Empty when no input in the conversation holds an image.
    #[serde(default)]
    pub session_dir: PathBuf,
}

/// An image a tool result or a person's message carries: the log's `image` part
/// (`docs/events.md`), without the bytes. The file is what every request
/// sends (`docs/model-routing.md`, "Image limits").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    /// The file's path, relative to the session directory.
    pub path: String,
    /// Its type, such as `image/png`.
    pub mime_type: String,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
}

/// A PDF a tool result carries: the log's `pdf` part (`docs/events.md`),
/// without the bytes. The file is what every request sends
/// (`docs/model-routing.md`, "Image limits").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfRef {
    /// The file's path, relative to the session directory.
    pub path: String,
    /// The number of pages sent.
    pub page_count: u32,
    /// The pages rendered as image references, absent when they could not
    /// be rendered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<ImageRef>>,
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
    /// The vendor's hosted tool type, such as `web_search_20250305`, for a
    /// tool the provider runs itself; the protocol sends only this type and
    /// the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted: Option<String>,
}

/// One piece of the conversation, rendered from the log's durable events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    /// A person's message.
    User {
        /// Its text.
        text: String,
        /// Its image parts, in order. A protocol sends each after the text.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageRef>,
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
        /// `model`. A hosted call's two lines render as an empty `text` with
        /// the call block, then one with the result block.
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
        /// The model reference that produced it, `provider/model`. On
        /// `google-generative-ai`, a request to any other model reference
        /// sends it and its result as plain text (`docs/loop.md`, "What the
        /// model is sent").
        model: String,
    },
    /// A tool call's result, from `tool_call_completed`.
    ToolResult {
        /// The call's action.
        action_id: ActionId,
        /// The text the model is sent.
        text: String,
        /// Whether the call's `tool_call_completed` is `failed`. A protocol
        /// sends it as its error flag (`docs/tools.md`, "What a result
        /// carries").
        #[serde(default)]
        is_error: bool,
        /// The result's image parts, in order. A protocol that carries an
        /// image inside a tool result sends each after the text.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageRef>,
        /// The result's PDF parts, in order. A protocol sends each natively
        /// where it accepts a PDF in a tool result, and otherwise as its
        /// pages rendered as images (`docs/tools.md`, "read").
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pdfs: Vec<PdfRef>,
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

/// The input a protocol sent for one call: its request body's size and
/// whether it carried an image (`docs/events.md`, `usage_recorded`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputSize {
    /// The request body's length in bytes, exactly as posted.
    pub bytes: u64,
    /// True when the body carried an image or PDF part.
    pub media: bool,
}

/// A call's generation and what it reported: a reply's, or, for a call that
/// ended without a reply, what it had seen (`docs/events.md`, "Usage and
/// notices").
#[derive(Debug, Clone, PartialEq)]
pub struct CallUsage {
    /// The provider's id for the generation; `None` when the provider named
    /// none. Never `Some` of an empty id.
    pub generation_id: Option<GenerationId>,
    /// The tokens seen; a count not seen is 0.
    pub tokens: Tokens,
    /// Hosted web searches seen; `None` when none seen.
    pub web_searches: Option<u64>,
    /// The input the protocol sent for the call.
    pub input_size: InputSize,
}

impl CallUsage {
    /// What a call that ended before reading any stream saw: no generation,
    /// every count 0, and the input it built.
    pub fn unnamed(input_size: InputSize) -> Self {
        Self {
            generation_id: None,
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            web_searches: None,
            input_size,
        }
    }
}

/// A reply that reached its protocol's terminal event.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// Its text parts, reasoning and tool calls, in the order the model
    /// emitted them.
    pub actions: Vec<ReplyAction>,
    /// Why the reply ended.
    pub finish: Finish,
    /// The provider's id for the generation; `None` when the provider named
    /// none. Never `Some` of an empty id.
    pub generation_id: Option<GenerationId>,
    /// The call's tokens.
    pub tokens: Tokens,
    /// Hosted web searches the reply reports; `None` when it reports none.
    pub web_searches: Option<u64>,
    /// The vendor's own figure for the call, in US dollars, where the
    /// response reports one.
    pub cost: Option<f64>,
    /// The input the protocol sent for the call.
    pub input_size: InputSize,
}

impl Reply {
    /// The reply's generation and what it reported.
    pub fn usage(&self) -> CallUsage {
        CallUsage {
            generation_id: self.generation_id.clone(),
            tokens: self.tokens.clone(),
            web_searches: self.web_searches,
            input_size: self.input_size,
        }
    }

    /// The reply's text: its text parts concatenated, in order, with no
    /// separator.
    pub fn text(&self) -> String {
        self.actions
            .iter()
            .filter_map(|action| match action {
                ReplyAction::Text(part) => Some(part.text.as_str()),
                ReplyAction::Reasoning(_) | ReplyAction::ToolCall(_) | ReplyAction::Hosted(_) => {
                    None
                }
            })
            .collect()
    }
}

/// A durable action in a reply.
#[allow(
    clippy::large_enum_variant,
    reason = "a reply holds a few actions for one step; boxing would allocate on each"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyAction {
    /// `text_completed`.
    Text(TextCompleted),
    /// `reasoning_completed`.
    Reasoning(ReasoningCompleted),
    /// `tool_call_requested`.
    ToolCall(ToolCallRequested),
    /// A call the provider ran itself, such as a hosted web search, with its
    /// result.
    Hosted(HostedCall),
}

/// A call the provider ran itself, such as a hosted web search, with its
/// result; both carry their raw blocks as `provider_item`.
#[derive(Debug, Clone, PartialEq)]
pub struct HostedCall {
    /// The call, as `tool_call_requested`.
    pub call: ToolCallRequested,
    /// The result, as `tool_call_completed`.
    pub completed: ToolCallCompleted,
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

/// A model's prices, in US dollars per million tokens
/// (`docs/configuration.md`, "A provider's data").
// debt: duplicates config's Cost and Tier; fixed by #425
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    /// Input tokens.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Tokens read from the prompt cache. Absent prices those tokens at 0.
    pub cache_read: Option<f64>,
    /// Tokens written to the prompt cache. Absent prices those tokens at 0.
    pub cache_write: Option<f64>,
    /// Higher input sizes, each with its own four prices.
    pub tiers: Vec<Tier>,
}

/// One price tier when cost varies by request size. A tier states all four
/// prices (`docs/model-routing.md`, "Cost").
#[derive(Debug, Clone, PartialEq)]
pub struct Tier {
    /// Input tokens above which this tier's prices apply to the whole call.
    pub input_tokens_above: u64,
    /// Input tokens.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Tokens read from the prompt cache.
    pub cache_read: f64,
    /// Tokens written to the prompt cache.
    pub cache_write: f64,
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
        /// What the call had seen, named or not.
        usage: Box<CallUsage>,
    },
    /// [`ModelCall::cancel`] ended it.
    Cancelled {
        /// What the call had seen, named or not.
        usage: Box<CallUsage>,
    },
}

impl CallError {
    /// What the call had seen, named or not: every call writes its
    /// `usage_recorded` however it ended (`docs/events.md`, "Usage and
    /// notices").
    pub fn usage(&self) -> &CallUsage {
        match self {
            CallError::Failed { usage, .. } => usage,
            CallError::Cancelled { usage } => usage,
        }
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
