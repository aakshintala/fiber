//! The bounded HTTP/1.1 request reader: framing only, no listener or
//! adapter state. It receives a core [`Connection`](super::Connection)
//! with its budget, and returns the raw request plus its framing outcome.
//! Deadline expiry panics at the core boundary naming the request and
//! phase; shutdown ends stopped; EOF or malformed framing returns a named
//! outcome the adapter maps. Caps admit equality and refuse one more,
//! checked with remaining-room arithmetic and overflow probes, never by
//! allocating an advertised length.

use std::fmt;

use super::{Connection, FillOutcome, Limits};
use crate::provider_server::{CREDENTIAL_HEADERS, fingerprint, fingerprint_query};

/// Why a chunked body could not be decoded. The wire texts match the
/// provider fake's, so its assertions survive the move.
type Malformed = &'static str;

/// Truncation, wherever framing ends before the body does.
const TRUNCATED: Malformed = "the chunked body ends before its framing does";

/// One request as received: raw method, target and headers, the decoded
/// body, and why its chunked framing is malformed, if it is. Malformed
/// bodies keep what decoded before the failure; credential projection is
/// the adapter's, never this reader's.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ParsedRequest {
    pub(crate) method: String,
    pub(crate) target: String,
    /// Headers in received order, names lowercased.
    pub(crate) headers: Vec<(String, String)>,
    /// The body bytes as received: fixed by length, chunked decoded.
    pub(crate) body: Vec<u8>,
    /// Why chunked framing is malformed, if it is.
    pub(crate) malformed: Option<Malformed>,
}

/// What one request read found.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum RequestOutcome {
    /// The whole head, and the body or its decoded prefix.
    Complete(ParsedRequest),
    /// The peer closed mid-head or mid-fixed-body: nothing to record.
    Eof,
    /// Shutdown ended the wait.
    Stopped,
    /// The head or declared length is unusable: the reason names it.
    Invalid(&'static str),
}

/// A request header for `Debug`: credential values appear only as
/// fingerprints, as in the recorded request.
struct ShownHeader<'a>(&'a (String, String));

impl fmt::Debug for ShownHeader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, value) = self.0;
        if CREDENTIAL_HEADERS.contains(&name.as_str()) {
            write!(f, "({name:?}, {:?})", fingerprint(value))
        } else {
            write!(f, "({name:?}, {value:?})")
        }
    }
}

/// Headers for `Debug`: each in received order, credentials fingerprinted.
struct ShownHeaders<'a>(&'a [(String, String)]);

impl fmt::Debug for ShownHeaders<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(ShownHeader))
            .finish()
    }
}

impl fmt::Debug for ParsedRequest {
    /// The request with credentials redacted: credential header values
    /// and any `key` query parameter appear only as fingerprints, so a
    /// failure or log line never holds the credential.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParsedRequest")
            .field("method", &self.method)
            .field("target", &fingerprint_query(&self.target))
            .field("headers", &ShownHeaders(&self.headers))
            .field("body", &self.body)
            .field("malformed", &self.malformed)
            .finish()
    }
}

impl fmt::Debug for RequestOutcome {
    /// The outcome with credentials redacted through the request.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestOutcome::Complete(request) => f.debug_tuple("Complete").field(request).finish(),
            RequestOutcome::Eof => write!(f, "Eof"),
            RequestOutcome::Stopped => write!(f, "Stopped"),
            RequestOutcome::Invalid(why) => f.debug_tuple("Invalid").field(why).finish(),
        }
    }
}

/// Whether `len` fits `cap`: equality admitted, one more refused, probed
/// with saturating arithmetic rather than an ordered comparison.
fn fits(len: usize, cap: usize) -> bool {
    len.saturating_sub(cap) == 0
}

/// One socket byte past the lookahead: the byte, the peer's close, or
/// shutdown's end.
enum ByteEnd {
    Byte(u8),
    Eof,
    Stopped,
}

