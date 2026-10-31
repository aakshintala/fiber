//! `fiber mcp login`'s authorization flow (`docs/mcp.md`, "What Fiber does
//! with MCP"): discovery through `discovery`, dynamic client registration,
//! a PKCE authorization code on a loopback redirect through `callback`,
//! and the token exchange, stored by canonical URL. Method follows MCP's
//! authorization spec: protected-resource metadata (RFC 9728),
//! authorization-server metadata (RFC 8414), client registration
//! (RFC 7591, public client), PKCE `S256` (RFC 7636), and the RFC 8707
//! `resource` parameter.

mod callback;
mod discovery;

use callback::wait_callback;
use discovery::discover;

use std::io::Read;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use contract::clock::Clock;
use contract::diag::{DebugLog, HttpRequest};
use contract::mcp_tokens::TokenFiles;
use contract::{ErrorCode, Secret};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use ureq::unversioned::resolver::DefaultResolver;

use crate::token::{Token, key, store_token, url_has_userinfo};

/// How long the login waits for the browser callback, on the injected
/// clock: 15 minutes, the precedent `credential`'s callback waits.
pub const CALLBACK_WAIT: Duration = Duration::from_secs(900);

/// What one login request reports to: the sink the line goes to and the
/// clock its stamps are read on. `None` debug stays silent: stamps are
/// skipped and no line is written.
pub(crate) struct Observe<'a> {
    pub(crate) debug: Option<Arc<dyn DebugLog>>,
    pub(crate) clock: &'a dyn Clock,
}

impl Observe<'_> {
    /// The send stamp, `None` when silent so no clock is read at info.
    fn stamp(&self) -> Option<Instant> {
        self.debug.as_ref().map(|_| self.clock.now())
    }

    /// Writes one `mcp` line for a request that reached the client: no
    /// identity, host, path, URL, server name or credential, attempt 1,
    /// the posted byte length, the consumed response bytes, the status
    /// when a response came, and the send/headers/end stamps. A sink
    /// that stopped debugging writes nothing.
    fn line(
        &self,
        start: Option<Instant>,
        headers: Option<Instant>,
        status: Option<u16>,
        request_bytes: u64,
        response_bytes: u64,
    ) {
        let (Some(debug), Some(start)) = (self.debug.as_ref(), start) else {
            return;
        };
        if !debug.debugging() {
            return;
        }
        let end = self.clock.now();
        let millis = |instant: Instant| {
            u64::try_from(instant.saturating_duration_since(start).as_millis()).unwrap_or(u64::MAX)
        };
        let mut line = HttpRequest::for_mcp();
        line.status = status;
        line.headers_ms = headers.map(millis);
        line.total_ms = millis(end);
        line.request_bytes = request_bytes;
        line.response_bytes = response_bytes;
        debug.http_request(&line);
    }
}

