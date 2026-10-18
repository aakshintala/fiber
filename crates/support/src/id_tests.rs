//! [`mint`](super::mint) carries its prefix, holds 16 lowercase hex digits
//! after it, and mints a fresh id on every call.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use super::mint;

#[test]
fn minted_ids_carry_their_prefix_and_sixteen_lowercase_hex_digits() {
    for prefix in ["s_", "c_", ""] {
        let id = mint(prefix);
        assert!(
            id.starts_with(prefix),
            "a minted id carries its prefix: {id}"
        );
        let digits = id.strip_prefix(prefix).unwrap_or_default();
        assert_eq!(digits.len(), 16, "a minted id holds 16 digits: {id}");
        assert!(
            digits
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "the 16 digits are lowercase hex: {id}"
        );
    }
}

#[test]
fn two_mints_differ() {
    assert_ne!(mint("s_"), mint("s_"), "two mints differ");
}
