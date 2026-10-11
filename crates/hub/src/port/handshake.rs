//! The websocket opening handshake (RFC 6455, 1.3) on the hub's port:
//! the request line, the required headers and the page's origin. Bytes past
//! the blank line stay readable on the same reader, so a client that
//! pipelines its first frame loses nothing.

use std::io::{BufRead, Read};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::digest;

/// The most bytes the handshake request (request line, headers and the
/// blank line) may hold: past it the hub answers 400. The request is read
/// through [`Read::take`], so the cap is not a comparison.
pub(crate) const HANDSHAKE_MAX: u64 = 8_192;

/// Why the handshake was refused.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Refusal {
    /// Not a websocket request: 400.
    BadRequest,
    /// The page's origin is not listed: 403.
    Forbidden,
}

/// Reads the handshake request, checks it and the page's origin, and
/// answers with the `Sec-WebSocket-Accept` key: `Ok` holds it, `Err` how
/// to refuse. A request with no `Origin` header comes from a program
/// rather than a page, and passes whatever the list holds.
pub(crate) fn read_request(read: &mut impl BufRead, origins: &[String]) -> Result<String, Refusal> {
    let lines = request_lines(read)?;
    let [request, headers @ ..] = lines.as_slice() else {
        return Err(Refusal::BadRequest);
    };
    let parts: Vec<&str> = request.split(' ').collect();
    let ["GET", _path, "HTTP/1.1"] = parts.as_slice() else {
        return Err(Refusal::BadRequest);
    };
    let mut fields: Vec<(String, String)> = Vec::new();
    for header in headers {
        let Some((name, value)) = header.split_once(':') else {
            return Err(Refusal::BadRequest);
        };
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err(Refusal::BadRequest);
        }
        fields.push((name, value.trim().to_owned()));
    }
    let values = |name: &str| -> Vec<&str> {
        fields
            .iter()
            .filter(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
            .collect()
    };
    if !values("upgrade")
        .join(",")
        .trim()
        .eq_ignore_ascii_case("websocket")
    {
        return Err(Refusal::BadRequest);
    }
    if !values("connection")
        .join(",")
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
    {
        return Err(Refusal::BadRequest);
    }
    if values("sec-websocket-version").join(",").trim() != "13" {
        return Err(Refusal::BadRequest);
    }
    let key = values("sec-websocket-key").join(",");
    let decoded = STANDARD
        .decode(key.trim())
        .map_err(|_| Refusal::BadRequest)?;
    if decoded.len() != 16 {
        return Err(Refusal::BadRequest);
    }
    if !values("origin")
        .iter()
        .all(|origin| origins.iter().any(|listed| listed == origin))
    {
        return Err(Refusal::Forbidden);
    }
    Ok(accept_key(key.trim()))
}

/// The `Sec-WebSocket-Accept` answer for `key`: SHA-1 of the key and the
/// websocket GUID, base64-encoded (RFC 6455, 1.3).
pub(crate) fn accept_key(key: &str) -> String {
    let concat = format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    STANDARD.encode(digest::digest(
        &digest::SHA1_FOR_LEGACY_USE_ONLY,
        concat.as_bytes(),
    ))
}

/// The 101 answer carrying `accept`.
pub(crate) fn switching(accept: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\
         \r\n"
    )
    .into_bytes()
}

/// The refusal answer: 400 with the connection closed, or 403.
pub(crate) fn refused(refusal: Refusal) -> Vec<u8> {
    match refusal {
        Refusal::BadRequest => {
            "HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".as_bytes()
        }
        Refusal::Forbidden => {
            "HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".as_bytes()
        }
    }
    .to_vec()
}

/// Reads the request line and headers through the cap, one line at a
/// time: past the cap, at EOF before the blank line, or on a line with no
/// LF, the request is refused. Nothing past the blank line is consumed, so
/// pipelined bytes stay readable on the same reader.
fn request_lines(read: &mut impl BufRead) -> Result<Vec<String>, Refusal> {
    let mut capped = read.take(HANDSHAKE_MAX);
    let mut lines = Vec::new();
    loop {
        let mut line = Vec::new();
        match capped.read_until(b'\n', &mut line) {
            Ok(_) if line.ends_with(b"\n") => {}
            Ok(_) | Err(_) => return Err(Refusal::BadRequest),
        }
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let text = String::from_utf8(line).map_err(|_| Refusal::BadRequest)?;
        if text.is_empty() {
            return Ok(lines);
        }
        lines.push(text);
    }
}

#[cfg(test)]
#[path = "handshake_tests.rs"]
mod tests;
