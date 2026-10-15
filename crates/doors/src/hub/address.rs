use crate::Error;
use std::path::PathBuf;

/// A websocket endpoint or a forwarded local socket.
#[derive(Debug)]
pub enum Address {
    /// An absolute socket path.
    Unix(PathBuf),
    /// A websocket endpoint.
    Web(net::websocket::Endpoint),
}

/// Hub names must also be safe credential file names.
pub const NAME_RULE: &str = "A hub name is 1 to 64 letters, digits, dots, hyphens or underscores, does not start with a dot, does not end in `.lock` or `.tmp`, and is not `local`.";

/// Checks a hub name before any file or connection is opened.
pub fn check_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('.')
        || name == "local"
        || name.ends_with(".lock")
        || name.ends_with(".tmp")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(Error::BadName { name: name.into() });
    }
    Ok(())
}

impl Address {
    /// Parses `ws://host[:port][/target]`, `wss://...`, or `unix:/absolute/path`.
    pub fn parse(text: &str) -> Result<Self, Error> {
        if text.chars().any(|c| c.is_whitespace() || c.is_control()) || text.contains('#') {
            return Err(bad_address());
        }
        if let Some(path) = text.strip_prefix("unix:") {
            let path = PathBuf::from(path);
            return if path.is_absolute() {
                Ok(Self::Unix(path))
            } else {
                Err(bad_address())
            };
        }
        let (secure, rest) = if let Some(rest) = text.strip_prefix("ws://") {
            (false, rest)
        } else if let Some(rest) = text.strip_prefix("wss://") {
            (true, rest)
        } else {
            return Err(bad_address());
        };
        let (authority, target) = rest
            .split_once('/')
            .map_or((rest, "/".to_owned()), |(host, path)| {
                (host, format!("/{path}"))
            });
        if authority.contains(['@', '?']) {
            return Err(bad_address());
        }
        let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
            let (host, suffix) = bracketed.split_once(']').ok_or_else(bad_address)?;
            host.parse::<std::net::Ipv6Addr>()
                .map_err(|_| bad_address())?;
            let port = if suffix.is_empty() {
                None
            } else {
                Some(suffix.strip_prefix(':').ok_or_else(bad_address)?)
            };
            (host, port)
        } else {
            let (host, port) = authority
                .split_once(':')
                .map_or((authority, None), |(host, port)| (host, Some(port)));
            if host.contains(['[', ']']) {
                return Err(bad_address());
            }
            (host, port)
        };
        if host.is_empty() {
            return Err(bad_address());
        }
        let port = match port {
            None => {
                if secure {
                    443
                } else {
                    80
                }
            }
            Some(port) => {
                if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(bad_address());
                }
                let port = port.parse::<u16>().map_err(|_| bad_address())?;
                if port == 0 {
                    return Err(bad_address());
                }
                port
            }
        };
        Ok(Self::Web(net::websocket::Endpoint {
            secure,
            host: host.into(),
            port,
            target,
        }))
    }
}

fn bad_address() -> Error {
    Error::BadAddress { why: "A hub address is ws://host[:port][/target], wss://host[:port][/target], or unix: followed by an absolute socket path; no user part, fragment, whitespace or control characters.".into() }
}

#[cfg(test)]
#[path = "address_tests.rs"]
mod tests;
