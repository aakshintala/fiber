//! The fake OAuth token endpoint (`docs/testing.md`, "Fakes"): a local HTTP
//! server over [`ProviderServer`] that answers form-encoded POSTs from one
//! script, in request order, and records each request's path and decoded form
//! fields. It does not route on the path: a flow uses one endpoint at a time,
//! so a refresh test scripts `/token` replies and a device-code test scripts
//! `/device/token` replies.

use std::time::Duration;

use serde_json::json;

use crate::provider_server::{ProviderServer, Response};

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
    /// The raw request body, for requests that are not form-encoded.
    pub body: String,
    /// The form body's fields in order, percent-decoded.
    pub form: Vec<(String, String)>,
}

/// Builds an unsigned test JWT carrying `claims`: an `{"alg":"none"}`
/// header and the claims as base64url without padding, with an empty
/// signature (`docs/testing.md`, "Fakes"). Hand-written so the fake takes
/// no encoding dependency.
pub fn jwt(claims: &serde_json::Value) -> String {
    format!(
        "{}.{}.",
        base64url(br#"{"alg":"none"}"#),
        base64url(claims.to_string().as_bytes())
    )
}

/// Base64url without padding.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut bits: u32 = 0;
        for byte in chunk {
            // The new byte occupies bits disjoint from the shifted prefix.
            bits = (bits << 8) + u32::from(*byte);
        }
        bits <<= 8 * (3 - chunk.len());
        for i in 0..chunk.len() + 1 {
            // Six bits name one of the 64 letters.
            let at = ((bits >> (18 - 6 * i)) & 0x3f) as usize;
            out.push(ALPHABET.get(at).copied().unwrap_or(b'?') as char);
        }
    }
    out
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
        let script = replies.into_iter().map(|reply| reply.0);
        let exhausted = Response::status(500, r#"{"error":"script_exhausted"}"#);
        Self {
            inner: ProviderServer::start_with_fallback(script, exhausted)
                .expect("binding 127.0.0.1"),
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
                body: String::from_utf8_lossy(&request.body).into_owned(),
                form: decode_form(&String::from_utf8_lossy(&request.body)),
            })
            .collect()
    }

    /// How many requests have been received.
    pub fn request_count(&self) -> usize {
        self.inner.requests().len()
    }

    /// Waits, at most `within` of real time, until at least `count` requests
    /// are recorded. True once they are; false at the deadline.
    pub fn await_requests(&self, count: usize, within: Duration) -> bool {
        self.inner.await_requests(count, within)
    }

    /// Holds every reply until [`OauthServer::release`]. A request is still
    /// recorded first, so [`OauthServer::await_requests`] sees it while the
    /// client waits. Dropping the server releases what it holds.
    pub fn hold(&self) {
        self.inner.hold();
    }

    /// Sends the replies [`OauthServer::hold`] is holding.
    pub fn release(&self) {
        self.inner.release();
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
    let mut bytes = text.bytes();
    let mut out = Vec::with_capacity(text.len());
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                // Two hex digits make a byte; anything else leaves the `%`
                // as written and the next bytes to be read as themselves.
                let mut ahead = bytes.clone();
                match (ahead.next().and_then(hex), ahead.next().and_then(hex)) {
                    (Some(high), Some(low)) => {
                        out.push(high * 16 + low);
                        bytes = ahead;
                    }
                    _ => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

#[cfg(test)]
#[path = "oauth_server_tests.rs"]
mod tests;
