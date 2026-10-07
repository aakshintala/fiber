//! The `google-generative-ai` protocol (`docs/model-routing.md`, "Protocols
//! and providers"): the Gemini API, Google Vertex (Gemini) and OpenCode Zen's
//! Gemini models, Gemini 3 and later. A request is a `POST` to
//! `<base_url>/models/<model>:streamGenerateContent?alt=sse`; the reply is a
//! server-sent event stream of `GenerateContentResponse` objects, the last
//! carrying the candidate's `finishReason`
//! (`research/google-generative-ai-probe`).

use std::io::BufReader;
use std::sync::Arc;

use contract::provider::{
    CallError, Delta, ModelCall, ModelRequest, Provider, Reply, ToolDefinition,
};
use serde_json::{Map, Value};

pub use crate::google_generative_ai_decode::decode;
use crate::http::{self, Cancel};
use crate::redact::Secrets;

use crate::google_generative_ai_request::{body, wire_tools};
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
    pub fn request(&self, request: &ModelRequest) -> Call {
        let endpoint = &self.endpoint;
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("accept".to_owned(), "text/event-stream".to_owned()),
            (
                "user-agent".to_owned(),
                concat!("fiber/", env!("CARGO_PKG_VERSION")).to_owned(),
            ),
        ];
        // The header and `?key=` both authenticate
        // (`docs/model-routing.md`, "Google Generative AI wire facts"); the
        // header keeps the key out of the URL.
        if let Some(key) = &endpoint.key {
            headers.push(("x-goog-api-key".to_owned(), key.expose().to_owned()));
        }
        headers.extend(endpoint.headers.iter().cloned());
        if let Some(name) = &self.cache_key_header {
            headers.push((name.clone(), request.cache_key.clone()));
        }
        Call {
            url: format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                endpoint.base_url.trim_end_matches('/'),
                endpoint.model
            ),
            headers,
            body: body(endpoint, request),
            provider: endpoint.provider.clone(),
            signer: endpoint.signer.clone(),
            direct: endpoint.direct,
            cancel: Arc::default(),
            secrets: endpoint.secrets(),
        }
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

/// One `google-generative-ai` call, ready to send.
pub struct Call {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    provider: String,
    signer: Option<Arc<dyn contract::signing::Signer>>,
    direct: bool,
    cancel: Arc<Cancel>,
    secrets: Secrets,
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let mut secrets = self.secrets.clone();
        let (reply, should_retry) = match http::post_signed(
            &self.url,
            &self.headers,
            &self.body,
            self.signer.as_deref(),
            self.direct,
            &self.cancel,
            &mut secrets,
        ) {
            Ok((stream, should_retry)) => (decode(BufReader::new(stream), sink), should_retry),
            Err(e) => {
                let should_retry = e.should_retry();
                (Err(retry_info(e)), should_retry)
            }
        };
        // Whatever a cancelled call returns, the cancel ended it.
        if self.cancel.is_cancelled() {
            return Err(CallError::Cancelled);
        }
        reply.map_err(|e| CallError::Failed {
            failure: e.failure(&self.provider, &secrets),
            should_retry,
        })
    }

    fn cancel(&self) {
        self.cancel.cancel();
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
            }
        }
        other @ (Error::Status { .. }
        | Error::Connection(_)
        | Error::StreamIncomplete(_)
        | Error::ReplyFailed { .. }
        | Error::UnknownStopReason(_)
        | Error::ContextOverflow(_)
        | Error::QuotaExceeded(_)
        | Error::Refused(_)
        | Error::Sign(_)) => other,
    }
}

/// The string at `key`, or `""`.
pub(crate) fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
