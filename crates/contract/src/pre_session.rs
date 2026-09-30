//! The line a process prints when it fails before any session exists
//! (`docs/errors.md`, "Before a session exists").

use serde::{Deserialize, Serialize};

use crate::SCHEMA_VERSION;
use crate::shapes::Failure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Kind {
    #[serde(rename = "fiber_exited")]
    FiberExited,
}

/// The `fiber_exited` line of a process that failed before any session
/// existed. It is not an [`Envelope`](crate::Envelope): it has no
/// `session_id`, `ts`, `seq` or `payload` (`docs/events.md`, "The envelope").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreSessionExit {
    kind: Kind,
    /// The schema version this line is written against.
    pub schema_version: u32,
    /// The process's exit code.
    pub exit_code: i32,
    /// Why the process failed.
    pub error: Failure,
}

impl PreSessionExit {
    /// The line for a process that exits with `exit_code` because of `error`.
    pub fn new(exit_code: i32, error: Failure) -> Self {
        Self {
            kind: Kind::FiberExited,
            schema_version: SCHEMA_VERSION,
            exit_code,
            error,
        }
    }
}

#[cfg(test)]
#[path = "pre_session_tests.rs"]
mod tests;
