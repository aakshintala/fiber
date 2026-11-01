//! The `bedrock-converse` protocol (`docs/model-routing.md`, "Protocols and
//! providers"): AWS Bedrock's models other than Claude, through its Converse
//! API. A request is a `POST` to `<base_url>/model/<id>/converse-stream`; the
//! reply is AWS event-stream framing whose events end with `metadata`, after
//! `messageStop` (`docs/model-routing.md`, "Bedrock Converse wire facts").

use contract::provider::{ModelCall, ModelRequest, Provider, ToolDefinition};
use serde_json::{Map, Value};

use crate::Endpoint;
#[cfg(any(test, debug_assertions))]
pub use crate::bedrock_converse_decode::decode;
use crate::bedrock_converse_decode::decode_tracked;
use crate::bedrock_converse_request::{body, hashes, sends_output_limit, wire_tools};
pub use crate::call::Call;

/// The media type a Converse reply streams in.
const EVENT_STREAM: &str = "application/vnd.amazon.eventstream";

/// One model reached over `bedrock-converse`.
#[derive(Debug, Clone)]
pub struct Converse {
    endpoint: Endpoint,
}

impl Converse {
    /// The protocol for one model of one provider.
    pub fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }

    /// Builds the call for `request`. Two calls built from the same inputs
    /// send the same bytes (`docs/prompt-cache.md`, "Bytes").
    #[allow(
        clippy::result_large_err,
        reason = "the decode carries its partial alongside the error for the call's usage"
    )]
    pub fn request(&self, request: &ModelRequest) -> Call {
        let endpoint = &self.endpoint;
        // A Bedrock API key is a bearer token; `sign()` replaces it with
        // SigV4 for AWS credentials (`docs/model-routing.md`, "Protocols and
        // providers").
        let auth = endpoint
            .key
            .as_ref()
            .map(|key| ("authorization", format!("Bearer {}", key.expose())));
        let headers =
            crate::call::base_headers(endpoint, EVENT_STREAM, &[], auth, None, &request.cache_key);
        let url = format!(
            "{}/model/{}/converse-stream",
            endpoint.base_url.trim_end_matches('/'),
            crate::wire::path_segment(&endpoint.model)
        );
        Call::new(
            endpoint,
            request,
            url,
            headers,
            body(endpoint, request),
            |stream, _, sink| decode_tracked(stream, sink),
        )
        // No event names the generation: AWS's id for the request does
        // (`docs/model-routing.md`, "Bedrock Converse wire facts").
        .named_by_request_id()
    }

    /// Builds the observed call for `request`: as the plain request, writing
    /// one `http_request` line when the call was sent. At `info` the
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

impl Provider for Converse {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(self.request(request))
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        wire_tools(tools)
    }

    fn warms(&self, request: &ModelRequest) -> bool {
        // A request that may not carry `maxTokens` cannot be capped at one
        // token (`docs/prompt-cache.md`, "Warming while idle").
        sends_output_limit(&self.endpoint, request)
    }
}
