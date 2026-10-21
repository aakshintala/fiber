//! AWS event-stream framing (`application/vnd.amazon.eventstream`), written
//! here rather than taken from a crate (`docs/dependencies.md`, "Written
//! ourselves"): the binary frames AWS Bedrock streams a reply in
//! (`docs/model-routing.md`, "Protocols and providers"). Each message is a
//! 12-byte prelude (the message's total length, its headers' length and the
//! prelude's CRC-32), the headers, the payload, and the CRC-32 of everything
//! before it. Every length and checksum is big-endian, and the checksum is
//! CRC-32/ISO-HDLC, the one zlib computes.

use std::io::{ErrorKind, Read};

use crate::Error;

/// The prelude's length: total length, headers length and prelude CRC.
const PRELUDE: usize = 12;
/// The prelude plus the message CRC that ends every message.
const OVERHEAD: usize = PRELUDE + 4;
/// The longest message AWS frames, 16 MiB, as its SDKs bound it.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
/// The longest headers section, 128 KiB, as AWS's SDKs bound it.
const MAX_HEADERS: usize = 128 * 1024;

/// The header value type that holds a UTF-8 string.
const STRING: u8 = 7;

/// One message: its string headers, in the order they came, and its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message {
    /// Each string-valued header as `(name, value)`. Headers of the other
    /// value types are read past and left out: AWS frames a model's reply
    /// with string headers only.
    pub(crate) headers: Vec<(String, String)>,
    /// The payload bytes.
    pub(crate) payload: Vec<u8>,
}

impl Message {
    /// The string header `name`, if the message carries one.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    /// The failure an `exception` or `error` message reports, or `None` for
    /// any other message. An exception names itself in `:exception-type` and
    /// carries its text as a JSON payload's `message`; an error names itself
    /// in `:error-code` and its text in `:error-message`.
    pub(crate) fn failure(&self) -> Option<Error> {
        match self.header(":message-type")? {
            "exception" => {
                let message = serde_json::from_slice::<serde_json::Value>(&self.payload)
                    .ok()
                    .and_then(|payload| {
                        ["message", "Message"]
                            .iter()
                            .find_map(|key| payload.get(*key)?.as_str().map(str::to_owned))
                    })
                    .unwrap_or_else(|| String::from_utf8_lossy(&self.payload).into_owned());
                Some(Error::ReplyFailed {
                    code: self.header(":exception-type").map(str::to_owned),
                    message,
                })
            }
            "error" => Some(Error::ReplyFailed {
                code: self.header(":error-code").map(str::to_owned),
                message: self.header(":error-message").unwrap_or("").to_owned(),
            }),
            _ => None,
        }
    }
}

/// Reads `stream` and passes each message to `on_message` until it returns
/// `true` or the stream ends. A message the stream ends inside is never
/// passed on, as an unfinished server-sent event is not. A length out of
/// range, a checksum that does not match, or headers that cannot be read
/// fail with `stream_incomplete`: past them nothing in the stream can be
/// trusted.
pub(crate) fn read(
    mut stream: impl Read,
    mut on_message: impl FnMut(Message) -> Result<bool, Error>,
) -> Result<(), Error> {
    loop {
        let mut prelude = [0_u8; PRELUDE];
        if !fill(&mut stream, &mut prelude)? {
            return Ok(());
        }
        let [t0, t1, t2, t3, h0, h1, h2, h3, c0, c1, c2, c3] = prelude;
        let total = length(u32::from_be_bytes([t0, t1, t2, t3]))?;
        let headers_length = length(u32::from_be_bytes([h0, h1, h2, h3]))?;
        if !(OVERHEAD..=MAX_MESSAGE).contains(&total) {
            return Err(broken(format!("a message is {total} bytes long")));
        }
        if headers_length > MAX_HEADERS || headers_length > total - OVERHEAD {
            return Err(broken(format!(
                "a message's headers are {headers_length} bytes long"
            )));
        }
        let (lengths, prelude_crc) = prelude.split_at(8);
        let mut crc = Crc::new();
        crc.update(lengths);
        if crc.value() != u32::from_be_bytes([c0, c1, c2, c3]) {
            return Err(broken("a message's prelude failed its checksum".into()));
        }
        crc.update(prelude_crc);
        let mut rest = vec![0_u8; total - PRELUDE];
        if !fill(&mut stream, &mut rest)? {
            return Ok(());
        }
        let Some((body, sent)) = rest.split_last_chunk::<4>() else {
            return Err(broken("a message has no checksum".into()));
        };
        crc.update(body);
        if crc.value() != u32::from_be_bytes(*sent) {
            return Err(broken("a message failed its checksum".into()));
        }
        let (headers, payload) = body
            .split_at_checked(headers_length)
            .ok_or_else(|| broken("a message's headers overrun it".into()))?;
        let message = Message {
            headers: headers_of(headers)?,
            payload: payload.to_vec(),
        };
        if on_message(message)? {
            return Ok(());
        }
    }
}

