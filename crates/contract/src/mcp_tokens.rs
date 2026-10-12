//! The credential-file seam for remote MCP OAuth (`docs/mcp.md`).
use crate::ErrorCode;
use serde_json::Value;

/// Token files keyed by the SHA-256 of a canonical server URL.
pub trait TokenFiles: Send + Sync {
    /// Reads without locking or creating directories; absent files return `None`.
    fn read(&self, key: &str) -> Result<Option<Value>, TokenFileError>;
    /// Tries once to hold the credential lock; `None` means another holder owns it.
    fn try_lock(&self, key: &str) -> Result<Option<Box<dyn HeldTokenFile>>, TokenFileError>;
}
/// A credential file whose lock is released when the holder is dropped.
pub trait HeldTokenFile: Send {
    /// Re-reads the whole file under the lock.
    fn read(&self) -> Result<Option<Value>, TokenFileError>;
    /// Atomically replaces the file, with owner-only permissions.
    fn write(&self, value: &Value) -> Result<(), TokenFileError>;
}
/// Storage failures contain paths, never credential contents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenFileError {
    /// A path is unsafe to follow.
    #[error("{0}")]
    Refused(String),
    /// The record could not be read.
    #[error("{0}")]
    Unreadable(String),
    /// A write failed before its rename; the old file remains.
    #[error("{0}")]
    Unwritable(String),
    /// Directory sync failed after the rename; the new file remains.
    #[error("{0}")]
    Unsynced(String),
}
impl TokenFileError {
    /// The stable code for a credential-file failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Refused(_) | Self::Unreadable(_) | Self::Unwritable(_) | Self::Unsynced(_) => {
                ErrorCode::IoFailed
            }
        }
    }
}

#[cfg(test)]
#[path = "mcp_tokens_tests.rs"]
mod tests;
