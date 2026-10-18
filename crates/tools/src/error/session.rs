//! Failures of the session tools (`session_list`, `session_message`).

use std::path::PathBuf;

use contract::ErrorCode;
use contract::SessionId;
use contract::shapes::Failure;

/// Every way `session_list` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionListError {
    /// The running sessions could not be listed.
    #[error("Could not list sessions at {}: {message}", path.display())]
    Io {
        /// The directory or socket path involved.
        path: PathBuf,
        /// The operating system's error.
        message: String,
    },
}

impl SessionListError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Io { .. } => ErrorCode::ToolError,
        }
    }
}

/// Every way `session_message` fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionMessageError {
    /// No running session has the id.
    #[error("No running session has the id {}.", id.0)]
    Unreachable {
        /// The target session's id, as the call named it.
        id: SessionId,
    },
    /// The target answered `command_rejected`: its code and message reach
    /// `Output.error` unchanged, with no retry or provider carried over.
    /// The code is the target's.
    #[error("{}", .0.message)]
    Rejected(Failure),
    /// The message could not be sent.
    #[error("Could not message session {}: {message}", id.0)]
    Io {
        /// The target session's id, as the call named it.
        id: SessionId,
        /// The operating system's error.
        message: String,
    },
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] super::ArgumentsError),
}

impl SessionMessageError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Unreachable { .. } => ErrorCode::Unreachable,
            Self::Rejected(failure) => failure.code.clone(),
            Self::Io { .. } => ErrorCode::ToolError,
            Self::Arguments(error) => error.code(),
        }
    }
}