/// Fills `buf` from `stream`: `false` when the stream ends first, so the
/// message it ended inside is dropped.
fn fill(stream: &mut impl Read, buf: &mut [u8]) -> Result<bool, Error> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..)
        && !rest.is_empty()
    {
        match stream.read(rest) {
            Ok(0) => return Ok(false),
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Connection(e.to_string())),
        }
    }
    Ok(true)
}

/// A frame length as a `usize`.
fn length(value: u32) -> Result<usize, Error> {
    usize::try_from(value).map_err(|_| broken(format!("a message is {value} bytes long")))
}

/// The failure for a frame that cannot be trusted.
fn broken(why: String) -> Error {
    Error::StreamIncomplete(format!("{why} in its AWS event-stream framing"))
}

/// The string headers of one headers section. Each header is its name's
/// length (one byte), its name, its value type (one byte) and its value,
/// whose size the type fixes or a two-byte length gives.
fn headers_of(mut bytes: &[u8]) -> Result<Vec<(String, String)>, Error> {
    let malformed = || broken("a message's headers are malformed".into());
    let mut headers = Vec::new();
    while let Some((&name_length, rest)) = bytes.split_first() {
        let (name, rest) = rest
            .split_at_checked(usize::from(name_length))
            .ok_or_else(malformed)?;
        let name = std::str::from_utf8(name).map_err(|_| malformed())?;
        let (&kind, rest) = rest.split_first().ok_or_else(malformed)?;
        let fixed = match kind {
            // `true` and `false` carry no value.
            0 | 1 => Some(0),
            2 => Some(1),
            3 => Some(2),
            4 => Some(4),
            // A long and a timestamp are eight bytes.
            5 | 8 => Some(8),
            // A UUID is sixteen bytes.
            9 => Some(16),
            // Bytes and a string carry a two-byte length.
            6 | STRING => None,
            _ => return Err(malformed()),
        };
        let (value, rest) = match fixed {
            Some(size) => rest.split_at_checked(size).ok_or_else(malformed)?,
            None => {
                let (size, rest) = rest.split_first_chunk::<2>().ok_or_else(malformed)?;
                rest.split_at_checked(usize::from(u16::from_be_bytes(*size)))
                    .ok_or_else(malformed)?
            }
        };
        if kind == STRING {
            let value = std::str::from_utf8(value).map_err(|_| malformed())?;
            headers.push((name.to_owned(), value.to_owned()));
        }
        bytes = rest;
    }
    Ok(headers)
}

/// CRC-32/ISO-HDLC: the reflected polynomial `0xEDB88320`, starting from
/// and finishing with every bit set, as zlib computes it.
struct Crc(u32);

/// One entry per byte value: the register after shifting that byte through.
const TABLE: [u32; 256] = table();

const fn table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut byte = 0_u32;
    while byte < 256 {
        let mut register = byte;
        let mut bit = 0;
        while bit < 8 {
            register = if register & 1 == 1 {
                0xEDB8_8320 ^ (register >> 1)
            } else {
                register >> 1
            };
            bit += 1;
        }
        #[allow(
            clippy::indexing_slicing,
            reason = "byte stays below 256, the table's length"
        )]
        {
            table[byte as usize] = register;
        }
        byte += 1;
    }
    table
}

impl Crc {
    fn new() -> Self {
        Self(u32::MAX)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let [low, ..] = (self.0 ^ u32::from(byte)).to_le_bytes();
            let entry = TABLE.get(usize::from(low)).copied().unwrap_or(0);
            self.0 = entry ^ (self.0 >> 8);
        }
    }

    fn value(&self) -> u32 {
        !self.0
    }
}

/// The CRC-32 of `bytes`, for tests that frame their own messages.
#[cfg(test)]
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc::new();
    crc.update(bytes);
    crc.value()
}

#[cfg(test)]
#[path = "event_stream_tests.rs"]
mod tests;
