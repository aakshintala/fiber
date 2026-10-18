//! Running sessions reached through their sockets (`docs/tools.md`,
//! "Messaging other sessions"). `doors` supplies the client and `tools`
//! consumes it without depending on the socket implementation.

use std::path::PathBuf;

use crate::SessionId;
use crate::events::SessionStatus;
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

/// The socket client behind the session tools.
pub trait Sessions: Send + Sync {
    /// Fiber home's run directory, with a trailing `/`.
    fn scope(&self) -> PathBuf;

    /// Lists accepted session sockets in id order, waiting at most two
    /// seconds in all for their latest summaries.
    fn list(&self, cancel: &dyn Cancel) -> Result<Vec<Running>, ListError>;
}
