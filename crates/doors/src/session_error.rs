//! Why a session boundary call failed: prompting, sockets, subscribing,
//! watching, and worktree isolation (`docs/errors.md`, "Where a code comes
//! from"). Each case maps to its stable code with no wildcard arm, so a new
//! case does not compile until it has one.

use std::io;
use std::path::PathBuf;

use contract::shapes::Failure;
use contract::{ErrorCode, SCHEMA_VERSION, SessionId};

/// Why a session boundary call failed: `prompt` and `no_prompt`, the
/// session socket, the subscribe handshake, and worktree isolation. A
/// failure forwarded from another crate's value keeps that value.
#[derive(thiserror::Error)]
pub enum SessionError {
    /// No prompt was supplied on the arguments or stdin.
    #[error("No prompt. Run `fiber ask \"<prompt>\"` or `fiber ask < <file>`.")]
    NoPrompt,
    /// Stdin was read but is not UTF-8 text.
    #[error("stdin is not UTF-8 text.")]
    StdinNotUtf8,
    /// Stdin could not be read.
    #[error("stdin could not be read: {source}")]
    StdinUnreadable {
        /// Why the read failed.
        #[source]
        source: io::Error,
    },
    /// `FIBER_HOME` is too long for a session socket path to fit.
    #[error(
        "FIBER_HOME is too long: a session's socket path must fit in {max} bytes. Set FIBER_HOME to a shorter path."
    )]
    SocketPathTooLong {
        /// The longest socket path the platform binds.
        max: usize,
    },
    /// A connect reached the session socket: another session is running.
    #[error("{}: another session is running", socket.display())]
    SocketHeld {
        /// The live session socket.
        socket: PathBuf,
    },
    /// A session socket call failed: the full operating-system message.
    #[error("{}: {source}", path.display())]
    SocketIo {
        /// The path the call was for.
        path: PathBuf,
        /// Why the call failed.
        #[source]
        source: io::Error,
    },
    /// A worktree lock call failed: the lock file and the kind only, never
    /// the operating system's message.
    #[error("{}: {}.", path.display(), source.kind())]
    SharedIo {
        /// The lock file the call was for.
        path: PathBuf,
        /// Why the call failed.
        #[source]
        source: io::Error,
    },
    /// The subscribe reader side did not clone.
    #[error("{}: {source}", socket.display())]
    SubscribeClone {
        /// The session socket being subscribed to.
        socket: PathBuf,
        /// Why the clone failed.
        #[source]
        source: io::Error,
    },
    /// A command could not be written as JSON.
    #[error("a command could not be written as JSON: {source}")]
    CommandEncode {
        /// Why the write failed.
        #[source]
        source: serde_json::Error,
    },
    /// A turn line could not be written to stdout.
    #[error("stdout could not be written: {source}")]
    StdoutFailed {
        /// Why the write failed.
        #[source]
        source: io::Error,
    },
    /// The session rejected a command, with the wire code and message.
    #[error("{message}")]
    Rejected {
        /// The rejection code read off the wire.
        code: ErrorCode,
        /// The rejection message read off the wire.
        message: String,
    },
    /// The session runs another schema version.
    #[error(
        "session {} runs schema version {version}, this Fiber runs schema version {}; close the session or let it exit, then resume",
        id.0,
        SCHEMA_VERSION
    )]
    VersionMismatch {
        /// The session that answered.
        id: SessionId,
        /// The schema version it runs.
        version: u64,
    },
    /// The session ended before its turn completed.
    #[error("session {} ended before its turn completed", id.0)]
    SessionEnded {
        /// The session that ended.
        id: SessionId,
    },
    /// A session thread could not start.
    #[error("cannot start a thread: {source}")]
    ThreadSpawn {
        /// Why the spawn failed.
        #[source]
        source: io::Error,
    },
    /// Making the session's worktree failed; the sentence is the site's.
    #[error("{message}")]
    WorktreeFailed {
        /// A safe sentence describing the failure, never git's output.
        message: String,
        /// What went wrong; omitted from `Debug`, never from `source()`.
        #[source]
        source: worktree::Error,
    },
    /// Reading or holding the session's worktree failed; the sentence is
    /// the site's.
    #[error("{message}")]
    WorktreeAccess {
        /// A safe sentence describing the failure, never git's output.
        message: String,
        /// What went wrong; omitted from `Debug`, never from `source()`.
        #[source]
        source: worktree::Error,
    },
    /// The recorded repository is gone, so the worktree cannot be remade.
    #[error(
        "The repository {} is gone, so the worktree {} cannot be made again.",
        repository.display(),
        path.display()
    )]
    RepositoryGone {
        /// The recorded git common directory.
        repository: PathBuf,
        /// The recorded worktree path.
        path: PathBuf,
    },
    /// A worktree lock path was absent, its wait was exhausted, or its
    /// type could not be read.
    #[error("{}: {kind}.", path.display())]
    ShareFailed {
        /// Whichever path the site formats: the worktree or its lock.
        path: PathBuf,
        /// The kind the site read.
        kind: io::ErrorKind,
    },
    /// The worktree is in use by another process.
    #[error(
        "The worktree {} is in use by another process, so Fiber did not make it again.",
        path.display()
    )]
    WorktreeInUse {
        /// The worktree another process holds.
        path: PathBuf,
    },
    /// A foreign failure value, forwarded as data: the code is the
    /// caller's. Only for values built outside `doors`.
    #[error("{}", .0.message)]
    Forwarded(Failure),
}

