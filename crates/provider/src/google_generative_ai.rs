//! The `google-generative-ai` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): the Gemini API, Google Vertex (Gemini) and OpenCode Zen's
//! Gemini models, Gemini 3 and later. A request is a `POST` to
//! `<base_url>/models/<model>:streamGenerateContent?alt=sse`; the reply is a
//! server-sent event stream of `GenerateContentResponse` objects, the last
//! carrying the candidate's `finishReason`
//! (`research/google-generative-ai-probe`).

use contract::provider::{ModelCall, ModelRequest, Provider, ToolDefinition};
use serde_json::{Map, Value};

pub use crate::call::Call;
pub use crate::google_generative_ai_decode::decode;
use crate::google_generative_ai_decode::decode_tracked;

use crate::google_generative_ai_request::{body, wire_tools};
use crate::wire::str_at;
use crate::{Endpoint, Error};

/// One model reached over `google-generative-ai`.
#[derive(Debug, Clone)]
pub struct Gemini {
    endpoint: Endpoint,
    cache_key_header: Option<String>,
}

impl Gemini {
    /// The protocol for one model of one provider.
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            cache_key_header: None,
        }
    }

    /// Also sends each request's cache key in the header `name`, for a
    /// provider that routes by it, such as OpenCode's `x-opencode-session`
    /// (`docs/prompt-cache.md`, "Cache markers and keys").
    #[must_use]
    pub fn cache_key_header(mut self, name: impl Into<String>) -> Self {
        self.cache_key_header = Some(name.into());
        self
    }

    /// Builds the call for `request`. Two calls built from the same inputs
    /// send the same bytes (`docs/prompt-cache.md`, "Bytes").
    #[allow(
        clippy::result_large_err,
        reason = "the decode carries its partial alongside the error for the call's usage"
    )]
    pub fn request(&self, request: &ModelRequest) -> Call {
        let endpoint = &self.endpoint;
        // The header and `?key=` both authenticate
        // (`docs/model-routing.md`, "Google Generative AI wire facts"); the
        // header keeps the key out of the URL.
        let headers = crate::call::base_headers(
            endpoint,
            &[],
            endpoint
                .key
                .as_ref()
                .map(|key| ("x-goog-api-key", key.expose().to_owned())),
            self.cache_key_header.as_deref(),
            &request.cache_key,
        );
        let body = body(endpoint, request);
        Call::new(
            endpoint,
            request,
            format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                endpoint.base_url.trim_end_matches('/'),
                endpoint.model
            ),
            headers,
            body,
            |stream, _, sink| decode_tracked(stream, sink),
        )
        .on_error(retry_info)
    }
}

impl Provider for Gemini {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        wire_tools(tools)
    }
}

/// The wait a failed response asks for: `Retry-After`, or else the error
/// body's `RetryInfo.retryDelay`, a protobuf duration such as `"37s"`
/// (`docs/model-routing.md`, "When a model call fails"; the shape is
/// `google.rpc.RetryInfo` in googleapis' `google/rpc/error_details.proto`).
fn retry_info(error: Error) -> Error {
    match error {
        Error::Status {
            status,
            body,
            retry_after: None,
            should_retry,
            url,
        } => {
            let retry_after = serde_json::from_str::<Value>(&body).ok().and_then(|v| {
                v.pointer("/error/details")?
                    .as_array()?
                    .iter()
                    .find(|d| str_at(d, "@type") == "type.googleapis.com/google.rpc.RetryInfo")?
                    .get("retryDelay")?
                    .as_str()?
                    .strip_suffix('s')?
                    .parse::<f64>()
                    .ok()
            });
            Error::Status {
                status,
                body,
                retry_after,
                should_retry,
                url,
            }
        }
        other @ (Error::Status { .. }
        | Error::Connection(_)
        | Error::Stalled { .. }
        | Error::StreamIncomplete(_)
        | Error::ReplyFailed { .. }
        | Error::UnknownStopReason(_)
        | Error::ContextOverflow(_)
        | Error::QuotaExceeded(_)
        | Error::Refused(_)
        | Error::Sign(_)) => other,
    }
}
