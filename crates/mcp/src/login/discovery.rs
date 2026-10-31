//! The login's server discovery (`docs/mcp.md`, "What Fiber does with
//! MCP"): the protected-resource metadata (RFC 9728), the
//! authorization-server metadata (RFC 8414), and the endpoints that
//! metadata lists with their PKCE `S256` offer. The single entry is
//! [`discover`]; everything else is its helper. The transport stays
//! `login`'s: discovery reads through its `get` and shares its body cap,
//! so the boundary is the entry's signature, not shared privates.

use serde_json::Value;

use super::{LoginError, MAX_BODY, Observe, get, origin_of};
use crate::token::url_has_userinfo;

/// The endpoints discovery found: where to authorize, exchange, and
/// register, each an HTTP URL without userinfo.
pub(crate) struct Discovered {
    pub(crate) authorization_endpoint: String,
    pub(crate) token_endpoint: String,
    pub(crate) registration_endpoint: String,
}

/// Discovers the login's endpoints: the protected-resource metadata's
/// first authorization server, else the resource's own origin, then that
/// server's metadata, then the endpoints that metadata lists.
pub(crate) fn discover(
    authenticate: Option<&str>,
    canonical: &str,
    origin: &str,
    observe: &Observe,
) -> Result<Discovered, LoginError> {
    let prm = protected_resource(authenticate, canonical, origin, observe)?;
    let base = authorization_base(prm.as_ref(), origin)?;
    discovered(&authorization_metadata(&base, observe)?)
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
fn fetch_json(url: &str, step: &str, observe: &Observe) -> Result<Option<Value>, LoginError> {
    let (status, _, body) =
        get(url, observe).map_err(|_| LoginError::Discovery(step.to_owned()))?;
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(LoginError::Discovery(step.to_owned()));
    }
    if body.len() > MAX_BODY {
        return Err(LoginError::Discovery(step.to_owned()));
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|_| LoginError::Discovery(step.to_owned()))
}

/// The protected-resource metadata: the header's document when the probe
/// named one, else the path-inserted well-known document, else the root
/// one. Each candidate is fetched in order; a 404 continues to the next.
fn protected_resource(
    authenticate: Option<&str>,
    canonical: &str,
    origin: &str,
    observe: &Observe,
) -> Result<Option<Value>, LoginError> {
    const STEP: &str = "protected-resource metadata";
    if let Some(url) = authenticate.and_then(resource_metadata_url) {
        if !is_usable_endpoint(&url) {
            return Err(LoginError::Discovery(STEP.to_owned()));
        }
        return match fetch_json(&url, STEP, observe)? {
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
        candidates.push(format!(
            "{origin}/.well-known/oauth-protected-resource{path}"
        ));
    }
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    for url in &candidates {
        match fetch_json(url, STEP, observe)? {
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

/// The authorization server's metadata, fetched in order: the RFC 8414
/// path-inserted `oauth-authorization-server` document first for a
/// path-bearing issuer, then the base-appended one, then
/// `openid-configuration`. A pathless base names its first two alike, and
/// each URL is fetched once. A 404 continues; anything else failing is
/// terminal at the named step.
fn authorization_metadata(base: &str, observe: &Observe) -> Result<Value, LoginError> {
    const STEP: &str = "authorization-server metadata";
    let base = base.trim_end_matches('/');
    let origin = origin_of(base).unwrap_or_else(|| base.to_owned());
    let path = base
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|at| rest.get(at..).unwrap_or_default()))
        .unwrap_or_default();
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut candidates = Vec::new();
    if !path.is_empty() && path != "/" {
        // RFC 8414 section 3.1: a path-bearing issuer's well-known URI
        // inserts `/.well-known/oauth-authorization-server` between the
        // origin and the path.
        candidates.push(format!(
            "{origin}/.well-known/oauth-authorization-server{path}"
        ));
    }
    candidates.push(format!("{base}/.well-known/oauth-authorization-server"));
    candidates.push(format!("{base}/.well-known/openid-configuration"));
    candidates.dedup();
    for url in &candidates {
        match fetch_json(url, STEP, observe)? {
            Some(document) => return Ok(document),
            None => continue,
        }
    }
    Err(LoginError::Discovery(STEP.to_owned()))
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
        .is_some_and(|methods| methods.iter().any(|method| method.as_str() == Some("S256")));
    if !offers {
        return Err(LoginError::PkceUnsupported);
    }
    Ok(Discovered {
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
    })
}
