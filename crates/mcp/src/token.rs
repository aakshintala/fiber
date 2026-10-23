//! URL-bound OAuth tokens for remote MCP servers (`docs/mcp.md`).
use crate::rpc::Outcome;
use contract::{
    ErrorCode, Secret,
    clock::Clock,
    mcp_tokens::{TokenFileError, TokenFiles},
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A remote MCP server's OAuth token, supplied by login.
#[derive(Clone)]
pub struct Token {
    /// The bearer token.
    pub access_token: Secret,
    /// A rotating refresh token, when the endpoint issues one.
    pub refresh_token: Option<Secret>,
    /// Expiry in Unix seconds, or no known expiry.
    pub expires_at: Option<u64>,
    /// The public client's token endpoint.
    pub token_endpoint: String,
    /// The registered public client id.
    pub client_id: String,
}
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("expires_at", &self.expires_at)
            .field(
                "token_endpoint",
                &crate::start::redacted_url(&self.token_endpoint),
            )
            .field("client_id", &self.client_id)
            .finish()
    }
}
/// A token could not be stored.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// The URL is not an HTTP URL without userinfo.
    #[error("The token needs an HTTP URL without a user name or password.")]
    Url,
    /// Another login or refresh holds the credential lock.
    #[error("Another login or refresh holds the credential lock.")]
    Busy,
    /// Credential storage failed.
    #[error("{0}")]
    Files(#[from] TokenFileError),
}
impl TokenError {
    /// The stable code for a token-store failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Url => ErrorCode::InvalidArguments,
            Self::Busy => ErrorCode::IoFailed,
            Self::Files(error) => error.code(),
        }
    }
}
/// Stores a token under its canonical server URL, without waiting for a lock.
pub fn store_token(files: &dyn TokenFiles, url: &str, token: &Token) -> Result<(), TokenError> {
    let url = canonical_url(url)?;
    canonical_url(&token.token_endpoint)?;
    let held = files.try_lock(&key(&url))?.ok_or(TokenError::Busy)?;
    // A new login replaces an unreadable record; only a parsed record carries
    // a generation forward. Unsafe paths still refuse the store.
    let previous = match held.read() {
        Ok(value) => value,
        Err(TokenFileError::Unreadable(_)) => None,
        Err(error) => return Err(TokenError::Files(error)),
    };
    let generation = previous
        .and_then(|value| parse_record(value).ok())
        .map_or(1, |record| record.generation.wrapping_add(1));
    let mut value = serde_json::Map::from_iter([
        ("url".into(), json!(url)),
        ("generation".into(), json!(generation)),
        ("access_token".into(), json!(token.access_token.expose())),
        ("token_endpoint".into(), json!(token.token_endpoint)),
        ("client_id".into(), json!(token.client_id)),
    ]);
    if let Some(refresh) = &token.refresh_token {
        value.insert("refresh_token".into(), json!(refresh.expose()));
    }
    if let Some(expiry) = token.expires_at {
        value.insert("expires_at".into(), json!(expiry));
    }
    held.write(&Value::Object(value))?;
    Ok(())
}

const UNUSABLE: &str = "Fiber could not use the stored login.";
const REJECTED: &str = "Fiber could not renew the login at its token endpoint.";
struct Record {
    value: serde_json::Map<String, Value>,
    generation: u64,
    token: Token,
}
fn parse_record(value: Value) -> Result<Record, String> {
    let failed = || UNUSABLE.to_owned();
    let value = value.as_object().cloned().ok_or_else(failed)?;
    let generation = value
        .get("generation")
        .and_then(Value::as_u64)
        .ok_or_else(failed)?;
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(failed)?;
    header(access)?;
    let endpoint = value
        .get("token_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(failed)?;
    canonical_url(endpoint).map_err(|_| failed())?;
    let client_id = value
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or_else(failed)?;
    let refresh_token = match value.get("refresh_token") {
        None => None,
        Some(value) => Some(Secret::new(value.as_str().ok_or_else(failed)?.to_owned())),
    };
    let expires_at = match value.get("expires_at") {
        None => None,
        Some(value) => Some(value.as_u64().ok_or_else(failed)?),
    };
    let token = Token {
        access_token: Secret::new(access.to_owned()),
        refresh_token,
        expires_at,
        token_endpoint: endpoint.to_owned(),
        client_id: client_id.to_owned(),
    };
    Ok(Record {
        value,
        generation,
        token,
    })
}
fn header(access: &str) -> Result<String, String> {
    if access.is_empty() {
        return Err(UNUSABLE.to_owned());
    }
    let header = format!("Bearer {access}");
    ureq::http::HeaderValue::from_str(&header).map_err(|_| UNUSABLE.to_owned())?;
    Ok(header)
}
fn login(detail: String) -> Outcome {
    Outcome::Login(detail)
}

pub(crate) struct Bearer {
    target: Option<(String, String)>,
    files: Arc<dyn TokenFiles>,
    clock: Arc<dyn Clock>,
    refreshing: AtomicBool,
}
#[derive(PartialEq, Eq)]
pub(crate) enum Authorization {
    None,
    Send(String),
    SendAndRefresh(String),
}
impl std::fmt::Debug for Authorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Send(_) => f.debug_tuple("Send").field(&"<redacted>").finish(),
            Self::SendAndRefresh(_) => f
                .debug_tuple("SendAndRefresh")
                .field(&"<redacted>")
                .finish(),
        }
    }
}

