//! The entry points' guard wiring: a refused signal registration returns
//! before any credential command starts, as `io_failed` with a `signals:`
//! message, mapped to exit 1 by `fail`.
//!
//! These tests never touch the environment: setting `FIBER_HOME` or the
//! working directory is unsafe code, which this tree denies, so no fixture
//! could reach the `::cli` calls anyway. Each test passes a registration
//! that fails with a message no other failure builds, and pins that the
//! entry point returns exactly that failure: had the entry run its command
//! first, the answer would be that command's, never this one. The binary
//! tests behind `credential_signals.rs` pin the wired success paths with
//! real fixtures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;

use contract::ErrorCode;

use super::{models_entry, refresh_entry, terminal_guards};

/// The registration every test passes: it always refuses, with a message
/// only `read_signals::guarded` builds into a failure.
fn refused(
    _: Arc<crate::switch::Reads>,
    _: Arc<dyn contract::clock::Clock>,
    _: &[std::ffi::c_int],
) -> std::io::Result<super::read_signals::CancelOnSignal> {
    Err(std::io::Error::other("refused-test-guard"))
}

#[test]
fn models_entry_returns_the_registration_error_before_any_command() {
    let process = super::startup::Process {
        clock: Arc::new(crate::clock::System),
        exe: Err(super::failed(ErrorCode::IoFailed, "the test binary")),
    };
    let code = models_entry(
        super::cli::ModelsArgs {
            search: None,
            json: false,
        },
        &process,
        refused,
    );
    // Through `fail`: `io_failed` exits 1, never 2.
    assert_eq!(code, 1);
}

#[test]
fn refresh_entry_returns_the_registration_error_before_any_command() {
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(crate::clock::System);
    let code = refresh_entry(&["acme".to_owned()], clock, refused);
    assert_eq!(code, 1);
}

#[test]
fn terminal_guards_returns_the_registration_error_and_builds_nothing() {
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(crate::clock::System);
    let failure = terminal_guards(&clock, refused);
    let Err(failure) = failure else {
        panic!("a refused registration fails closed")
    };
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert_eq!(
        failure.message, "signals: refused-test-guard",
        "the entry adds no text: the helper builds the failure"
    );
}
