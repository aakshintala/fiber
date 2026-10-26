//! One deadline for a test's waits (`docs/testing.md`, "Waits and timeouts"):
//! every wait takes what remains of it, so a second wait gets only what the
//! first left, and every crate's tests share it. The implementation lives in
//! [`support::deadline`]; this module re-exports it so `fakes::Deadline` and
//! `fakes::deadline::Deadline` keep resolving.

pub use support::deadline::{BUDGET, CLEANUP, Deadline, WAITS};
