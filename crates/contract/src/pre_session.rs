//! The line a process prints when it fails before any session exists
//! (`docs/errors.md`, "Before a session exists").

use serde::ser::Error as _;
use serde::{Deserialize, Serialize, Serializer};

use crate::SCHEMA_VERSION;
use crate::events::FiberExited;
use crate::shapes::Failure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Kind {
    #[serde(rename = "fiber_exited")]
    FiberExited,
}

/// The `fiber_exited` line of a process that failed before any session
/// existed. It is not an [`Envelope`](crate::Envelope): it has no
/// `session_id`, `ts` or `seq` (`docs/events.md`, "The envelope"). Its
/// `payload` is the `fiber_exited` payload with `exit_code` and `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreSessionExit {
    kind: Kind,
    /// The schema version this line is written against.
    pub schema_version: u32,
    /// The exit code and why the process failed. Its keys serialize sorted,
    /// as an envelope's payload does.
    #[serde(serialize_with = "sorted")]
    pub payload: FiberExited,
}

fn sorted<S: Serializer>(payload: &FiberExited, serializer: S) -> Result<S::Ok, S::Error> {
    let value = serde_json::to_value(payload).map_err(S::Error::custom)?;
    value.serialize(serializer)
}

impl PreSessionExit {
    /// The line for a process that exits with `exit_code` because of `error`.
    pub fn new(exit_code: i32, error: Failure) -> Self {
        let payload = FiberExited {
            exit_code,
            final_message: None,
            error: Some(error),
            suspended_on: None,
            questions: None,
        };
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
