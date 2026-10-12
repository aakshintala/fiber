//! `fiber mcp login`'s authorization flow (`docs/mcp.md`, "What Fiber does
//! with MCP"): discovery, dynamic client registration, a PKCE authorization
//! code on a loopback redirect, and the token exchange, stored by canonical
//! URL. Method follows MCP's authorization spec: protected-resource metadata
//! (RFC 9728), authorization-server metadata (RFC 8414), client registration
//! (RFC 7591, public client), PKCE `S256` (RFC 7636), and the RFC 8707
//! `resource` parameter.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use contract::clock::Clock;
use contract::mcp_tokens::TokenFiles;
use contract::{ErrorCode, Secret};
use ring::rand::SecureRandom;
use serde_json::Value;
use ureq::unversioned::resolver::DefaultResolver;

use crate::token::{Token, key, store_token, url_has_userinfo};

/// How long the login waits for the browser callback, on the injected
/// clock: 15 minutes, the precedent `credential`'s callback waits.
pub const CALLBACK_WAIT: Duration = Duration::from_secs(900);

/// The longest request head the callback listener reads. Past it the
/// client gets a 431 and the listener serves the next connection.
const MAX_HEAD: usize = 8 * 1024;

/// The longest metadata or token body the login reads: past it the step
/// fails, and nothing is stored.
const MAX_BODY: usize = 1024 * 1024;

/// How many reads in a row a callback connection may send nothing before
/// the listener drops it and serves the next. Browsers open idle
/// speculative connections, so silence on one cannot end the wait. The
/// count restarts whenever bytes arrive.
const SILENT_POLLS: u32 = 100;

/// One callback socket read's kernel timeout: short, so the injected
/// 900 s deadline is checked between reads and an idle or slow-drip
/// connection cannot extend the wait.
const READ_TIMEOUT: Duration = Duration::from_millis(20);

/// One accept poll's real-time pause between listener checks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

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
            Self::Discovery(_)
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
            Self::Discovery(step) => write!(
                f,
                "Fiber could not discover the server's authorization flow at {step}."
            ),
            Self::Registration(step) => write!(
                f,
                "Fiber could not register with the authorization server at {step}."
            ),
            Self::Exchange(step) => {
                write!(f, "Fiber could not exchange the authorization code at {step}.")
            }
            Self::Callback(step) => {
                write!(f, "Fiber could not read the browser callback at {step}.")
            }
            Self::Denied(code) if is_known_error(code) => write!(
                f,
                "The authorization server returned an error: {code}."
            ),
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
/// value prints the generic sentence, never the value.
fn is_known_error(code: &str) -> bool {
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
/// body capped at 1 MiB plus one byte.
fn get(url: &str) -> Result<(u16, Option<String>, Vec<u8>), ureq::Error> {
    let mut response = agent().get(url).call()?;
    let status = response.status().as_u16();
    let authenticate = response
        .headers()
        .get("www-authenticate")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| ureq::Error::Timeout(ureq::Timeout::Global))?;
    Ok((status, authenticate, body))
}

/// A JSON `POST`'s status and its body capped at 1 MiB plus one byte.
fn post_json(url: &str, body: &str) -> Result<(u16, Vec<u8>), ureq::Error> {
    let mut response = agent()
        .post(url)
        .header("content-type", "application/json")
        .send(body)?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ureq::Error::Timeout(ureq::Timeout::Global))?;
    Ok((status, bytes))
}

/// A form `POST`'s status and its body capped at 1 MiB plus one byte.
fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(u16, Vec<u8>), ureq::Error> {
    let mut response = agent().post(url).send_form(form.iter().copied())?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ureq::Error::Timeout(ureq::Timeout::Global))?;
    Ok((status, bytes))
}

/// Whether `url` is an http or https URL without userinfo: every
/// discovered endpoint must be one, since the token is only ever sent to
/// the URL it was issued for.
fn is_usable_endpoint(url: &str) -> bool {
    let Ok(uri): Result<ureq::http::Uri, _> = url.parse() else {
        return false;
    };
    let scheme = uri.scheme_str().map(str::to_ascii_lowercase);
    if !matches!(scheme.as_deref(), Some("http") | Some("https")) {
        return false;
    }
    uri.host().is_some() && !url_has_userinfo(url)
}

