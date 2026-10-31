//! Lowercase hex and SHA-256 as lowercase hex, each built into one
//! `String`; and percent-decoding, shared by the provider and extension
//! download paths so the two never drift.

use ring::digest::{SHA256, digest};

/// Two lowercase hex digits per byte, no prefix.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 * bytes.len());
    for &byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            out.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    out
}

/// The SHA-256 of `bytes` as 64 lowercase hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(digest(&SHA256, bytes).as_ref())
}

/// Percent-decodes `raw`: each `%` followed by two hex digits, in either
/// case, becomes its byte; anything else stays literal. The bytes are
/// consumed through the iterator, with no index arithmetic: a `%` peeks at
/// the next two bytes without taking them, so a cut-short or non-hex
/// follower is re-scanned as a fresh byte, ready to start another escape.
pub fn percent_decode(raw: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            decoded.push(byte);
            continue;
        }
        let mut look = bytes.clone();
        match (look.next(), look.next()) {
            (Some(high), Some(low)) => match (hex_val(high), hex_val(low)) {
                (Some(high), Some(low)) => {
                    decoded.push((high << 4) + low);
                    bytes.next();
                    bytes.next();
                }
                // Not hex: the `%` stays literal.
                (None, _) | (_, None) => {
                    decoded.push(byte);
                }
            },
            // Cut short: the `%` stays literal.
            _ => {
                decoded.push(byte);
            }
        }
    }
    decoded
}

/// A hex digit's value, in either case.
fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "hex_tests.rs"]
mod tests;
