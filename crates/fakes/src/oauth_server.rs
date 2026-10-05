//! The fake OAuth token endpoint (`docs/testing.md`, "Fakes"): a local HTTP
//! server over [`ProviderServer`] that answers form-encoded POSTs from one
//! script, in request order, and records each request's path and decoded form
//! fields. It does not route on the path: a flow uses one endpoint at a time,
//! so a refresh test scripts `/token` replies and a device-code test scripts
//! `/device/token` replies.

use serde_json::json;

use crate::provider_server::{ProviderServer, Response};

/// How many `script_exhausted` replies follow a script. Past them the
/// provider server's own 500 answers.
// debt: 256 requests past the script, a test that needs more takes its count
// from the provider server's fallback or this constant is raised.
const PAST_THE_END: usize = 256;

/// One scripted reply of the token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthReply(Response);

impl OauthReply {
    /// A 200 token reply: `access_token`, `refresh_token`, `token_type` and
    /// `expires_in` seconds.
    pub fn token(access: &str, refresh: &str, expires_in: u64) -> Self {
        Self(Response::status(
            200,
            json!({
                "access_token": access,
                "refresh_token": refresh,
                "token_type": "Bearer",
                "expires_in": expires_in,
            })
            .to_string(),
        ))
    }

    /// A device-code reply: the user has not approved yet.
    pub fn pending() -> Self {
        Self::error("authorization_pending")
    }

    /// A device-code reply: poll less often.
    pub fn slow_down() -> Self {
        Self::error("slow_down")
    }

    /// A reply: the user refused.
    pub fn denied() -> Self {
        Self::error("access_denied")
    }

    /// A reply: the device code ran out.
    pub fn expired() -> Self {
        Self::error("expired_token")
    }

    /// Any status and body, sent as they are: a malformed body, a 500.
    pub fn raw(status: u16, body: &str) -> Self {
        Self(Response::status(status, body))
    }

    fn error(code: &str) -> Self {
        Self(Response::status(400, json!({ "error": code }).to_string()))
    }
}

/// One request the endpoint received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthRequest {
    /// The request target, such as `/token`.
    pub path: String,
    /// The form body's fields in order, percent-decoded.
    pub form: Vec<(String, String)>,
}

/// A fake token endpoint listening on a local port. Dropping it closes the
/// port.
pub struct OauthServer {
    inner: ProviderServer,
}

impl OauthServer {
    /// Listens on a free 127.0.0.1 port and serves `replies` in order, one per
    /// request whatever its path. A request past the script gets a 500
    /// `{"error":"script_exhausted"}`.
    #[allow(
        clippy::expect_used,
        reason = "a fake that cannot bind a loopback port has nothing to run; a build aborts on panic"
    )]
    pub fn start(replies: Vec<OauthReply>) -> Self {
        let exhausted = std::iter::repeat_with(|| {
            OauthReply(Response::status(500, r#"{"error":"script_exhausted"}"#))
        })
        .take(PAST_THE_END);
        let script = replies.into_iter().chain(exhausted).map(|reply| reply.0);
        Self {
            inner: ProviderServer::start(script).expect("binding 127.0.0.1"),
        }
    }

    /// The base URL, such as `http://127.0.0.1:49152`.
    pub fn url(&self) -> String {
        self.inner.url()
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<OauthRequest> {
        self.inner
            .requests()
            .into_iter()
            .map(|request| OauthRequest {
                path: request.path,
                form: decode_form(&String::from_utf8_lossy(&request.body)),
            })
            .collect()
    }

    /// How many requests have been received.
    pub fn request_count(&self) -> usize {
        self.inner.requests().len()
    }
}

/// `application/x-www-form-urlencoded` fields, in order: `+` is a space and
/// `%XX` is a byte. An invalid escape stays as written.
fn decode_form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(name), percent_decode(value))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let escaped = (byte == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (byte, escaped) {
            (_, Some(decoded)) => {
                out.push(decoded);
                i += 3;
            }
            (b'+', None) => {
                out.push(b' ');
                i += 1;
            }
            (other, None) => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[path = "oauth_server_tests.rs"]
mod tests;
