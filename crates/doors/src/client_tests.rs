//! Command-line details a socket test cannot see.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use super::is_line_ending;

#[test]
fn a_cr_and_a_lf_both_end_a_line() {
    assert!(is_line_ending(b'\n'));
    assert!(is_line_ending(b'\r'));
    assert!(!is_line_ending(b'x'));
}