/// The origin of a canonical URL: its scheme and authority, the fallback
/// authorization server when there is no protected-resource metadata.
fn origin_of(canonical: &str) -> Option<String> {
    let (scheme, rest) = canonical.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(format!("{scheme}://{}", rest.get(..end)?))
}

/// The `resource_metadata` URL of a `WWW-Authenticate` header, when the
/// probe carried one.
fn resource_metadata_url(header: &str) -> Option<String> {
    let start = header.find("resource_metadata")?;
    let quote = header.get(start..)?.find('"')?;
    let rest = header.get(start + quote + 1..)?;
    let end = rest.find('"')?;
    rest.get(..end).map(str::to_owned)
}

/// Fetches one JSON document: a 404 means absent, a 2xx is parsed and a
/// body past 1 MiB or not JSON fails the named step, and any other status
/// or a transport error fails it too. Nothing fetched is retried.
fn fetch_json(
    url: &str,
    step: &str,
) -> Result<Option<Value>, LoginError> {
    let (status, _, body) = post_get(url).map_err(|_| LoginError::Discovery(step.to_owned()))?;
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(LoginError::Discovery(step.to_owned()));
    }
    if body.len() > MAX_BODY {
        return Err(LoginError::Discovery(step.to_owned()));
    }
    serde_json::from_slice(&body).map(Some).map_err(|_| LoginError::Discovery(step.to_owned()))
}

fn post_get(url: &str) -> Result<(u16, Option<String>, Vec<u8>), ureq::Error> {
    get(url)
}

/// The protected-resource metadata: the header's document when the probe
/// named one, else the path-inserted well-known document, else the root
/// one. Each candidate is fetched in order; a 404 continues to the next.
fn protected_resource(
    agent_probe: (u16, Option<String>),
    canonical: &str,
    origin: &str,
) -> Result<Option<Value>, LoginError> {
    const STEP: &str = "protected-resource metadata";
    if let Some(url) = agent_probe.1.as_deref().and_then(resource_metadata_url) {
        if !is_usable_endpoint(&url) {
            return Err(LoginError::Discovery(STEP.to_owned()));
        }
        return match fetch_json(&url, STEP)? {
            Some(document) => Ok(Some(document)),
            None => Err(LoginError::Discovery(STEP.to_owned())),
        };
    }
    let path = canonical
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|at| rest.get(at..).unwrap_or_default()))
        .unwrap_or_default();
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut candidates = Vec::new();
    if !path.is_empty() && path != "/" {
        candidates.push(format!("{origin}/.well-known/oauth-protected-resource{path}"));
    }
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    for url in &candidates {
        match fetch_json(url, STEP)? {
            Some(document) => return Ok(Some(document)),
            None => continue,
        }
    }
    Ok(None)
}

/// The authorization server's base: the first server the
/// protected-resource metadata lists, else the resource's own origin.
fn authorization_base(prm: Option<&Value>, origin: &str) -> Result<String, LoginError> {
    const STEP: &str = "protected-resource metadata";
    let first = prm
        .and_then(|document| document.get("authorization_servers"))
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(Value::as_str)
        .map(str::to_owned);
    match first {
        Some(server) => {
            if !is_usable_endpoint(&server) {
                return Err(LoginError::Discovery(STEP.to_owned()));
            }
            Ok(server.trim_end_matches('/').to_owned())
        }
        None => Ok(origin.to_owned()),
    }
}

/// The authorization server's metadata: the `oauth-authorization-server`
/// document, then `openid-configuration`. A 404 continues; anything else
/// failing is terminal at the named step.
fn authorization_metadata(base: &str) -> Result<Value, LoginError> {
    const STEP: &str = "authorization-server metadata";
    let base = base.trim_end_matches('/');
    for url in [
        format!("{base}/.well-known/oauth-authorization-server"),
        format!("{base}/.well-known/openid-configuration"),
    ] {
        match fetch_json(&url, STEP)? {
            Some(document) => return Ok(document),
            None => continue,
        }
    }
    Err(LoginError::Discovery(STEP.to_owned()))
}

struct Discovered {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
}

