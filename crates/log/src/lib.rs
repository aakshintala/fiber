//! Owns the session directory (`docs/events.md`, "Writing" and "The session
//! directory"; `docs/state.md`). It is the only thing that opens
//! `events.jsonl`, holds `session.lock`, mints `seq` and decides when to
//! fsync, and it hands every event to whoever is watching.
//!
//! [`Log`] is the writing side, one per session process. [`read`] and
//! [`Watcher`] are the reading side, which `tui` and `doors` may use
//! (`docs/architecture.md`, "The call rules").

mod dependents;
mod export;
mod offsets;
mod rate;
mod read;
mod resolve;
mod weak_emit;
mod write;

use std::io;
use std::path::{Path, PathBuf};

use contract::{ErrorCode, SessionId};

pub use dependents::dependents;
pub use export::export;
pub use rate::Rate;
pub use read::{Injector, Lines, Watcher, lines, read};
pub use resolve::resolve;
pub use weak_emit::WeakEmit;
pub use write::Log;

/// The log's name in a session directory.
const EVENTS: &str = "events.jsonl";
/// The lock file's name in a session directory.
const LOCK: &str = "session.lock";
/// The directory for bytes too large to inline.
const ARTIFACTS: &str = "artifacts";

/// A project's key (`docs/state.md`, "Projects"): its identity path with
/// every `/` made `-`. `project` is that identity path, symlinks already
/// resolved.
pub fn project_key(project: &Path) -> String {
    project.to_string_lossy().replace('/', "-")
}

/// Where a project's session directories live in Fiber home
/// (`docs/state.md`, "Projects"): `projects/<key>/sessions`, the key from
/// [`project_key`].
pub fn sessions_dir(home: &Path, project: &Path) -> PathBuf {
    home.join("projects")
        .join(project_key(project))
        .join("sessions")
}

/// What can go wrong opening, writing or reading a session.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Another writer holds the session's lock.
    #[error("session {session} is held by {holder}; only one Fiber process may write a session")]
    Held {
        /// The session.
        session: String,
        /// Who holds it, such as `process 4242`.
        holder: String,
    },
    /// An earlier write or fsync failed, so the log refuses to write more.
    /// Reopening the session carries on from its last complete line.
    #[error(
        "session {session} stopped writing after a failed write ({cause}); reopen it to carry on"
    )]
    Poisoned {
        /// The session.
        session: String,
        /// The failure that stopped it.
        cause: String,
    },
    /// The session directory, or its log, does not exist.
    #[error("no session at {0}")]
    NotFound(PathBuf),
    /// A `--resume` selector matching more than one session.
    #[error("\"{selector}\" matches more than one session: {}", matches.join(", "))]
    Ambiguous {
        /// The selector.
        selector: String,
        /// The matching ids, sorted.
        matches: Vec<String>,
    },
    /// The export target already exists: an export writes a new directory.
    #[error("{0} already exists; export writes a new directory")]
    Exists(PathBuf),
    /// A complete line in the log is not an event line.
    #[error("{path}, line {line}: {source}")]
    Unreadable {
        /// The log.
        path: PathBuf,
        /// The line's number, from 1.
        line: usize,
        /// Why it did not parse.
        source: serde_json::Error,
    },
    /// An event could not be written as JSON.
    #[error("cannot write an event as JSON: {0}")]
    Encode(#[from] serde_json::Error),
    /// The file system refused.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
}

impl Error {
    /// The stable code a consumer switches on (`docs/errors.md`,
    /// "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Held { .. } => ErrorCode::SessionHeld,
            Self::NotFound(_) => ErrorCode::SessionNotFound,
            Self::Ambiguous { .. } | Self::Exists(_) => ErrorCode::Usage,
            // Only a failed write or fsync stops a log.
            Self::Poisoned { .. } | Self::Io { .. } => ErrorCode::IoFailed,
            Self::Unreadable { .. } | Self::Encode(_) => ErrorCode::LogCorrupt,
        }
    }
}

/// Wraps an I/O failure with the path it happened on.
fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_owned(),
        source,
    }
}

/// The session directory of `id` in `sessions`.
fn session_path(sessions: &Path, id: &SessionId) -> PathBuf {
    sessions.join(&id.0)
}