pub(crate) trait Refresh {
    fn stopped(&self) -> bool;
    fn pause(&self);
    fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, Vec<u8>), ()>;
}
impl Bearer {
    pub(crate) fn new(url: &str, files: Arc<dyn TokenFiles>, clock: Arc<dyn Clock>) -> Self {
        Self {
            target: canonical_url(url).ok().map(|url| {
                let key = key(&url);
                (url, key)
            }),
            files,
            clock,
            refreshing: AtomicBool::new(false),
        }
    }
    fn load(&self, value: Option<Value>) -> Result<Option<Record>, String> {
        let Some(value) = value else {
            return Ok(None);
        };
        let Some((url, _)) = &self.target else {
            return Ok(None);
        };
        let stored = value
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| UNUSABLE.to_owned())?;
        if stored != url {
            return Ok(None);
        }
        parse_record(value).map(Some)
    }
    fn record(&self) -> Result<Option<Record>, String> {
        let Some((_, key)) = &self.target else {
            return Ok(None);
        };
        self.load(self.files.read(key).map_err(|_| UNUSABLE.to_owned())?)
    }
    fn now(&self) -> u64 {
        contract::clock::wall_ms(self.clock.wall()) / 1000
    }
    pub(crate) fn stored(&self) -> Result<Option<String>, String> {
        self.record()?
            .map(|record| header(record.token.access_token.expose()))
            .transpose()
    }
    pub(crate) fn authorization(&self, wait: &dyn Refresh) -> Result<Authorization, Outcome> {
        let Some(record) = self.record().map_err(login)? else {
            return Ok(Authorization::None);
        };
        let now = self.now();
        if let Some(expiry) = record.token.expires_at {
            if expiry <= now {
                return self
                    .renew(record.generation, wait, false)
                    .map(|token| token.map_or(Authorization::None, Authorization::Send))
                    .map_err(login);
            }
            if expiry <= now.saturating_add(300) && !self.refreshing.swap(true, Ordering::AcqRel) {
                return header(record.token.access_token.expose())
                    .map(Authorization::SendAndRefresh)
                    .map_err(login);
            }
        }
        header(record.token.access_token.expose())
            .map(Authorization::Send)
            .map_err(login)
    }
    fn renew(
        &self,
        seen: u64,
        wait: &dyn Refresh,
        background: bool,
    ) -> Result<Option<String>, String> {
        let Some((url, key)) = &self.target else {
            return Ok(None);
        };
        let held = loop {
            if wait.stopped() {
                return Err(UNUSABLE.to_owned());
            }
            match self.files.try_lock(key).map_err(|_| UNUSABLE.to_owned())? {
                Some(held) => break held,
                None => wait.pause(),
            }
        };
        let Some(mut record) = self.load(held.read().map_err(|_| UNUSABLE.to_owned())?)? else {
            return Ok(None);
        };
        let now = self.now();
        let expired = record.token.expires_at.is_some_and(|expiry| expiry <= now);
        if record.generation != seen && !expired {
            return header(record.token.access_token.expose()).map(Some);
        }
        if background
            && !record
                .token
                .expires_at
                .is_some_and(|expiry| expiry <= now.saturating_add(300))
        {
            return header(record.token.access_token.expose()).map(Some);
        }
        let refresh = record
            .token
            .refresh_token
            .as_ref()
            .ok_or_else(|| UNUSABLE.to_owned())?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.expose()),
            ("client_id", record.token.client_id.as_str()),
            ("resource", url.as_str()),
        ];
        let (status, bytes) = wait
            .post(&record.token.token_endpoint, &form)
            .map_err(|()| {
                "Fiber could not reach its token endpoint to renew the login.".to_owned()
            })?;
        // A successful reply may rotate the refresh token even after the
        // request stops; persist it so the next request can still renew.
        if !(200..300).contains(&status) {
            return Err(REJECTED.to_owned());
        }
        if bytes.len() > crate::pipes::MAX_LINE {
            return Err(REJECTED.to_owned());
        }
        let reply: Value = serde_json::from_slice(&bytes).map_err(|_| REJECTED.to_owned())?;
        let access = reply
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| REJECTED.to_owned())?;
        let authorization = header(access)?;
        record.value.insert("access_token".into(), json!(access));
        if let Some(refresh) = reply.get("refresh_token") {
            let refresh = refresh.as_str().ok_or_else(|| REJECTED.to_owned())?;
            record.value.insert("refresh_token".into(), json!(refresh));
        }
        match reply.get("expires_in") {
            Some(expiry) => {
                let expiry = expiry.as_u64().ok_or_else(|| REJECTED.to_owned())?;
                record
                    .value
                    .insert("expires_at".into(), json!(now.saturating_add(expiry)));
            }
            None => {
                record.value.remove("expires_at");
            }
        }
        record.value.insert(
            "generation".into(),
            json!(record.generation.wrapping_add(1)),
        );
        held.write(&Value::Object(record.value))
            .map_err(|error| match error {
                TokenFileError::Unsynced(_) => {
                    "Fiber renewed the login but could not finish saving it.".to_owned()
                }
                TokenFileError::Refused(_)
                | TokenFileError::Unreadable(_)
                | TokenFileError::Unwritable(_) => {
                    "Fiber could not save the renewed login.".to_owned()
                }
            })?;
        Ok(Some(authorization))
    }
    pub(crate) fn refresh_in_background(self: &Arc<Self>, wait: Box<dyn Refresh + Send>) {
        let clearing = Clearing(self.clone());
        let bearer = self.clone();
        let _started = std::thread::Builder::new()
            .name("mcp-token-refresh".into())
            .spawn(move || {
                if let Ok(Some(record)) = bearer.record() {
                    let _refreshed = bearer.renew(record.generation, wait.as_ref(), true);
                }
                drop(clearing);
            });
    }
}
struct Clearing(Arc<Bearer>);
impl Drop for Clearing {
    fn drop(&mut self) {
        self.0.refreshing.store(false, Ordering::Release);
    }
}

