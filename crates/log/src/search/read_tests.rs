//! Tests for the cancelling reader.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::Read;

use contract::tool::Cancel;
use fakes::CancelToken;

use super::*;

#[test]
fn a_read_fails_once_the_call_is_cancelled() {
    let cancel = CancelToken::new();
    let mut reader = Cancelling::new(&b"abcdef"[..], &cancel);
    let mut buf = [0_u8; 3];
    assert_eq!(reader.read(&mut buf).unwrap(), 3);
    cancel.cancel();
    assert!(cancel.is_cancelled());
    let error = reader.read(&mut buf).unwrap_err();
    assert_ne!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(!reader.eof());
}

#[test]
fn the_end_is_noted_only_when_a_read_returns_it() {
    let cancel = CancelToken::new();
    let mut reader = Cancelling::new(&b"abc"[..], &cancel);
    let mut buf = [0_u8; 3];
    assert_eq!(reader.read(&mut buf).unwrap(), 3);
    assert!(!reader.eof());
    // An empty buffer reads nothing without reaching the end.
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    assert!(!reader.eof());
    assert_eq!(reader.read(&mut buf).unwrap(), 0);
    assert!(reader.eof());
}
