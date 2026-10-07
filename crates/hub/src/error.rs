//! Why the hub could not start (`docs/errors.md`, "Before a session
//! exists"). Each case maps to its stable code with no wildcard arm.

use std::io;
use std::path::PathBuf;

use contract::ErrorCode;
use contract::shapes::Failure;

/// A failure of [`crate::serve`] before the hub listens.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// `FIBER_HOME` is too long for `run/hub` to fit the platform's socket
    /// path (`docs/state.md`, "Sockets").
    #[error("FIBER_HOME is too long for the hub's socket path, which must fit in {max} bytes.")]
    HomeTooLong {
        /// The longest socket path the platform binds.
        max: usize,
    },
    /// Creating, locking or binding under `run/` failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path the failure concerns.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// Reading the configuration failed: the failure as the caller built it.
    #[error("{}", .0.message)]
    Configure(Failure),
}

impl StartError {
    /// The code from `docs/errors.md`.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::HomeTooLong { .. } => ErrorCode::Usage,
            Self::Io { .. } => ErrorCode::IoFailed,
            Self::Configure(failure) => failure.code.clone(),
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
