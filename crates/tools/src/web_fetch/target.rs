//! The URL rules of `web_fetch` (`docs/tools.md`, "web_fetch" and "Effects"):
//! which URLs are accepted and what they declare, how a redirect's
//! `location` joins the current URL, and which hosts and addresses fetch
//! refuses.

use std::net::{IpAddr, Ipv6Addr};

use ureq::http::Uri;
use ureq::http::uri::Scheme;

/// The addresses of a cloud metadata server that are not link-local, as the
/// name check cannot see them: AWS's IPv6 one.
const AWS_METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// Cloud metadata host names, lowercase and without a trailing dot.
const METADATA_NAMES: [&str; 2] = ["metadata.google.internal", "metadata.goog"];

/// `url` without a fragment and surrounding whitespace.
fn without_fragment(url: &str) -> &str {
    url.split_once('#').map_or(url, |(before, _)| before).trim()
}

/// Parses what the model wrote: an `http` or `https` URL with a host. The
/// fragment is dropped, since it is never sent.
pub(super) fn parse(url: &str) -> Result<Uri, String> {
    let refused = |why: &str| format!("`{url}` is not a URL to fetch: {why}.");
    let uri: Uri = without_fragment(url)
        .parse()
        .map_err(|error| refused(&format!("{error}")))?;
    if uri.scheme() != Some(&Scheme::HTTP) && uri.scheme() != Some(&Scheme::HTTPS) {
        return Err(refused("the scheme must be `http` or `https`"));
    }
    if uri.host().is_none_or(str::is_empty) {
        return Err(refused("it names no host"));
    }
    Ok(uri)
}

fn scheme(uri: &Uri) -> &str {
    uri.scheme_str().unwrap_or("http")
}

/// The authority without its user information, host lowercased and port as
/// written.
fn host_port(uri: &Uri) -> String {
    let authority = uri.authority().map_or("", |authority| authority.as_str());
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, rest)| rest);
    host_port.to_ascii_lowercase()
}

/// The path and query, with `/` for an empty path.
fn path_and_query(uri: &Uri) -> String {
    let path = uri.path_and_query().map_or("", |path| path.as_str());
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

/// What a rule matches: the URL as parsed, whose path is at least `/`.
pub(super) fn subject(uri: &Uri) -> String {
    let authority = uri.authority().map_or("", |authority| authority.as_str());
    let userinfo = authority
        .rsplit_once('@')
        .map_or(String::new(), |(userinfo, _)| format!("{userinfo}@"));
    format!(
        "{}://{userinfo}{}{}",
        scheme(uri),
        host_port(uri),
        path_and_query(uri)
    )
}

/// The widening a rule would offer: the URL's scheme and host, ending in `/`.
pub(super) fn prefix(uri: &Uri) -> String {
    format!("{}://{}/", scheme(uri), host_port(uri))
}

/// The host to resolve, without the brackets of an IPv6 literal and
/// lowercased, and the port, from the URL or the scheme's default.
pub(super) fn host_and_port(uri: &Uri) -> (String, u16) {
    let host = uri.host().unwrap_or_default();
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    let default = if uri.scheme() == Some(&Scheme::HTTPS) {
        443
    } else {
        80
    };
    (host.to_ascii_lowercase(), uri.port_u16().unwrap_or(default))
}

/// Whether `location` starts with a URI scheme, such as `https:`.
fn has_scheme(location: &str) -> bool {
    let Some((scheme, _)) = location.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The URL a redirect to `location` names, from `base`, the URL that sent it.
/// A fragment is dropped and dot segments are not resolved. A `location`
/// that is only a query keeps `base`'s path, as RFC 3986 says; every other
/// relative one replaces the last segment of it. The result is not checked:
/// [`parse`] refuses a scheme that is not `http` or `https`.
pub(super) fn join(base: &Uri, location: &str) -> String {
    let location = without_fragment(location);
    let scheme = scheme(base);
    let authority = base.authority().map_or("", |authority| authority.as_str());
    if location.is_empty() {
        return format!("{scheme}://{authority}{}", path_and_query(base));
    }
    if has_scheme(location) {
        return location.to_owned();
    }
    if location.starts_with("//") {
        return format!("{scheme}:{location}");
    }
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}");
    }
    if location.starts_with('?') {
        return format!("{scheme}://{authority}{}{location}", base.path());
    }
    let path = base.path();
    let directory = path
        .rfind('/')
        .map_or("/", |last| path.get(..=last).unwrap_or("/"));
    format!("{scheme}://{authority}{directory}{location}")
}

/// Whether `host`, a name or an address literal, is a cloud metadata host.
pub(super) fn blocked_name(host: &str) -> bool {
    let name = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    METADATA_NAMES.contains(&name.as_str())
}

/// Whether fetch refuses `address`: link-local (`169.254.0.0/16`,
/// `fe80::/10`), the IPv4-mapped form of one, or AWS's IPv6 metadata
/// address. Loopback and private addresses pass.
pub(super) fn blocked_addr(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6 == AWS_METADATA_V6
                || v6.is_unicast_link_local()
                || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_link_local())
        }
    }
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod tests;
