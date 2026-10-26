//! Owns the session directory (`docs/events.md`, "Writing" and "The session
//! directory"; `docs/state.md`). It is the only thing that opens
//! `events.jsonl`, holds `session.lock`, mints `seq` and decides when to
//! fsync, and it hands every event to whoever is watching.
//!
//! [`Log`] is the writing side, one per session process. [`read`] and
//! [`Watcher`] are the reading side, which `tui` and `doors` may use
//! (`docs/architecture.md`, "The call rules"). [`crash`] installs the
//! panic hook: one crash report under `crashes/` in Fiber home before the
//! process aborts.

mod artifact;
pub mod crash;
mod delete;
mod dependents;
pub mod diag;
mod escalations;
mod export;
#[cfg(test)]
mod fixtures;
mod history;
mod media;
mod name;
mod offsets;
mod rate;
mod read;
mod resolve;
mod scan;
mod search;
mod state_fold;
mod usage;
mod weak_emit;
mod write;

#[cfg(test)]
#[path = "session_dir_tests.rs"]
mod session_dir_tests;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use contract::{ErrorCode, SessionId};

pub use artifact::{OutputRead, Refusal, Unreadable, check_artifact_path, open_artifact};
pub use delete::{delete_session, is_plain_dir};
pub use dependents::{Children, dependents};
pub use escalations::{AllowedEscalation, allowed_escalations};
pub use export::export;
pub use history::{Segment, history, history_to, last_line, last_seq};
pub use media::read_media;
pub use name::session_name;
pub use name::session_parent;
pub use rate::{Rate, RateFold};
pub use read::{
    Injector, Lines, Scan, Watcher, complete_len_of, find_logged_session, find_session, kind_of,
    last_kind, lines, lines_from, read, tail_lines,
};
pub use resolve::{most_recent, opening_ts, resolve};
pub use scan::{
    FirstLineError, Hold, MalformedWorktree, SessionLock, SessionStarted, Started, last_line_where,
    last_ts, remaining, session_bytes, started_first, started_sessions, started_sessions_in,
    try_hold, try_hold_existing,
};
pub use search::{Identity, SessionScan};
pub use state_fold::{Fold, Ruled, StateSeed, fold, fold_boundary, fold_from, seed};
pub use usage::usage;
pub use weak_emit::WeakEmit;
pub use write::{Log, Subscription, cut_joined, cut_notice};

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

/// The session directory of `id` in `project`: `home/projects/<key>/sessions/<id>`,
/// a plain join. Rows in `recent.jsonl` are written by Fiber, so no shape
/// check happens here.
pub fn session_dir(home: &Path, project: &str, id: &SessionId) -> PathBuf {
    home.join("projects")
        .join(project)
        .join("sessions")
        .join(&id.0)
}

/// Whether the session directory is still there, following links as
/// [`Path::is_dir`] does.
pub fn dir_exists(dir: &Path) -> bool {
    dir.is_dir()
}

/// Whether `dir` sits at `expected`: `dir` canonicalized inside `log`
/// equals the expected path, expressed under a canonical root with normal
/// path components. Do not canonicalize a possibly linked candidate to
/// form `expected`: that would accept a link at or above `dir`. A failed
/// canonicalization is not placed, so the caller keeps its own refusal.
pub fn dir_is_at(dir: &Path, expected: &Path) -> bool {
    std::fs::canonicalize(dir).is_ok_and(|real| real.as_path() == expected)
}

/// A session directory: `sessions/<id>`, holding `events.jsonl`,
/// `session.lock` and `artifacts/` (`docs/events.md`, "The session
/// directory"). The only shape check is [`SessionDir::require_dir`]; the
/// join itself is plain, like [`session_dir`].
pub struct SessionDir(PathBuf);

impl SessionDir {
    /// The session directory of `id` in `sessions`: `sessions/<id>`, a plain
    /// join. No existence check happens here.
    pub fn new(sessions: &Path, id: &SessionId) -> Self {
        Self(session_path(sessions, id))
    }

    /// The log in this session directory.
    pub fn events(&self) -> PathBuf {
        self.0.join(EVENTS)
    }

    /// The directory for bytes too large to inline in this session directory.
    pub fn artifacts(&self) -> PathBuf {
        self.0.join(ARTIFACTS)
    }

    /// The session directory itself, for the existing `&Path` boundaries.
    /// It is never a base for another session-path join.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Whether the session directory is still there, following links as
    /// [`Path::is_dir`] does. A missing directory, or a regular file in its
    /// place, is [`Error::NotFound`] naming the session directory; any other
    /// metadata failure is [`Error::Io`] naming it.
    pub fn require_dir(&self) -> Result<(), Error> {
        match std::fs::metadata(&self.0) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(_) => Err(Error::NotFound(self.0.clone())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(Error::NotFound(self.0.clone()))
            }
            Err(source) => Err(Error::Io {
                path: self.0.clone(),
                source,
            }),
        }
    }
}

/// Where a project's session directories live in Fiber home
/// (`docs/state.md`, "Projects"): `projects/<key>/sessions`, the key from
/// [`project_key`].
pub fn sessions_dir(home: &Path, project: &Path) -> PathBuf {
    home.join("projects")
        .join(project_key(project))
        .join("sessions")
}

/// Where the sessions of the project named by `key` live in Fiber home:
/// `projects/<key>/sessions`. `None` when `key` is not one normal path
/// component, so a path never leaves `projects/`. Callers holding only a
/// worktree path use this after reading the key off it; the join stays in
/// `log` (`docs/architecture.md`, rule 3).
pub fn sessions_dir_for_key(home: &Path, key: &str) -> Option<PathBuf> {
    // Checked before any path is built from it, so a path never leaves
    // `projects/`.
    let mut parts = Path::new(key).components();
    if !matches!(parts.next(), Some(std::path::Component::Normal(_))) || parts.next().is_some() {
        return None;
    }
    Some(sessions_dir(home, Path::new(key)))
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
    /// The session ended: nothing more is written to its log.
    #[error("session {session} has ended; nothing more is written to it")]
    Ended {
        /// The session.
        session: String,
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
    /// A history pointer names a line no log holds: a fork or rewind
    /// point past its log's end, or a chain that repeats a session
    /// (`docs/events.md`, "Rewind"). A log whose last line gives no
    /// boundary, such as a torn-only log or a last line with no `seq`,
    /// is one too.
    #[error("{path}: {reason}")]
    Pointer {
        /// The log the pointer was read from.
        path: PathBuf,
        /// Why it names no line.
        reason: String,
    },
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
            Self::Ended { .. } => ErrorCode::Closing,
            Self::NotFound(_) => ErrorCode::SessionNotFound,
            Self::Ambiguous { .. } | Self::Exists(_) => ErrorCode::Usage,
            // Only a failed write or fsync stops a log.
            Self::Poisoned { .. } | Self::Io { .. } => ErrorCode::IoFailed,
            Self::Unreadable { .. } | Self::Encode(_) | Self::Pointer { .. } => {
                ErrorCode::LogCorrupt
            }
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

/// Opens the log in the session directory `dir`: the one open every other
/// reader goes through. A missing log is [`Error::NotFound`] naming the
/// session directory; any other failure is [`Error::Io`] naming the log.
pub(crate) fn open_log(dir: &Path) -> Result<File, Error> {
    let path = dir.join(EVENTS);
    File::open(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Error::NotFound(dir.to_owned())
        } else {
            io_at(&path)(error)
        }
    })
}

/// The session directory of `id` in `sessions`.
fn session_path(sessions: &Path, id: &SessionId) -> PathBuf {
    sessions.join(&id.0)
}