impl SessionError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm. `Rejected` carries the wire code, `Forwarded` the
    /// caller's, and `WorktreeFailed` its worktree source's code.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NoPrompt => ErrorCode::Usage,
            Self::StdinNotUtf8 => ErrorCode::Usage,
            Self::StdinUnreadable { .. } => ErrorCode::IoFailed,
            Self::SocketPathTooLong { .. } => ErrorCode::Usage,
            Self::SocketHeld { .. } => ErrorCode::SessionHeld,
            Self::SocketIo { .. } => ErrorCode::IoFailed,
            Self::SharedIo { .. } => ErrorCode::IoFailed,
            Self::SubscribeClone { .. } => ErrorCode::IoFailed,
            Self::CommandEncode { .. } => ErrorCode::IoFailed,
            Self::StdoutFailed { .. } => ErrorCode::IoFailed,
            Self::Rejected { code, .. } => code.clone(),
            Self::VersionMismatch { .. } => ErrorCode::SessionHeld,
            Self::SessionEnded { .. } => ErrorCode::Closing,
            Self::ThreadSpawn { .. } => ErrorCode::IoFailed,
            Self::WorktreeFailed { source, .. } => source.code(),
            Self::WorktreeAccess { .. } => ErrorCode::IoFailed,
            Self::RepositoryGone { .. } => ErrorCode::IoFailed,
            Self::ShareFailed { .. } => ErrorCode::IoFailed,
            Self::WorktreeInUse { .. } => ErrorCode::IoFailed,
            Self::Forwarded(failure) => failure.code.clone(),
        }
    }
}

impl From<SessionError> for Failure {
    /// The contract record of a case: its code and its sentence, with no
    /// retry and no provider. A forwarded failure returns unchanged.
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Forwarded(failure) => failure,
            SessionError::NoPrompt
            | SessionError::StdinNotUtf8
            | SessionError::StdinUnreadable { .. }
            | SessionError::SocketPathTooLong { .. }
            | SessionError::SocketHeld { .. }
            | SessionError::SocketIo { .. }
            | SessionError::SharedIo { .. }
            | SessionError::SubscribeClone { .. }
            | SessionError::CommandEncode { .. }
            | SessionError::StdoutFailed { .. }
            | SessionError::Rejected { .. }
            | SessionError::VersionMismatch { .. }
            | SessionError::SessionEnded { .. }
            | SessionError::ThreadSpawn { .. }
            | SessionError::WorktreeFailed { .. }
            | SessionError::WorktreeAccess { .. }
            | SessionError::RepositoryGone { .. }
            | SessionError::ShareFailed { .. }
            | SessionError::WorktreeInUse { .. } => Failure {
                code: error.code(),
                message: error.to_string(),
                retry_after_ms: None,
                provider: None,
            },
        }
    }
}

