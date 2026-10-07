//! Talks to model APIs: each protocol's wire format, sending a request and
//! streaming its reply into actions (`docs/model-routing.md`). Reached only
//! through the provider seam in `contract` (`docs/architecture.md`,
//! "Provider seam").
//!
//! A protocol is native Rust here; a provider is data an extension declares,
//! which arrives as an [`Endpoint`].

pub mod anthropic_messages;
mod anthropic_messages_decode;
mod error;
pub mod google_generative_ai;
mod google_generative_ai_decode;
mod google_generative_ai_request;
mod http;
mod images;
pub mod openai_completions;
mod openai_completions_messages;
mod openai_completions_tools;
pub mod openai_responses;
pub mod redact;
mod sse;
mod strict;

pub use error::Error;

use std::sync::Arc;

use contract::Secret;
use contract::signing::Signer;
use serde_json::{Map, Value};

/// One model of one provider, as its provider data declares it
/// (`docs/model-routing.md`, "What a provider extension declares").
#[derive(Clone, Default)]
pub struct Endpoint {
    /// The provider's name, the first half of the model reference.
    pub provider: String,
    /// The model's id, as the vendor spells it.
    pub model: String,
    /// The model's base URL. The protocol appends its own path, such as
    /// `/responses`.
    pub base_url: String,
    /// The key, sent as a bearer token; `None` sends no `Authorization`.
    pub key: Option<Secret>,
    /// Headers sent on every request, in order.
    pub headers: Vec<(String, String)>,
    /// Signs each request just before it is sent, a retry included
    /// (`docs/model-routing.md`, "Signing a request"); `None` sends the
    /// request as built.
    pub signer: Option<Arc<dyn Signer>>,
    /// The compatibility flags the protocol reads.
    pub compat: Compat,
    /// The model's output token limit, `max_output_tokens` in its data. A
    /// request's own limit never exceeds it (`docs/errors.md`, "Output
    /// tokens"); `None` when the data declares none.
    pub max_output_tokens: Option<u64>,
    /// Extra request body fields, added last.
    pub extra_body: Map<String, Value>,
    /// Connect without a proxy even when the environment names one. False
    /// by default, so production behaviour is unchanged; tests set it to
    /// reach a local server directly whatever the shell names
    /// (`docs/dependencies.md`, "Proxies").
    pub direct: bool,
    /// True when the model's declared `input` lacks `image`
    /// (`docs/model-routing.md`, "What a provider extension declares").
    /// A request for such a model carries no image part on any protocol;
    /// each left-out image gets one text line saying so (`docs/tools.md`,
    /// "read"). False by default, so images are sent unchanged.
    pub text_only: bool,
}

impl Endpoint {
    /// The model reference, `provider/model`.
    pub fn reference(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// The output limit to send: the smaller of the request's limit and the
    /// model's when both are set (`docs/errors.md`, "Output tokens").
    pub fn output_limit(&self, request: Option<u64>) -> Option<u64> {
        request.into_iter().chain(self.max_output_tokens).min()
    }
}

// `Arc<dyn Signer>` has no `Debug`: a debug print names the field without
// reaching into it. A header value can hold a key, so only names print
// (`docs/code-quality.md`, "Errors").
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("key", &self.key)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| (name, "redacted"))
                    .collect::<Vec<_>>(),
            )
            .field("signer", &self.signer.as_ref().map(|_| "Signer"))
            .field("compat", &self.compat)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("extra_body", &self.extra_body)
            .field("direct", &self.direct)
            .field("text_only", &self.text_only)
            .finish()
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
    /// `openai-completions`: the level goes in `reasoning: {effort}`, as
    /// OpenRouter takes it, not in `reasoning_effort`.
    pub reasoning_object: bool,
    /// `openai-completions`: the model is Anthropic's, reached through a
    /// gateway such as OpenRouter. It takes Anthropic's `cache_control`
    /// markers on content parts, which OpenRouter passes through
    /// (`research/openai-completions-probe`), and Anthropic's strict-tool
    /// limits apply.
    pub anthropic: bool,
    /// `openai-completions`: the body field that also carries the cache key,
    /// such as OpenRouter's `session_id` (`docs/prompt-cache.md`, "Cache
    /// markers and keys").
    pub cache_key_field: Option<String>,
}

impl Compat {
    /// The flags a model's `compat` object declares. A flag that is absent,
    /// or not a boolean, is not set.
    pub fn from_data(data: &Map<String, Value>) -> Self {
        let flag = |name: &str| data.get(name).and_then(Value::as_bool);
        Self {
            store: flag("store"),
            max_tokens: flag("max_tokens").unwrap_or(false),
            reasoning_object: flag("reasoning_object").unwrap_or(false),
            anthropic: flag("anthropic").unwrap_or(false),
            cache_key_field: data
                .get("cache_key_field")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }
}

#[cfg(test)]
#[path = "endpoint_tests.rs"]
mod endpoint_tests;
