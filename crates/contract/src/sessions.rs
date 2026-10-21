//! Running sessions reached through their sockets (`docs/tools.md`,
//! "Messaging other sessions"): `session_list` and `session_message`.
//! `doors` supplies the client and `tools` consumes it without depending
//! on the socket implementation.

use std::path::PathBuf;

use crate::SessionId;
use crate::events::SessionStatus;
use crate::shapes::Failure;
use crate::tool::Cancel;

/// A running session whose socket accepted a connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
    /// The session's id, taken from its socket's name.
    pub id: SessionId,
    /// Its latest summary, or none when it did not answer in time.
    pub status: Option<SessionStatus>,
}

/// Why listing sessions could not finish.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListError {
    /// A directory could not be read or a socket could not be created.
    #[error("Could not list sessions at {path}: {message}")]
    Io {
        /// The directory or socket path involved.
        path: PathBuf,
        /// The operating system's error.
        message: String,
    },
    /// The call was cancelled.
    #[error("Listing sessions was cancelled.")]
    Cancelled,
}

/// Why a session message was not delivered.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MessageError {
    /// No running session has the id: wrong shape, no socket, refused, or closed before answering.
    #[error("no running session has the id")]
    Unreachable,
    /// The target answered `command_rejected`: its code and message, unchanged.
    #[error("the target rejected the message")]
    Rejected(Failure),
    /// A socket could not be created.
    #[error("cannot create socket {}: {message}", path.display())]
    Io {
        /// The socket or directory that could not be used.
        path: PathBuf,
        /// The operating system's error.
        message: String,
    },
    /// The call was cancelled.
    #[error("the call was cancelled")]
    Cancelled,
}

/// The socket client behind the session tools.
pub trait Sessions: Send + Sync {
    /// Fiber home's run directory, with a trailing `/`.
    fn scope(&self) -> PathBuf;

    /// Lists accepted session sockets in id order, waiting at most two
    /// seconds in all for their latest summaries.
    fn list(&self, cancel: &dyn Cancel) -> Result<Vec<Running>, ListError>;

    /// Sends `text` to `to` as a session message from `from`, whose parent
    /// is `parent` when the sender has one, on one connection closed
    /// straight after the answer.
    fn message(
        &self,
        to: &SessionId,
        from: &SessionId,
        parent: Option<&SessionId>,
        text: &str,
        cancel: &dyn Cancel,
    ) -> Result<(), MessageError>;
}
