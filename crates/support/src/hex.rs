//! Lowercase hex and SHA-256 as lowercase hex, each built into one `String`.

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

#[cfg(test)]
#[path = "hex_tests.rs"]
mod tests;