fn next_byte(conn: &mut Connection, phase: &'static str) -> ByteEnd {
    if !conn.consume_check(phase) {
        return ByteEnd::Stopped;
    }
    if let Some(byte) = conn.buffered() {
        return ByteEnd::Byte(byte);
    }
    match conn.fill(phase) {
        FillOutcome::Held(_) => {
            if !conn.consume_check(phase) {
                return ByteEnd::Stopped;
            }
            match conn.buffered() {
                Some(byte) => ByteEnd::Byte(byte),
                None => ByteEnd::Eof,
            }
        }
        FillOutcome::Eof => ByteEnd::Eof,
        FillOutcome::Stopped => ByteEnd::Stopped,
    }
}

/// One `\n`-terminated line with its terminator, or why none follows.
enum LineEnd {
    Line(Vec<u8>),
    Eof,
    Stopped,
    TooLong,
}

fn take_line(conn: &mut Connection, phase: &'static str, cap: usize) -> LineEnd {
    let mut line = Vec::new();
    loop {
        match next_byte(conn, phase) {
            ByteEnd::Byte(byte) => {
                line.push(byte);
                // The cap admits `cap` bytes; the excess byte refuses.
                if !fits(line.len(), cap) {
                    return LineEnd::TooLong;
                }
                if byte == b'\n' {
                    return LineEnd::Line(line);
                }
            }
            ByteEnd::Eof => return LineEnd::Eof,
            ByteEnd::Stopped => return LineEnd::Stopped,
        }
    }
}

/// Reads one request within the connection's original budget, never
/// restarted here or by progress.
pub(crate) fn read_request(conn: &mut Connection) -> RequestOutcome {
    let limits = conn.shared.limits;
    let (method, target, headers) = match read_head(conn, &limits) {
        Ok(head) => head,
        Err(outcome) => return outcome,
    };
    // A request with any transfer coding ends in chunked, as before.
    let chunked = headers.iter().any(|(name, _)| name == "transfer-encoding");
    let (body, malformed) = if chunked {
        match read_chunked(conn, &limits) {
            Ok(decoded) => decoded,
            Err(outcome) => return outcome,
        }
    } else {
        let length = match headers.iter().find(|(name, _)| name == "content-length") {
            None => 0,
            Some((_, value)) => match value.parse::<usize>() {
                Ok(length) => length,
                Err(_) => return RequestOutcome::Invalid("content-length is not a number"),
            },
        };
        // The declared length is admitted before its allocation: one
        // excess byte refuses without reading or allocating for it.
        if !fits(length, limits.body) {
            return RequestOutcome::Invalid("the body exceeds its cap");
        }
        // Bulk reads, one bound check per fill: the check before the
        // drain stops an expired budget or a racing shutdown from
        // completing a request from bytes already held, and the fill
        // checks after each socket read, as the per-byte loop did. Past
        // the head's leftover, socket bytes land straight in the body
        // instead of transiting the lookahead one byte at a time.
        let mut body = Vec::with_capacity(length);
        while body.len() != length {
            if !conn.consume_check("to read the fixed body") {
                return RequestOutcome::Stopped;
            }
            let remaining = length - body.len();
            conn.drain_ahead(&mut body, remaining);
            // Complete without reading past the body: one more fill
            // would wait on the peer instead of answering.
            if body.len() == length {
                break;
            }
            // Recomputed past the drain: the cap above counts bytes the
            // drain already moved, so reusing it reads past the body
            // into pipelined bytes, and the next subtraction panics.
            let remaining = length - body.len();
            match conn.fill_into(&mut body, remaining, "to read the fixed body") {
                FillOutcome::Held(_) => {}
                FillOutcome::Eof => return RequestOutcome::Eof,
                FillOutcome::Stopped => return RequestOutcome::Stopped,
            }
        }
        // Pipelined bytes the capped reads left on the socket would
        // reset the close instead of delivering the reply: discard one
        // read of what already arrived, never waiting for more. Only
        // past a declared body, on the path that closes after replying:
        // a zero-length read (a CONNECT, or a reused connection's empty
        // body) leaves socket bytes the tunnel or the session still owns.
        if length != 0 {
            conn.drain_socket();
        }
        (body, None)
    };
    RequestOutcome::Complete(ParsedRequest {
        method,
        target,
        headers,
        body,
        malformed,
    })
}

