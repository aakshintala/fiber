//! Tests for the logo's PNG encoder: exact bytes from an independent
//! oracle (Python's `zlib`/`binascii`), never the encoder itself.

use super::{adler32, crc32, encode_rgba};

/// The check functions match their published vectors.
#[test]
fn checks_match_their_published_vectors() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
}

/// One red pixel: the exact bytes, oracle-computed.
#[test]
fn one_red_pixel_encodes_exactly() {
    #[rustfmt::skip]
    let want: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
        0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
        0x89, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41,
        0x54, 0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff,
        0x00, 0xff, 0x00, 0x00, 0xff, 0x05, 0x00, 0x01,
        0xff, 0xfa, 0x5c, 0x88, 0xd1, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
        0x82,
    ];
    assert_eq!(encode_rgba(1, 1, &[255, 0, 0, 255]), want);
}

/// A 3-by-2 image: filter bytes start each row, exact bytes.
#[test]
fn three_by_two_encodes_each_row_with_its_filter_byte() {
    let mut rgba = Vec::new();
    for value in 0..6u8 {
        rgba.extend_from_slice(&[value, value.saturating_add(1), value.saturating_add(2), 255]);
    }
    #[rustfmt::skip]
    let want: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
        0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x9d, 0x74, 0x66,
        0x1a, 0x00, 0x00, 0x00, 0x25, 0x49, 0x44, 0x41,
        0x54, 0x78, 0x01, 0x01, 0x1a, 0x00, 0xe5, 0xff,
        0x00, 0x00, 0x01, 0x02, 0xff, 0x01, 0x02, 0x03,
        0xff, 0x02, 0x03, 0x04, 0xff, 0x00, 0x03, 0x04,
        0x05, 0xff, 0x04, 0x05, 0x06, 0xff, 0x05, 0x06,
        0x07, 0xff, 0x47, 0x3c, 0x06, 0x3a, 0xd1, 0x0c,
        0x0a, 0xd5, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
        0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    assert_eq!(encode_rgba(3, 2, &rgba), want);
}

/// Past 65535 raw bytes the IDAT splits into two stored blocks: the
/// first non-final at the cap, the second final with the rest.
#[test]
fn past_the_cap_the_idat_splits_into_two_stored_blocks() {
    let width = 200u32;
    let height = 82u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for at in 0..width * height {
        rgba.extend_from_slice(&[
            (at % 251) as u8,
            ((at * 7) % 251) as u8,
            ((at * 13) % 251) as u8,
            255,
        ]);
    }
    let png = encode_rgba(width, height, &rgba);
    // The signature, then the IHDR, then the IDAT.
    assert_eq!(&png[..8], &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    let idat_len = u32::from_be_bytes(png[33..37].try_into().expect("IDAT length")) as usize;
    // 82 rows of 801 bytes split into 65535 and 147, each with its
    // 5-byte stored header, under the 2-byte zlib header and the
    // 4-byte Adler check.
    assert_eq!(idat_len, 2 + (5 + 65535) + (5 + 147) + 4);
    let idat = &png[41..41 + idat_len];
    assert_eq!(&idat[..2], &[0x78, 0x01]);
    // The first block is non-final at exactly the cap.
    assert_eq!(&idat[2..7], &[0x00, 0xff, 0xff, 0x00, 0x00]);
    // The second block is final with the remaining 147 bytes.
    let second = 2 + 5 + 65535;
    assert_eq!(&idat[second..second + 5], &[0x01, 0x93, 0x00, 0x6c, 0xff]);
    // Reassembling the blocks gives every filter byte and pixel back.
    let mut raw = Vec::new();
    raw.extend_from_slice(&idat[7..7 + 65535]);
    raw.extend_from_slice(&idat[second + 5..second + 5 + 147]);
    assert_eq!(raw.len(), 82 * (1 + 200 * 4));
    assert!(raw.iter().step_by(801).all(|byte| *byte == 0));
}
