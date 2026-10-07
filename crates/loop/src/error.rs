//! Errors returned while a session's loop runs.

use contract::ErrorCode;

/// What stops the loop.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The session log refused a write, so the turn cannot be recorded.
    #[error(transparent)]
    Log(#[from] log::Error),
    /// A durable line's payload does not read as its kind.
    #[error("a log line does not read as its kind: {0}")]
    Unreadable(serde_json::Error),
    /// The log has no `session_started`.
    #[error("the log has no session_started")]
    NoSessionStarted,
    /// The repository's code could not be read, or a `required` item nobody
    /// approved could not be offered (`docs/extensions.md`, "Code a
    /// repository ships").
    #[error("{}", .0.message)]
    RepositoryCode(contract::shapes::Failure),
}

impl Error {
    /// The stable code a consumer switches on (`docs/errors.md`, "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Log(error) => error.code(),
            Self::Unreadable(_) | Self::NoSessionStarted => ErrorCode::LogCorrupt,
            Self::RepositoryCode(failure) => failure.code.clone(),
        }
    }
}
