//! A minimal PNG encoder for the logo's iTerm2 image: 8-bit RGBA,
//! one IDAT of uncompressed deflate blocks (`docs/tui.md`, "The logo").
//! The logo is small and rare (once per theme per home visit), so the
//! encoder favors obviously-correct stored blocks over size; no image
//! dependency enters the tui crate for it.

/// The longest one stored deflate block holds.
const BLOCK_CAP: usize = 65535;

/// The CRC-32 of `bytes` (ISO 3309): the PNG chunk check.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The Adler-32 of `bytes` (RFC 1950): the zlib stream check.
fn adler32(bytes: &[u8]) -> u32 {
    const BASE: u32 = 65_521;
    let mut low = 1u32;
    let mut high = 0u32;
    for byte in bytes {
        low = (low + u32::from(*byte)) % BASE;
        high = (high + low) % BASE;
    }
    (high << 16) | low
}

/// Encodes `rgba` (`width` by `height`, 8-bit RGBA row-major) as a PNG:
/// the signature, an IHDR, one IDAT of stored deflate blocks split at
/// 65535 bytes, and IEND.
pub(crate) fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = width.saturating_mul(4);
    let mut raw = Vec::with_capacity(height.saturating_mul(stride.saturating_add(1)) as usize);
    for row in 0..height {
        raw.push(0);
        let start = row.saturating_mul(stride) as usize;
        let end = start.saturating_add(stride as usize);
        raw.extend_from_slice(rgba.get(start..end).unwrap_or_default());
    }
    // One zlib stream of stored blocks: each block carries its length
    // and its complement, so a flipped bit fails the read.
    let mut deflated = vec![0x78, 0x01];
    let mut blocks = raw.chunks(BLOCK_CAP).peekable();
    while let Some(block) = blocks.next() {
        let last = u8::from(blocks.peek().is_none());
        let len = u16::try_from(block.len()).unwrap_or(u16::MAX);
        deflated.push(last);
        deflated.extend_from_slice(&len.to_le_bytes());
        deflated.extend_from_slice(&(!len).to_le_bytes());
        deflated.extend_from_slice(block);
    }
    deflated.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &deflated);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Appends one chunk: its length, type, data and CRC.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut checked = kind.to_vec();
    checked.extend_from_slice(data);
    out.extend_from_slice(&crc32(&checked).to_be_bytes());
}

#[cfg(test)]
#[path = "png_tests.rs"]
mod tests;
