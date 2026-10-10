//! Shared helpers for the doors crate-integration tests and the unit tests
//! that include them: one temporary home, one hang bound, one hello line.

#![allow(dead_code, reason = "each test file uses its own subset")]

use std::path::PathBuf;
use std::time::Duration;

/// One hang bound for every wait: twice it plus run time stays under
/// nextest's 120 s kill (`docs/testing.md`, "Waits and timeouts").
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// A temporary directory a test owns: the path, held until drop.
pub(crate) struct Temp(
    pub(crate) PathBuf,
    #[allow(dead_code, reason = "Drop removes the directory")] fakes::TempDir,
);

impl Temp {
    /// A fresh directory under the system temporary directory.
    pub(crate) fn new() -> Self {
        let held = fakes::TempDir::new("fd");
        let dir = held.path().to_path_buf();
        Self(dir, held)
    }
}

/// The `hub_hello` line a hub speaks first, without its newline.
pub(crate) fn hello_line() -> Vec<u8> {
    br#"{"kind":"hub_hello","ts":1,"schema_version":1,"payload":{"fiber_version":"0.0.0"}}"#
        .to_vec()
}