/// One head: the method, the target and the headers, in received order
/// with lowercase names. A version is not required past the two tokens.
type Head = (String, String, Vec<(String, String)>);
fn read_head(conn: &mut Connection, limits: &Limits) -> Result<Head, RequestOutcome> {
    let raw = match take_line(conn, "to read the request line", limits.line) {
        LineEnd::Line(raw) => raw,
        LineEnd::Eof => return Err(RequestOutcome::Eof),
        LineEnd::Stopped => return Err(RequestOutcome::Stopped),
        LineEnd::TooLong => {
            return Err(RequestOutcome::Invalid("the request line exceeds its cap"));
        }
    };
    let mut words = head_text(&raw)?.split_whitespace();
    let (Some(method), Some(target)) = (words.next(), words.next()) else {
        return Err(RequestOutcome::Invalid("no request line"));
    };
    let (method, target) = (method.to_owned(), target.to_owned());
    let mut headers = Vec::new();
    let mut total = 0usize;
    loop {
        let raw = match take_line(conn, "to read the headers", limits.frame) {
            LineEnd::Line(raw) => raw,
            LineEnd::Eof => return Err(RequestOutcome::Eof),
            LineEnd::Stopped => return Err(RequestOutcome::Stopped),
            LineEnd::TooLong => {
                return Err(RequestOutcome::Invalid("a header line exceeds its cap"));
            }
        };
        let text = head_text(&raw)?;
        if text.is_empty() {
            break;
        }
        // Each content line counts with its terminator.
        total = total.saturating_add(raw.len());
        if !fits(total, limits.headers) {
            return Err(RequestOutcome::Invalid("the headers exceed their cap"));
        }
        let (name, value) = match text.split_once(':') {
            Some(header) => header,
            None => return Err(RequestOutcome::Invalid("a header has no colon")),
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        if !fits(headers.len(), limits.header_count) {
            return Err(RequestOutcome::Invalid("there are too many headers"));
        }
    }
    Ok((method, target, headers))
}

/// A head line's text, without its line terminator.
fn head_text(raw: &[u8]) -> Result<&str, RequestOutcome> {
    let mut text = raw;
    while let Some(stripped) = text.strip_suffix(b"\n".as_slice()) {
        text = stripped;
    }
    while let Some(stripped) = text.strip_suffix(b"\r".as_slice()) {
        text = stripped;
    }
    match std::str::from_utf8(text) {
        Ok(text) => Ok(text),
        Err(_) => Err(RequestOutcome::Invalid("the request head is not UTF-8")),
    }
}

/// One chunk-size line's end.
enum SizeEnd {
    /// More chunks: this many data bytes.
    Size(u64),
    /// The zero chunk: trailers follow.
    Zero,
    /// Framing failed: the decoded prefix stands.
    Malformed(Malformed),
    /// Shutdown ended the wait.
    Stopped,
}

/// A chunked body: each chunk's bytes in order, extensions and trailers
/// dropped. Malformed framing keeps what decoded before it; only
/// shutdown stops the read. `Err` is shutdown alone.
fn read_chunked(
    conn: &mut Connection,
    limits: &Limits,
) -> Result<(Vec<u8>, Option<Malformed>), RequestOutcome> {
    let mut decoded = Vec::new();
    loop {
        let size = match framing_size(conn, limits)? {
            SizeEnd::Size(size) => size,
            SizeEnd::Zero => {
                let malformed = read_trailers(conn, limits)?;
                return Ok((decoded, malformed));
            }
            SizeEnd::Malformed(why) => return Ok((decoded, Some(why))),
            SizeEnd::Stopped => return Err(RequestOutcome::Stopped),
        };
        // The cumulative decoded bytes are admitted before a byte is
        // read for them: an enormous declared size refuses at once.
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        if !fits(decoded.len().saturating_add(size), limits.body) {
            return Ok((decoded, Some("the body exceeds its cap")));
        }
        let mut left = size;
        while left != 0 {
            match next_byte(conn, "to read chunk data") {
                ByteEnd::Byte(byte) => {
                    decoded.push(byte);
                    left -= 1;
                }
                ByteEnd::Eof => return Ok((decoded, Some(TRUNCATED))),
                ByteEnd::Stopped => return Err(RequestOutcome::Stopped),
            }
        }
        match take_line(conn, "to read chunk framing", limits.frame) {
            LineEnd::Line(raw) if matches!(raw.as_slice(), [b'\r', b'\n']) => {}
            LineEnd::Line(_) => {
                return Ok((decoded, Some("a chunk's data is not followed by CRLF")));
            }
            LineEnd::Eof => return Ok((decoded, Some(TRUNCATED))),
            LineEnd::Stopped => return Err(RequestOutcome::Stopped),
            LineEnd::TooLong => return Ok((decoded, Some("a chunk line exceeds its cap"))),
        }
    }
}

/// One chunk-size line: the size, the zero end, or the failure its prefix
/// carries. A close is truncation, not an end: the framing is malformed
/// with what decoded.
fn framing_size(conn: &mut Connection, limits: &Limits) -> Result<SizeEnd, RequestOutcome> {
    let raw = match take_line(conn, "to read a chunk size", limits.frame) {
        LineEnd::Line(raw) => raw,
        LineEnd::Eof => return Ok(SizeEnd::Malformed(TRUNCATED)),
        LineEnd::Stopped => return Ok(SizeEnd::Stopped),
        LineEnd::TooLong => return Ok(SizeEnd::Malformed("a chunk line exceeds its cap")),
    };
    let Some(content) = raw.strip_suffix(b"\r\n".as_slice()) else {
        return Ok(SizeEnd::Malformed(
            "a chunked framing line does not end in CRLF",
        ));
    };
    let text = match std::str::from_utf8(content) {
        Ok(text) => text,
        Err(_) => return Ok(SizeEnd::Malformed("a chunk size is not a hex number")),
    };
    let size = text.split(';').next().unwrap_or("").trim();
    match u64::from_str_radix(size, 16) {
        Ok(0) => Ok(SizeEnd::Zero),
        Ok(size) => Ok(SizeEnd::Size(size)),
        Err(_) => Ok(SizeEnd::Malformed("a chunk size is not a hex number")),
    }
}

/// The trailers ending a zero chunk, read and dropped: `None` for a
/// clean end, the failure otherwise. `Err` is shutdown alone.
fn read_trailers(
    conn: &mut Connection,
    limits: &Limits,
) -> Result<Option<Malformed>, RequestOutcome> {
    let mut count = 0usize;
    let mut total = 0usize;
    loop {
        let raw = match take_line(conn, "to read the trailers", limits.frame) {
            LineEnd::Line(raw) => raw,
            LineEnd::Eof => return Ok(Some(TRUNCATED)),
            LineEnd::Stopped => return Err(RequestOutcome::Stopped),
            LineEnd::TooLong => return Ok(Some("a chunk line exceeds its cap")),
        };
        if matches!(raw.as_slice(), [b'\r', b'\n']) {
            return Ok(None);
        }
        if raw.strip_suffix(b"\r\n".as_slice()).is_none() {
            return Ok(Some("a chunked framing line does not end in CRLF"));
        }
        total = total.saturating_add(raw.len());
        if !fits(total, limits.trailers) {
            return Ok(Some("the trailers exceed their cap"));
        }
        count += 1;
        if !fits(count, limits.trailer_count) {
            return Ok(Some("there are too many trailers"));
        }
    }
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