fn discovered(metadata: &Value) -> Result<Discovered, LoginError> {
    const STEP: &str = "authorization-server metadata";
    let endpoint = |name| {
        metadata
            .get(name)
            .and_then(Value::as_str)
            .filter(|url| is_usable_endpoint(url))
            .map(str::to_owned)
    };
    let Some(authorization_endpoint) = endpoint("authorization_endpoint") else {
        return Err(LoginError::Discovery(STEP.to_owned()));
    };
    let Some(token_endpoint) = endpoint("token_endpoint") else {
        return Err(LoginError::Discovery(STEP.to_owned()));
    };
    let Some(registration_endpoint) = endpoint("registration_endpoint") else {
        return Err(LoginError::Discovery(
            "client registration endpoint".to_owned(),
        ));
    };
    // MCP's authorization flow requires PKCE: an authorization server
    // whose metadata does not list `S256` is refused before anything is
    // registered, with no error code printed.
    let offers = metadata
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        .is_some_and(|methods| {
            methods.iter().any(|method| method.as_str() == Some("S256"))
        });
    if !offers {
        return Err(LoginError::PkceUnsupported);
    }
    Ok(Discovered {
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
    })
}

/// Registers the public client: a 2xx with a JSON `client_id` answers it,
/// and anything else is terminal, never retried.
fn register(registration_endpoint: &str, redirect_uri: &str) -> Result<String, LoginError> {
    const STEP: &str = "client registration";
    let body = serde_json::json!({
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "client_name": "Fiber",
    })
    .to_string();
    let (status, bytes) =
        post_json(registration_endpoint, &body).map_err(|_| LoginError::Registration(STEP.to_owned()))?;
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
/// section 4.1): a verifier, and a `state` of the same shape.
fn random_43() -> String {
    let mut bytes = [0_u8; 32];
    ring::rand::SystemRandom::new().fill(&mut bytes).unwrap_or_else(|_| {
        use std::hash::{BuildHasher, Hasher};
        // Randomness is best-effort here only in the sense that a
        // platform without any cannot run at all; the fallback below
        // still differs per call.
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write(&std::time::SystemTime::now().elapsed().map_or(0, |d| d.as_nanos()).to_le_bytes());
        hasher.write(&std::process::id().to_le_bytes());
        hasher.write(&bytes);
            let seed = hasher.finish();
            for (at, byte) in bytes.iter_mut().enumerate() {
                *byte = seed.wrapping_add(at as u64 * 0x9E3779B97F4A7C15).to_le_bytes()[0];
            }
        });
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 challenge of `verifier` (RFC 7636, section 4.2).
fn challenge_of(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()))
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
    format!(
        "{endpoint}?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}&resource={}",
        encode(client_id),
        encode(redirect_uri),
        encode(challenge),
        encode(state),
        encode(resource),
    )
}

/// A query string's parameters in order, percent-decoded with `+` as a
/// space.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