impl From<&SessionError> for Failure {
    /// The contract record of a case, as [`From<SessionError>`] builds it:
    /// a forwarded failure returns as a copy of itself.
    fn from(error: &SessionError) -> Self {
        match error {
            SessionError::Forwarded(failure) => failure.clone(),
            SessionError::NoPrompt
            | SessionError::StdinNotUtf8
            | SessionError::StdinUnreadable { .. }
            | SessionError::SocketPathTooLong { .. }
            | SessionError::SocketHeld { .. }
            | SessionError::SocketIo { .. }
            | SessionError::SharedIo { .. }
            | SessionError::SubscribeClone { .. }
            | SessionError::CommandEncode { .. }
            | SessionError::StdoutFailed { .. }
            | SessionError::Rejected { .. }
            | SessionError::VersionMismatch { .. }
            | SessionError::SessionEnded { .. }
            | SessionError::ThreadSpawn { .. }
            | SessionError::WorktreeFailed { .. }
            | SessionError::WorktreeAccess { .. }
            | SessionError::RepositoryGone { .. }
            | SessionError::ShareFailed { .. }
            | SessionError::WorktreeInUse { .. } => Failure {
                code: error.code(),
                message: error.to_string(),
                retry_after_ms: None,
                provider: None,
            },
        }
    }
}

/// Every case shows its safe fields, but `WorktreeFailed` and
/// `WorktreeAccess` omit their `worktree::Error` source, whose `Git`
/// stderr can carry credentials (`docs/code-quality.md`, "Errors"). Their
/// `Display` and `Error::source()` still carry the accepted chain.
impl std::fmt::Debug for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPrompt => write!(f, "NoPrompt"),
            Self::StdinNotUtf8 => write!(f, "StdinNotUtf8"),
            Self::StdinUnreadable { source } => f
                .debug_struct("StdinUnreadable")
                .field("source", source)
                .finish(),
            Self::SocketPathTooLong { max } => f
                .debug_struct("SocketPathTooLong")
                .field("max", max)
                .finish(),
            Self::SocketHeld { socket } => f
                .debug_struct("SocketHeld")
                .field("socket", socket)
                .finish(),
            Self::SocketIo { path, source } => f
                .debug_struct("SocketIo")
                .field("path", path)
                .field("source", source)
                .finish(),
            Self::SharedIo { path, source } => f
                .debug_struct("SharedIo")
                .field("path", path)
                .field("source", source)
                .finish(),
            Self::SubscribeClone { socket, source } => f
                .debug_struct("SubscribeClone")
                .field("socket", socket)
                .field("source", source)
                .finish(),
            Self::CommandEncode { source } => f
                .debug_struct("CommandEncode")
                .field("source", source)
                .finish(),
            Self::StdoutFailed { source } => f
                .debug_struct("StdoutFailed")
                .field("source", source)
                .finish(),
            Self::Rejected { code, message } => f
                .debug_struct("Rejected")
                .field("code", code)
                .field("message", message)
                .finish(),
            Self::VersionMismatch { id, version } => f
                .debug_struct("VersionMismatch")
                .field("id", id)
                .field("version", version)
                .finish(),
            Self::SessionEnded { id } => f.debug_struct("SessionEnded").field("id", id).finish(),
            Self::ThreadSpawn { source } => f
                .debug_struct("ThreadSpawn")
                .field("source", source)
                .finish(),
            Self::WorktreeFailed { message, .. } => f
                .debug_struct("WorktreeFailed")
                .field("message", message)
                .field("code", &self.code())
                .finish(),
            Self::WorktreeAccess { message, .. } => f
                .debug_struct("WorktreeAccess")
                .field("message", message)
                .finish(),
            Self::RepositoryGone { repository, path } => f
                .debug_struct("RepositoryGone")
                .field("repository", repository)
                .field("path", path)
                .finish(),
            Self::ShareFailed { path, kind } => f
                .debug_struct("ShareFailed")
                .field("path", path)
                .field("kind", kind)
                .finish(),
            Self::WorktreeInUse { path } => {
                f.debug_struct("WorktreeInUse").field("path", path).finish()
            }
            Self::Forwarded(failure) => f.debug_tuple("Forwarded").field(failure).finish(),
        }
    }
}

#[cfg(test)]
#[path = "session_error_tests.rs"]
mod tests;