/// The form body `send_form` would send, built here so its length is the
/// posted byte count: keys and values percent-encoded per the URL spec's
/// query set with space as `+`, pairs joined by `&` and `=`. The bytes
/// sent are identical to `send_form`'s, so the content type stays
/// `application/x-www-form-urlencoded`.
pub(crate) fn form_body(form: &[(&str, &str)]) -> String {
    fn encoded(out: &mut String, value: &str) {
        for byte in value.bytes() {
            if matches!(
                byte,
                b'A'..=b'Z'
                    | b'a'..=b'z'
                    | b'0'..=b'9'
                    | b'!'
                    | b'('
                    | b')'
                    | b'*'
                    | b'-'
                    | b'.'
                    | b'_'
                    | b'~'
            ) {
                out.push(byte as char);
            } else if byte == b' ' {
                out.push('+');
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    let mut body = String::new();
    for (key, value) in form {
        if !body.is_empty() {
            body.push('&');
        }
        encoded(&mut body, key);
        body.push('=');
        encoded(&mut body, value);
    }
    body
}

/// The longest metadata or token body the login reads: past it the step
/// fails, and nothing is stored. Shared with `discovery`, which caps its
/// documents the same way.
pub(crate) const MAX_BODY: usize = 1024 * 1024;

/// A login could not be acquired. The string-bearing variants hold a step
/// name only, never a token, code, verifier, state, response body or
/// description; `Debug` prints the name only, never the string
/// (`docs/code-quality.md`, "Errors").
pub enum LoginError {
    /// The URL is not an HTTP URL.
    Url,
    /// The URL carries a user name or password.
    Userinfo,
    /// The server or its authorization server could not be reached.
    Unreachable(String),
    /// The platform gave no random bytes for the PKCE verifier and state.
    Random,
    /// Discovery failed at the named step.
    Discovery(String),
    /// Client registration failed at the named step.
    Registration(String),
    /// The token exchange failed at the named step.
    Exchange(String),
    /// The browser callback failed at the named step.
    Callback(String),
    /// The authorization server answered the callback with an error. The
    /// string is the error code when it is one of RFC 6749's, else empty,
    /// so `Display` never echoes a description or a `state`.
    Denied(String),
    /// The authorization server's metadata does not list PKCE `S256`.
    PkceUnsupported,
    /// No callback arrived within 15 minutes.
    Timeout,
    /// The token could not be stored.
    Store(crate::token::TokenError),
}

impl LoginError {
    /// The stable code for a login failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Url | Self::Userinfo | Self::PkceUnsupported => ErrorCode::ConfigInvalid,
            Self::Unreachable(_) => ErrorCode::ConnectionFailed,
            Self::Random
            | Self::Discovery(_)
            | Self::Registration(_)
            | Self::Exchange(_)
            | Self::Callback(_)
            | Self::Denied(_)
            | Self::Timeout => ErrorCode::AuthenticationFailed,
            Self::Store(error) => error.code(),
        }
    }
}

impl std::fmt::Debug for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url => f.write_str("Url"),
            Self::Userinfo => f.write_str("Userinfo"),
            Self::Unreachable(_) => f.write_str("Unreachable"),
            Self::Random => f.write_str("Random"),
            Self::Discovery(_) => f.write_str("Discovery"),
            Self::Registration(_) => f.write_str("Registration"),
            Self::Exchange(_) => f.write_str("Exchange"),
            Self::Callback(_) => f.write_str("Callback"),
            Self::Denied(_) => f.write_str("Denied"),
            Self::PkceUnsupported => f.write_str("PkceUnsupported"),
            Self::Timeout => f.write_str("Timeout"),
            Self::Store(error) => f.debug_tuple("Store").field(error).finish(),
        }
    }
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url => f.write_str("The server's URL is not an HTTP URL."),
            Self::Userinfo => {
                f.write_str("The server's URL carries a user name or password; remove it.")
            }
            Self::Unreachable(_) => {
                f.write_str("Fiber could not reach the server or its authorization server.")
            }
            Self::Random => f.write_str("Fiber could not generate the login's PKCE secrets."),
            Self::Discovery(step) => write!(
                f,
                "Fiber could not discover the server's authorization flow at {step}."
            ),
            Self::Registration(step) => write!(
                f,
                "Fiber could not register with the authorization server at {step}."
            ),
            Self::Exchange(step) => {
                write!(
                    f,
                    "Fiber could not exchange the authorization code at {step}."
                )
            }
            Self::Callback(step) => {
                write!(f, "Fiber could not read the browser callback at {step}.")
            }
            Self::Denied(code) if is_known_error(code) => {
                write!(f, "The authorization server returned an error: {code}.")
            }
            Self::Denied(_) => f.write_str("The authorization server returned an error."),
            Self::PkceUnsupported => {
                f.write_str("its authorization server does not offer PKCE with S256")
            }
            Self::Timeout => f.write_str("Fiber got no callback within 15 minutes."),
            Self::Store(error) => write!(f, "Fiber could not store the login: {error}."),
        }
    }
}