fn decode(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = |next: Option<u8>| next.and_then(|b| char::from(b).to_digit(16));
                match (hex(bytes.next()), hex(bytes.next())) {
                    (Some(high), Some(low)) => out.push((high * 16 + low) as u8),
                    _ => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The request line's target, when the head is a request.
fn request_target(head: &[u8]) -> Option<&str> {
    let line = head.split(|byte| *byte == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split_whitespace();
    let (_method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    (version.starts_with("HTTP/") && parts.next().is_none()).then_some(target)
}

fn respond(stream: &mut TcpStream, status: &str) {
    let body = if status.starts_with("200") {
        "<!doctype html><title>Fiber</title><p>You can close this tab and return to Fiber.</p>"
    } else {
        ""
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    match stream.write_all(reply.as_bytes()) {
        Ok(()) | Err(_) => {}
    }
}

enum Served {
    /// The wait is over with the authorization code.
    Code(String),
    /// The wait is over with a login failure.
    Failed(LoginError),
}

/// Answers one callback connection. `Some` ends the wait; `None` serves
/// the next connection.
fn answer(
    mut stream: TcpStream,
    clock: &dyn Clock,
    deadline: Instant,
    state: &str,
) -> Option<Served> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    let mut silent = 0;
    loop {
        // The whole head read is bounded by the injected deadline: an
        // idle or slow-drip connection cannot extend the wait.
        if clock.now() >= deadline {
            return Some(Served::Failed(LoginError::Timeout));
        }
        if head.len() >= MAX_HEAD {
            respond(&mut stream, "431 Request Header Fields Too Large");
            return None;
        }
        match stream.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) => {
                head.push(byte[0]);
                silent = 0;
            }
            Err(_) => {
                silent += 1;
                if silent >= SILENT_POLLS {
                    return None;
                }
            }
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
    }
    let Some(target) = request_target(&head) else {
        respond(&mut stream, "400 Bad Request");
        return Some(Served::Failed(LoginError::Callback("callback request".to_owned())));
    };
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    if path != "/callback" {
        respond(&mut stream, "404 Not Found");
        return None;
    }
    let query = target.split_once('?').map_or("", |(_, query)| query);
    let pairs = parse_query(query);
    let find = |name| pairs.iter().find(|(key, _)| key == name).map(|(_, v)| v.as_str());
    respond(&mut stream, "200 OK");
    if let Some(error) = find("error") {
        // Only the seven codes print; any other value, and never the
        // description or an echo of `state`, prints the generic sentence.
        if is_known_error(error) {
            return Some(Served::Failed(LoginError::Denied(error.to_owned())));
        }
        return Some(Served::Failed(LoginError::Denied(String::new())));
    }
    let (Some(code), Some(returned)) = (find("code"), find("state")) else {
        return Some(Served::Failed(LoginError::Callback("callback parameters".to_owned())));
    };
    if code.is_empty() || returned != state {
        return Some(Served::Failed(LoginError::Callback("callback state".to_owned())));
    }
    Some(Served::Code(code.to_owned()))
}

/// Waits for the first `/callback` request, on the injected clock. A
/// request for another path gets 404 and the wait continues; the first
/// `/callback` request ends it.
fn wait_callback(
    listener: &TcpListener,
    clock: &dyn Clock,
    deadline: Instant,
    state: &str,
) -> Result<String, LoginError> {
    loop {
        if clock.now() >= deadline {
            return Err(LoginError::Timeout);
        }
        match listener.accept() {
            Ok((stream, _)) => match answer(stream, clock, deadline, state) {
                None => {}
                Some(Served::Code(code)) => return Ok(code),
                Some(Served::Failed(error)) => return Err(error),
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // Parked on the clock, so a test's `await_parked` sees the
                // wait; the short real pause paces the poll.
                clock.wait_until(Some(deadline), &mut |_| {
                    std::thread::sleep(POLL_PAUSE);
                });
            }
            Err(_) => {
                clock.wait_until(Some(deadline), &mut |_| {
                    std::thread::sleep(POLL_PAUSE);
                });
            }
        }
    }
}

/// Exchanges the code for tokens and stores them under the configured URL.
/// A reply without `access_token`, with a non-Bearer `token_type`, or not
/// JSON fails and stores nothing; without `expires_in` no expiry is
/// stored.
fn exchange(
    files: &dyn TokenFiles,
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    verifier: &str,
    canonical: &str,
    clock: &dyn Clock,
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
    let (status, bytes) =
        post_form(token_endpoint, &form).map_err(|_| LoginError::Exchange(STEP.to_owned()))?;
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
/// stored unless the exchange succeeded after a matching `state`.
pub fn login(
    files: &dyn TokenFiles,
    url: &str,
    present: &dyn Fn(&str),
    clock: &dyn Clock,
) -> Result<(), LoginError> {
    if url_has_userinfo(url) {
        return Err(LoginError::Userinfo);
    }
    let canonical = crate::token::canonical_url(url).map_err(|_| LoginError::Url)?;
    let origin = origin_of(&canonical).ok_or(LoginError::Url)?;
    // Bound before discovery, so `present` runs after the bind and the
    // listener is closed on every exit path.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| LoginError::Unreachable("callback listener".to_owned()))?;
    listener.set_nonblocking(true).map_err(|_| LoginError::Unreachable("callback listener".to_owned()))?;
    let port = listener.local_addr().map(|addr| addr.port()).map_err(|_| LoginError::Unreachable("callback listener".to_owned()))?;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    // The unauthenticated probe is read for its `WWW-Authenticate` header
    // on any status.
    let (_, authenticate, _) = get(&canonical).map_err(|_| LoginError::Unreachable(canonical.clone()))?;
    let prm = protected_resource((0, authenticate), &canonical, &origin)?;
    let base = authorization_base(prm.as_ref(), &origin)?;
    let metadata = authorization_metadata(&base)?;
    let found = discovered(&metadata)?;
    let client_id = register(&found.registration_endpoint, &redirect_uri)?;
    let verifier = random_43();
    let state = random_43();
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
    let deadline = clock.now().checked_add(CALLBACK_WAIT).unwrap_or(clock.now());
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
    )?;
    let _ = key(&canonical);
    Ok(())
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
