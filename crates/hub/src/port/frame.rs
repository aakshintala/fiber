//! RFC 6455 frames on the hub's websocket port (`docs/invocation.md`,
//! "What the hub speaks"): reading masked client frames and writing
//! unmasked server frames. A fragmented text message assembles before it
//! is read as text; a control frame between its fragments answers at once.

use std::io::{BufRead, Read};

/// The largest first message, in payload bytes summed over fragments: past
/// it the hub closes 1009. Authenticated messages have no cap, so their
/// reader carries no limit here.
pub(crate) const FIRST_MESSAGE_MAX: usize = 65_536;

/// Payload bytes read per round: a header's length never sizes an
/// allocation, only bounds how much is taken per round.
const CHUNK: usize = 4_096;

/// What one [`Reader::next`] call decided.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Incoming {
    /// A whole text message, assembled over its fragments.
    Text(String),
    /// A ping and its payload, to answer with a pong carrying the same.
    Ping(Vec<u8>),
    /// A pong, ignored.
    Pong,
    /// A close: the client's code and reason, or nothing when it sent no
    /// payload, to echo before shutting the stream down.
    Close(Option<(u16, String)>),
    /// A protocol failure: the close code to send before shutting down.
    Fail(u16),
    /// The stream ended, in a header or a payload: the connection is gone.
    Gone,
}

/// Reads masked client frames through one [`BufRead`], so bytes buffered
/// past a frame are never lost. A fragmented text message assembles across
/// calls; control frames between its fragments answer at once.
pub(crate) struct Reader {
    open: Option<Vec<u8>>,
    total: usize,
    limit: usize,
}

impl Reader {
    /// Reads messages of at most `limit` payload bytes, summed over
    /// fragments; past it [`Incoming::Fail`] carries 1009.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            open: None,
            total: 0,
            limit,
        }
    }

    /// Reads one frame: a whole text message, a control frame's answer, a
    /// protocol failure's close code, or [`Incoming::Gone`] at EOF.
    pub(crate) fn next(&mut self, read: &mut impl BufRead) -> Incoming {
        loop {
            let mut head = [0u8; 2];
            if read.read_exact(&mut head).is_err() {
                return Incoming::Gone;
            }
            let [b0, b1] = head;
            if b0 & 0x70 != 0 {
                // RSV bits must be clear: no extensions are negotiated.
                return Incoming::Fail(1002);
            }
            let opcode = b0 & 0x0F;
            match opcode {
                0x0 | 0x1 | 0x2 | 0x8 | 0x9 | 0xA => {}
                _ => return Incoming::Fail(1002),
            }
            if b1 & 0x80 == 0 {
                // Client frames must be masked.
                return Incoming::Fail(1002);
            }
            let len = match b1 & 0x7F {
                126 => {
                    let mut pair = [0u8; 2];
                    if read.read_exact(&mut pair).is_err() {
                        return Incoming::Gone;
                    }
                    let len = u16::from_be_bytes(pair);
                    // Lengths use the minimal encoding.
                    if len < 126 {
                        return Incoming::Fail(1002);
                    }
                    u64::from(len)
                }
                127 => {
                    let mut wide = [0u8; 8];
                    if read.read_exact(&mut wide).is_err() {
                        return Incoming::Gone;
                    }
                    let len = u64::from_be_bytes(wide);
                    // No message is that long: the top bit is never set.
                    if len & (1 << 63) != 0 {
                        return Incoming::Fail(1002);
                    }
                    if len < 65_536 {
                        return Incoming::Fail(1002);
                    }
                    len
                }
                short => u64::from(short),
            };
            let Ok(len) = usize::try_from(len) else {
                return Incoming::Fail(1009);
            };
            let mut key = [0u8; 4];
            if read.read_exact(&mut key).is_err() {
                return Incoming::Gone;
            }
            if opcode >= 0x8 {
                return control(opcode, b0, len, key, read);
            }
            if opcode == 0x0 && self.open.is_none() {
                // A continuation opens no message.
                return Incoming::Fail(1002);
            }
            if opcode != 0x0 && self.open.is_some() {
                // A new message opens while one is still fragmented.
                return Incoming::Fail(1002);
            }
            if opcode == 0x2 {
                // A binary message is not text.
                return Incoming::Fail(1003);
            }
            if opcode == 0x1 {
                self.open = Some(Vec::new());
            }
            let mut remaining = len;
            let mut masks = key.iter().cycle();
            while remaining > 0 {
                let want = remaining.min(CHUNK);
                let mut fragment = Vec::new();
                let mut take = read.by_ref().take(want as u64);
                match take.read_to_end(&mut fragment) {
                    Ok(n) if n == want => {}
                    Ok(_) | Err(_) => return Incoming::Gone,
                }
                for (byte, mask) in fragment.iter_mut().zip(&mut masks) {
                    *byte ^= *mask;
                }
                self.total = self.total.saturating_add(fragment.len());
                if self.total > self.limit {
                    // Past the cap the hub closes 1009.
                    return Incoming::Fail(1009);
                }
                if let Some(open) = self.open.as_mut() {
                    open.extend_from_slice(&fragment);
                }
                remaining -= fragment.len();
            }
            if b0 & 0x80 == 0 {
                // More fragments follow.
                continue;
            }
            let Some(bytes) = self.open.take() else {
                return Incoming::Fail(1002);
            };
            self.total = 0;
            match String::from_utf8(bytes) {
                Ok(text) => return Incoming::Text(text),
                Err(_) => return Incoming::Fail(1007),
            }
        }
    }
}

