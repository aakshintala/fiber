//! [`hex`](super::hex) writes two lowercase digits per byte into one
//! `String`; [`sha256_hex`](super::sha256_hex) is the SHA-256 in that form.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use super::{hex, sha256_hex};

#[test]
fn hex_of_empty_is_empty() {
    assert_eq!(hex(&[]), "");
}

#[test]
fn hex_pads_zeroes_orders_nibbles_and_lowercases() {
    assert_eq!(hex(&[0x0f, 0xf0, 0x00, 0xab, 0xff]), "0ff000abff");
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