impl std::error::Error for LoginError {}

/// The seven error codes of RFC 6749 section 4.1.2.1, in underscore
/// spelling: the only callback `error` values `Display` repeats. Any other
/// value prints the generic sentence, never the value. Shared with
/// `callback`, which reads the error out of the request.
pub(crate) fn is_known_error(code: &str) -> bool {
    matches!(
        code,
        "invalid_request"
            | "unauthorized_client"
            | "access_denied"
            | "unsupported_response_type"
            | "invalid_scope"
            | "server_error"
            | "temporarily_unavailable"
    )
}

/// One agent per login step, over the shared connector.
fn agent() -> ureq::Agent {
    net::agent(
        net::config()
            .proxy(ureq::Proxy::try_from_env())
            .http_status_as_error(false)
            .max_redirects(0)
            .build(),
        Arc::new(()),
        DefaultResolver::default(),
        net::LIMITS,
    )
}

/// A `GET`'s status, its `WWW-Authenticate` header when present, and its
/// body capped at 1 MiB plus one byte. Discovery reads through it. When
/// observed, the read writes one `mcp` line: without status, headers
/// time or body bytes when no head came back, with the status, the
/// headers time and the body bytes read however the read ended once a
/// head arrived.
pub(crate) fn get(
    url: &str,
    observe: &Observe,
) -> Result<(u16, Option<String>, Vec<u8>), ureq::Error> {
    let start = observe.stamp();
    let response = match agent().get(url).call() {
        Ok(response) => response,
        Err(error) => {
            observe.line(start, None, None, 0, 0);
            return Err(error);
        }
    };
    let headers = observe.stamp();
    let mut response = response;
    let status = response.status().as_u16();
    let authenticate = response
        .headers()
        .get("www-authenticate")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    let read = response
        .body_mut()
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut body);
    observe.line(
        start,
        headers,
        Some(status),
        0,
        u64::try_from(body.len()).unwrap_or(u64::MAX),
    );
    read.map_err(|_| ureq::Error::Timeout(ureq::Timeout::Global))?;
    Ok((status, authenticate, body))
}

/// Sends a login `POST`, reads at most 1 MiB plus one byte, and records
/// the sent and consumed body lengths on the request's `mcp` line.
fn post(
    url: &str,
    content_type: &str,
    body: &str,
    observe: &Observe,
) -> Result<(u16, Vec<u8>), ureq::Error> {
    let request_bytes = u64::try_from(body.len()).unwrap_or(u64::MAX);
    let start = observe.stamp();
    let response = match agent()
        .post(url)
        .header("content-type", content_type)
        .send(body)
    {
        Ok(response) => response,
        Err(error) => {
            observe.line(start, None, None, request_bytes, 0);
            return Err(error);
        }
    };
    let headers = observe.stamp();
    let mut response = response;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    let read = response
        .body_mut()
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut bytes);
    observe.line(
        start,
        headers,
        Some(status),
        request_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    read.map_err(|_| ureq::Error::Timeout(ureq::Timeout::Global))?;
    Ok((status, bytes))
}

/// A JSON `POST`'s status and body, using the common login request pipeline.
fn post_json(url: &str, body: &str, observe: &Observe) -> Result<(u16, Vec<u8>), ureq::Error> {
    post(url, "application/json", body, observe)
}

/// A form `POST`'s status and body; the encoded body length is counted.
fn post_form(
    url: &str,
    form: &[(&str, &str)],
    observe: &Observe,
) -> Result<(u16, Vec<u8>), ureq::Error> {
    post(
        url,
        "application/x-www-form-urlencoded",
        &form_body(form),
        observe,
    )
}

/// The origin of a canonical URL: its scheme and authority, the fallback
/// authorization server when there is no protected-resource metadata.
/// Shared with `discovery`, which inserts a path-bearing issuer's
/// well-known document between the origin and the path.
pub(crate) fn origin_of(canonical: &str) -> Option<String> {
    let (scheme, rest) = canonical.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(format!("{scheme}://{}", rest.get(..end)?))
}

