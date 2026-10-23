//! The `google-generative-ai` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): the Gemini API, Google Vertex (Gemini) and OpenCode Zen's
//! Gemini models, Gemini 3 and later. A request is a `POST` to
//! `<base_url>/models/<model>:streamGenerateContent?alt=sse`; the reply is a
//! server-sent event stream of `GenerateContentResponse` objects, the last
//! carrying the candidate's `finishReason`
//! (`research/google-generative-ai-probe`).

use contract::diag::PrefixHashes;
use contract::provider::{ModelCall, ModelRequest, Provider, ToolDefinition};
use serde_json::{Map, Value};

pub use crate::call::Call;
#[cfg(any(test, feature = "test-hooks"))]
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
            "text/event-stream",
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
    /// Builds the observed call for `request`: as the plain request, writing
    /// one `provider_request` line when the call was sent. At `info` the
    /// call runs exactly as an unobserved one does, with no model cloned.
    pub fn call_observed(
        &self,
        request: &ModelRequest,
        observation: crate::RequestObservation,
    ) -> Box<dyn ModelCall> {
        let mut call = self.request(request);
        if !observation.debugging() {
            return Box::new(call);
        }
        // The digests cost a second encoding, so they run only with the
        // setting on; the hot path with it off is unchanged.
        if observation.cache_diagnostics() {
            call = call.with_hashes(hashes(&self.endpoint, request));
        }
        Box::new(call.observed(observation.with_model(self.endpoint.model.clone())))
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

/// The redacted prefix digests of the encoded segments: the
/// `systemInstruction`, the `tools` array and each cumulative `contents`
/// prefix as sent. Either segment is absent when the request sends none.
fn hashes(endpoint: &Endpoint, request: &ModelRequest) -> PrefixHashes {
    let (system, tools, contents) =
        crate::google_generative_ai_request::body_segments(endpoint, request);
    PrefixHashes {
        system: system.map(|system| crate::cache_diagnostics::segment(&system)),
        tools: tools.map(|tools| crate::cache_diagnostics::segment(&tools)),
        messages: crate::cache_diagnostics::cumulative(&contents),
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
            error_type,
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
                error_type,
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
