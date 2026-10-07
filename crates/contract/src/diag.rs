//! The debug level's seam (`docs/state.md`, "Diagnostic logs"): what one
//! `provider_request` line records, and the sink a caller hands it to. Like
//! [`crate::clock::Clock`], it defines the seam and contains no behaviour;
//! `log::diag::Diag` writes the lines.

use serde::Serialize;

/// Why Fiber made a provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// A model call: a turn's, the reviewer's or a cache-warming refresh.
    ModelCall,
    /// A provider's model-list refresh.
    ModelList,
    /// A quota read.
    Quota,
    /// An OAuth token refresh.
    TokenRefresh,
    /// A provider's `cost()` lookup.
    Cost,
}

/// The `data` of one `provider_request` line. The fields serialize in the
/// order declared here, and an absent option is left out, never `null`.
/// Nothing here may hold a credential, header value, query string, prompt or
/// model text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderRequest {
    /// The provider's name.
    pub provider: String,
    /// The model, on a model call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Why the request was made.
    pub purpose: Purpose,
    /// The URL's authority, without userinfo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The URL's path, with no query string or fragment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The HTTP status, when a response came.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Which attempt this was, from 1.
    pub attempt: u32,
    /// The request body's length.
    pub request_bytes: u64,
    /// The response body bytes read; 0 when no response came.
    pub response_bytes: u64,
    /// Milliseconds from the send to the response headers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers_ms: Option<u64>,
    /// Milliseconds from the send to the first streamed token, on a model call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    /// Milliseconds from the send until the call returned.
    pub total_ms: u64,
}

/// Where debug lines go. `provider_request` writes nothing when
/// [`DebugLog::debugging`] is false, so a caller may skip building the value.
pub trait DebugLog: Send + Sync {
    /// Whether debug lines are written.
    fn debugging(&self) -> bool;

    /// Writes one `provider_request` line, when debugging.
    fn provider_request(&self, request: &ProviderRequest);
}

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;