/// Reads one control frame's payload: FIN must be set, and 125 bytes is
/// the most a control frame carries.
fn control(opcode: u8, b0: u8, len: usize, key: [u8; 4], read: &mut impl BufRead) -> Incoming {
    if b0 & 0x80 == 0 {
        return Incoming::Fail(1002);
    }
    if len > 125 {
        return Incoming::Fail(1002);
    }
    let mut payload = [0u8; 125];
    let Some(slot) = payload.get_mut(..len) else {
        return Incoming::Fail(1002);
    };
    if read.read_exact(slot).is_err() {
        return Incoming::Gone;
    }
    for (byte, mask) in slot.iter_mut().zip(key.iter().cycle()) {
        *byte ^= *mask;
    }
    match opcode {
        0x8 => close(slot),
        0x9 => Incoming::Ping(slot.to_vec()),
        0xA => Incoming::Pong,
        _ => Incoming::Fail(1002),
    }
}

/// Reads a close frame's payload: an empty payload carries no code, one
/// byte is a failure, and a code no endpoint may send is never echoed.
fn close(payload: &[u8]) -> Incoming {
    let mut bytes = payload.iter();
    let (Some(first), Some(second)) = (bytes.next(), bytes.next()) else {
        if payload.is_empty() {
            return Incoming::Close(None);
        }
        return Incoming::Fail(1002);
    };
    let code = u16::from_be_bytes([*first, *second]);
    if !close_code_valid(code) {
        return Incoming::Fail(1002);
    }
    let reason: Vec<u8> = bytes.copied().collect();
    match String::from_utf8(reason) {
        Ok(reason) => Incoming::Close(Some((code, reason))),
        Err(_) => Incoming::Fail(1007),
    }
}

/// Whether an endpoint may send `code` in a close frame: 1000 to 1003,
/// 1007 to 1014, or 3000 to 4999. The codes 1004, 1005, 1006 and 1015 must
/// never be set, and 1016 to 2999 are unassigned.
fn close_code_valid(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

/// Writes `payload` as one unmasked text frame with FIN set.
pub(crate) fn text_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(10 + payload.len());
    push_header(&mut out, 0x81, payload.len());
    out.extend_from_slice(payload);
    out
}

/// Writes `payload` as one unmasked pong frame.
pub(crate) fn pong_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(10 + payload.len());
    push_header(&mut out, 0x8A, payload.len());
    out.extend_from_slice(payload);
    out
}

/// Writes a close frame carrying `code` and `reason`.
pub(crate) fn close_frame(code: u16, reason: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(10 + 2 + reason.len());
    push_header(&mut out, 0x88, 2 + reason.len());
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(reason.as_bytes());
    out
}

/// Echoes a client's close: its code with an empty reason, or an empty
/// payload when it sent none.
pub(crate) fn echo_close(code: Option<u16>) -> Vec<u8> {
    match code {
        Some(code) => close_frame(code, ""),
        None => vec![0x88, 0x00],
    }
}

/// Pushes one frame header: `first` holds FIN and the opcode, and the
/// length follows in its minimal encoding.
fn push_header(out: &mut Vec<u8>, first: u8, len: usize) {
    out.push(first);
    if len < 126 {
        out.push(u8::try_from(len).unwrap_or(u8::MAX));
    } else if let Ok(short) = u16::try_from(len) {
        out.push(126);
        out.extend_from_slice(&short.to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&u64::try_from(len).unwrap_or(u64::MAX).to_be_bytes());
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