fn split_url(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some((scheme, rest.get(..end)?, rest.get(end..)?))
}

pub(crate) fn canonical_url(url: &str) -> Result<String, TokenError> {
    let (scheme, authority, tail) = split_url(url).ok_or(TokenError::Url)?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return Err(TokenError::Url),
    };
    if authority.contains('@') {
        return Err(TokenError::Url);
    }
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or(TokenError::Url)? + 1;
        let host = authority.get(..end).ok_or(TokenError::Url)?;
        let rest = authority.get(end..).ok_or(TokenError::Url)?;
        let port = match rest {
            "" => None,
            rest => Some(rest.strip_prefix(':').ok_or(TokenError::Url)?),
        };
        (host, port)
    } else {
        authority
            .rsplit_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)))
    };
    if host.is_empty() {
        return Err(TokenError::Url);
    }
    let port = match port {
        None | Some("") => default,
        Some(port) => {
            if !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(TokenError::Url);
            }
            port.parse::<u16>().map_err(|_| TokenError::Url)?
        }
    };
    let host = host.to_ascii_lowercase();
    let tail = tail.split_once('#').map_or(tail, |(tail, _)| tail);
    if port == default {
        Ok(format!("{scheme}://{host}{tail}"))
    } else {
        Ok(format!("{scheme}://{host}:{port}{tail}"))
    }
}
pub(crate) fn key(canonical: &str) -> String {
    support::sha256_hex(canonical.as_bytes())
}
/// The file name of the token for `url`: the lowercase hex SHA-256 of
/// the canonical URL's bytes (`docs/mcp.md`, "What Fiber does with
/// MCP").
pub fn token_key(url: &str) -> Result<String, TokenError> {
    Ok(key(&canonical_url(url)?))
}
/// Whether the URL's authority carries a user name or password.
pub fn url_has_userinfo(url: &str) -> bool {
    split_url(url).is_some_and(|(_, authority, _)| authority.contains('@'))
}

#[cfg(test)]
#[path = "token_tests.rs"]
mod tests;
