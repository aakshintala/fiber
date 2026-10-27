//! Errors returned while a session's loop runs.

use contract::{ErrorCode, SessionId};

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
    /// A `blocking` `session_start` hook failed at a start, a resume or a
    /// rewind, so the session did not start (`docs/extensions.md`,
    /// "When a hook fails"). Boxed: a second inline `Failure` variant
    /// would push the error past clippy's large-`Err` bound.
    #[error("{}", .0.message)]
    HookFailed(Box<contract::shapes::Failure>),
    /// The old log's last line is not `rewound` naming the new session,
    /// so there is no point for it to continue from.
    #[error("Session `{}` was not rewound for session `{}`.", old.0, new.0)]
    NotRewound {
        /// The session asked to continue.
        old: SessionId,
        /// The session that would continue it.
        new: SessionId,
    },
    /// The inbox closed while a move still listed running jobs, so their
    /// ends can never arrive: the move fails without `moved` instead of
    /// hiding the unfinished jobs (`docs/events.md`, "Change workspace").
    #[error("the inbox closed with jobs still running")]
    InboxClosed,
}

impl Error {
    /// The stable code a consumer switches on (`docs/errors.md`, "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Log(error) => error.code(),
            Self::Unreadable(_) | Self::NoSessionStarted => ErrorCode::LogCorrupt,
            Self::RepositoryCode(failure) => failure.code.clone(),
            Self::HookFailed(failure) => failure.code.clone(),
            Self::NotRewound { .. } => ErrorCode::InvalidArguments,
            Self::InboxClosed => ErrorCode::IoFailed,
        }
    }
}
