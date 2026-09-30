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
/// `session_id`, `ts` or `seq` (`docs/events.md`, "The envelope"). Its
/// `payload` holds `error` and `exit_code`, and nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreSessionExit {
    kind: Kind,
    /// The schema version this line is written against.
    pub schema_version: u32,
    /// Why the process failed, and its exit code.
    pub payload: PreSessionPayload,
}

/// The body of a [`PreSessionExit`]. The fields are declared in sorted order,
/// as an envelope's payload keys serialize.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreSessionPayload {
    /// Why the process failed.
    pub error: Failure,
    /// The process's exit code (`docs/invocation.md`, "Lifecycle").
    pub exit_code: i32,
}

impl PreSessionExit {
    /// The line for a process that exits with `exit_code` because of `error`.
    pub fn new(exit_code: i32, error: Failure) -> Self {
        let payload = PreSessionPayload { error, exit_code };
        Self {
            kind: Kind::FiberExited,
            schema_version: SCHEMA_VERSION,
            payload,
        }
    }
}

#[cfg(test)]
#[path = "pre_session_tests.rs"]
mod tests;
