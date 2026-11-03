//! The `probe` jig through its built binary: the probe's own checks and
//! the binary's behaviour against fakes. The unit tests live in
//! `tests/support/probe.rs`, included below, so they run inside this
//! binary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers outside a #[test]; a failure is the test's"
)]

#[path = "support/probe.rs"]
mod support;
