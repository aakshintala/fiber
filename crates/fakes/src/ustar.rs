//! A plain-ustar writer and gzip for release fixtures, so no test depends on
//! the host `tar`'s format (`docs/releasing.md`, "What a release
//! publishes").

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test helper; a failure is the test's"
)]

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;

/// One 512-byte ustar header with the `ustar\0` magic, version `00` and a
/// correct checksum. `kind` is the type flag, such as `b'0'` for a file,
/// `b'5'` for a directory or `b'2'` for a symlink to `link`. A `name` over
/// 100 bytes is split at a `/` into the `prefix` and `name` fields.
///
/// # Panics
///
/// When `name` fits neither field split, or `link` is over 100 bytes.
pub fn header(name: &str, kind: u8, size: u64, mode: u32, link: &str) -> [u8; 512] {
    let mut block = [0u8; 512];
    let (prefix, name) = split(name);
    put(&mut block, 0, 100, name.as_bytes());
    put(&mut block, 100, 8, format!("{mode:07o}\0").as_bytes());
    put(&mut block, 108, 8, b"0000000\0");
    put(&mut block, 116, 8, b"0000000\0");
    put(&mut block, 124, 12, format!("{size:011o}\0").as_bytes());
    put(&mut block, 136, 12, b"00000000000\0");
    put(&mut block, 156, 1, &[kind]);
    put(&mut block, 157, 100, link.as_bytes());
    put(&mut block, 257, 6, b"ustar\0");
    put(&mut block, 263, 2, b"00");
    put(&mut block, 345, 155, prefix.as_bytes());
    checksum(&mut block);
    block
}

/// Recomputes a header's checksum after a test changed its bytes: the sum
/// of every byte, with the checksum field itself counted as spaces.
pub fn checksum(header: &mut [u8; 512]) {
    put(header, 148, 8, b"        ");
    let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    put(header, 148, 8, format!("{sum:06o}\0 ").as_bytes());
}

/// A plain-ustar stream: each header then its data padded to 512 bytes,
/// then two zero blocks.
pub fn archive(members: &[([u8; 512], &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (header, data) in members {
        out.extend_from_slice(header);
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(512), 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

/// `bytes` gzip-compressed with flate2.
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// `bytes`' SHA-256 as lowercase hex, as a release's `.sha256` file holds it.
pub fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes `bytes` at `at` in a field `width` wide.
fn put(block: &mut [u8; 512], at: usize, width: usize, bytes: &[u8]) {
    assert!(
        bytes.len() <= width,
        "{} bytes in a {width}-byte field",
        bytes.len()
    );
    block
        .get_mut(at..at + bytes.len())
        .expect("a field inside the header")
        .copy_from_slice(bytes);
}

/// `(prefix, name)`: the name alone when it fits, else split at the last `/`
/// that fits both fields.
fn split(path: &str) -> (&str, &str) {
    if path.len() <= 100 {
        return ("", path);
    }
    for (i, _) in path.match_indices('/').rev() {
        let (prefix, rest) = path.split_at(i);
        let name = rest.get(1..).unwrap_or_default();
        if prefix.len() <= 155 && name.len() <= 100 {
            return (prefix, name);
        }
    }
    panic!("`{path}` fits no ustar prefix and name");
}

#[cfg(test)]
#[path = "ustar_tests.rs"]
mod tests;