/// Registers the public client: a 2xx with a JSON `client_id` answers it,
/// and anything else is terminal, never retried.
fn register(
    registration_endpoint: &str,
    redirect_uri: &str,
    observe: &Observe,
) -> Result<String, LoginError> {
    const STEP: &str = "client registration";
    let body = serde_json::json!({
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "client_name": "Fiber",
    })
    .to_string();
    let (status, bytes) = post_json(registration_endpoint, &body, observe)
        .map_err(|_| LoginError::Registration(STEP.to_owned()))?;
    if !(200..300).contains(&status) || bytes.len() > MAX_BODY {
        return Err(LoginError::Registration(STEP.to_owned()));
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|reply| reply.get("client_id")?.as_str().map(str::to_owned))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| LoginError::Registration(STEP.to_owned()))
}

/// 32 random bytes as base64url without padding, 43 characters (RFC 7636,
/// section 4.1): a verifier, and a `state` of the same shape. Fails
/// closed: a platform that gives no random bytes fails the login, and
/// never falls back to a guessable stand-in.
fn random_43(
    fill: &dyn Fn(&mut [u8]) -> Result<(), ring::error::Unspecified>,
) -> Result<String, LoginError> {
    let mut bytes = [0_u8; 32];
    fill(&mut bytes).map_err(|_| LoginError::Random)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// The S256 challenge of `verifier` (RFC 7636, section 4.2).
fn challenge_of(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

/// Percent-encodes a query value: the unreserved set stays, everything
/// else is `%XX` uppercase.
fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn authorization_url(
    endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    resource: &str,
) -> String {
    // An endpoint that already carries a query keeps it: the parameters
    // join it with the separator its shape needs.
    let separator = if endpoint.contains('?') {
        if endpoint.ends_with('?') || endpoint.ends_with('&') {
            ""
        } else {
            "&"
        }
    } else {
        "?"
    };
    format!(
        "{endpoint}{separator}response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}&resource={}",
        encode(client_id),
        encode(redirect_uri),
        encode(challenge),
        encode(state),
        encode(resource),
    )
}

/// Exchanges the code for tokens and stores them under the configured URL.
/// A reply without `access_token`, with a non-Bearer `token_type`, or not
/// JSON fails and stores nothing; without `expires_in` no expiry is
/// stored.
#[allow(
    clippy::too_many_arguments,
    reason = "one straight-line exchange: endpoints, code, client, verifier, URL and clock"
)]
fn exchange(
    files: &dyn TokenFiles,
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    verifier: &str,
    canonical: &str,
    clock: &dyn Clock,
    observe: &Observe,
) -> Result<(), LoginError> {
    const STEP: &str = "token exchange";
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", verifier),
        ("resource", canonical),
    ];
    let (status, bytes) = post_form(token_endpoint, &form, observe)
        .map_err(|_| LoginError::Exchange(STEP.to_owned()))?;
    if !(200..300).contains(&status) || bytes.len() > MAX_BODY {
        return Err(LoginError::Exchange(STEP.to_owned()));
    }
    let reply: Value =
        serde_json::from_slice(&bytes).map_err(|_| LoginError::Exchange(STEP.to_owned()))?;
    let access = reply
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|access| !access.is_empty())
        .ok_or_else(|| LoginError::Exchange(STEP.to_owned()))?;
    if ureq::http::HeaderValue::from_str(&format!("Bearer {access}")).is_err() {
        return Err(LoginError::Exchange(STEP.to_owned()));
    }
    let bearer = reply
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"));
    if !bearer {
        return Err(LoginError::Exchange(STEP.to_owned()));
    }
    let refresh = reply
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(|refresh| Secret::new(refresh.to_owned()));
    let now_secs = contract::clock::wall_ms(clock.wall()) / 1000;
    let expires_at = reply
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|lifetime| now_secs.saturating_add(lifetime));
    if reply.get("expires_in").is_some() && expires_at.is_none() {
        return Err(LoginError::Exchange(STEP.to_owned()));
    }
    let token = Token {
        access_token: Secret::new(access.to_owned()),
        refresh_token: refresh,
        expires_at,
        token_endpoint: token_endpoint.to_owned(),
        client_id: client_id.to_owned(),
    };
    store_token(files, canonical, &token).map_err(LoginError::Store)
}

