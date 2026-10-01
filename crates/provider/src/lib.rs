//! Talks to model APIs: each protocol's wire format, sending a request and
//! streaming its reply into actions (`docs/model-routing.md`). Reached only
//! through the provider seam in `contract` (`docs/architecture.md`,
//! "Provider seam").
//!
//! A protocol is native Rust here; a provider is data an extension declares,
//! which arrives as an [`Endpoint`].

pub mod anthropic_messages;
mod error;
mod http;
pub mod openai_completions;
pub mod openai_responses;
mod sse;
mod strict;

pub use error::Error;

use serde_json::{Map, Value};

/// One model of one provider, as its provider data declares it
/// (`docs/model-routing.md`, "What a provider extension declares").
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Endpoint {
    /// The provider's name, the first half of the model reference.
    pub provider: String,
    /// The model's id, as the vendor spells it.
    pub model: String,
    /// The model's base URL. The protocol appends its own path, such as
    /// `/responses`.
    pub base_url: String,
    /// The key, sent as a bearer token; `None` sends no `Authorization`.
    pub key: Option<String>,
    /// Headers sent on every request, in order.
    pub headers: Vec<(String, String)>,
    /// The compatibility flags the protocol reads.
    pub compat: Compat,
    /// The model's output token limit, `max_output_tokens` in its data. A
    /// request's own limit never exceeds it (`docs/errors.md`, "Output
    /// tokens"); `None` when the data declares none.
    pub max_output_tokens: Option<u64>,
    /// Extra request body fields, added last.
    pub extra_body: Map<String, Value>,
}

impl Endpoint {
    /// The model reference, `provider/model`.
    pub fn reference(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// Compatibility flags (`docs/configuration.md`, "A provider's data"). A
/// flag the model's data does not declare is not set: Fiber never guesses
/// one from a URL or a provider name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Compat {
    /// `store`, sent when declared; absent, the request has no `store` key.
    pub store: Option<bool>,
    /// `openai-completions`: the output limit goes in `max_tokens`, not
    /// `max_completion_tokens`. OpenAI's `gpt-6-luna` rejects `max_tokens`
    /// (`docs/model-routing.md`, "openai-completions facts").
    pub max_tokens: bool,
    /// `openai-completions`: the effort goes in `reasoning: {effort}`, as
    /// OpenRouter takes it, not in `reasoning_effort`.
    pub reasoning_object: bool,
    /// `openai-completions`: the model takes Anthropic's `cache_control`
    /// markers on content parts, as OpenRouter passes them to Anthropic.
    pub cache_control: bool,
}
