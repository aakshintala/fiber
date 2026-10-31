//! [`hex`](super::hex) writes two lowercase digits per byte into one
//! `String`; [`sha256_hex`](super::sha256_hex) is the SHA-256 in that form.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use super::{hex, percent_decode, sha256_hex};

#[test]
fn hex_of_empty_is_empty() {
    assert_eq!(hex(&[]), "");
}

#[test]
fn hex_reserves_two_bytes_per_input_byte() {
    assert_eq!(hex(&[1, 2, 3]).capacity(), 6);
}

#[test]
fn hex_of_every_byte_matches_the_format_oracle() {
    let bytes: Vec<u8> = (0..=255).collect();
    let oracle: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let got = hex(&bytes);
    assert_eq!(got.len(), 512);
    assert_eq!(got, oracle);
}

#[test]
fn sha256_hex_of_empty_matches_the_nist_vector() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn sha256_hex_of_abc_matches_the_nist_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn percent_decode_decodes_exact_bytes() {
    let cases: Vec<(&[u8], &[u8])> = vec![
        (b"", b""),
        (b"plain", b"plain"),
        (b"a%2Fb", b"a/b"),
        (b"a%2fb", b"a/b"),
        (b"%41%42", b"AB"),
        (b"%%41", b"%A"),
        (b"%4%41", b"%4A"),
        (b"%%%41", b"%%A"),
        (b"%zz", b"%zz"),
        (b"%2", b"%2"),
        (b"%", b"%"),
        (b"100%sure", b"100%sure"),
        (b"%FF", &[0xFF]),
        (b"a%20b", b"a b"),
    ];
    for (raw, expected) in cases {
        assert_eq!(percent_decode(raw), expected.to_vec(), "decoding {raw:?}");
    }
}
