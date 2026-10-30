//! A test's hang guard and each wait's own bound (`docs/testing.md`, "Waits
//! and timeouts"): the root deadline caps every wait, and each wait gets a
//! fresh bound made when it starts. The implementation lives in
//! [`support::deadline`]; this module re-exports it so `fakes::Deadline` and
//! `fakes::deadline::Deadline` keep resolving.

pub use support::deadline::{BUDGET, CLEANUP, Deadline, WAIT, WAITS};