/// Logs in to the remote server at `url`: discovery, registration, a PKCE
/// authorization shown through `present`, the loopback callback, and the
/// exchange, stored by canonical URL. `present` receives the authorization
/// URL once, after the listener is bound and before the wait. Nothing is
/// stored unless the exchange succeeded after a matching `state`. The
/// login stays silent: it writes no `http_request` line.
pub fn login(
    files: &dyn TokenFiles,
    url: &str,
    present: &dyn Fn(&str),
    clock: &dyn Clock,
) -> Result<(), LoginError> {
    login_observed(files, url, present, clock, None)
}

/// As [`login`], writing one `mcp` line per HTTP request the flow makes
/// on `debug`: the probe, every discovery fetch, the registration and
/// the token exchange. `None` stays silent, as [`login`] does.
pub fn login_observed(
    files: &dyn TokenFiles,
    url: &str,
    present: &dyn Fn(&str),
    clock: &dyn Clock,
    debug: Option<Arc<dyn DebugLog>>,
) -> Result<(), LoginError> {
    login_with_random(files, url, present, clock, debug, &|bytes| {
        SystemRandom::new().fill(bytes)
    })
}

/// The login with its random source as a parameter, so a test can fail
/// it: `login` runs it on the platform source.
fn login_with_random(
    files: &dyn TokenFiles,
    url: &str,
    present: &dyn Fn(&str),
    clock: &dyn Clock,
    debug: Option<Arc<dyn DebugLog>>,
    fill: &dyn Fn(&mut [u8]) -> Result<(), ring::error::Unspecified>,
) -> Result<(), LoginError> {
    if url_has_userinfo(url) {
        return Err(LoginError::Userinfo);
    }
    let canonical = crate::token::canonical_url(url).map_err(|_| LoginError::Url)?;
    let origin = origin_of(&canonical).ok_or(LoginError::Url)?;
    // Secrets first, before any network: a platform that gives no random
    // bytes fails before the listener is bound or the server is reached.
    let verifier = random_43(fill)?;
    let state = random_43(fill)?;
    // Bound before discovery, so `present` runs after the bind and the
    // listener is closed on every exit path.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| LoginError::Unreachable("callback listener".to_owned()))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| LoginError::Unreachable("callback listener".to_owned()))?;
    let port = listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|_| LoginError::Unreachable("callback listener".to_owned()))?;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let observe = Observe { debug, clock };
    // The unauthenticated probe is read for its `WWW-Authenticate` header
    // on any status.
    let (_, authenticate, _) =
        get(&canonical, &observe).map_err(|_| LoginError::Unreachable(canonical.clone()))?;
    let found = discover(authenticate.as_deref(), &canonical, &origin, &observe)?;
    let client_id = register(&found.registration_endpoint, &redirect_uri, &observe)?;
    let url = authorization_url(
        &found.authorization_endpoint,
        &client_id,
        &redirect_uri,
        &challenge_of(&verifier),
        &state,
        &canonical,
    );
    // The one exemption that prints `state` on purpose: the authorization
    // URL the browser opens.
    present(&url);
    let deadline = clock
        .now()
        .checked_add(CALLBACK_WAIT)
        .unwrap_or(clock.now());
    let code = wait_callback(&listener, clock, deadline, &state)?;
    exchange(
        files,
        &found.token_endpoint,
        &code,
        &redirect_uri,
        &client_id,
        &verifier,
        &canonical,
        clock,
        &observe,
    )?;
    let _ = key(&canonical);
    Ok(())
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
