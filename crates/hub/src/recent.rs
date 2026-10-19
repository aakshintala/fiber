//! `recent.jsonl` at the top of Fiber home (`docs/state.md`, "Recently
//! exited sessions"): one JSON line per session that exited. A session
//! appends its own row as it exits; the hub appends a crashed session's.
//! Nothing rewrites the file, and every reader skips a row whose session
//! directory is gone.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;

use contract::ErrorCode;
use contract::SessionId;
use contract::clock::Clock;
use contract::events::SessionStatus;
use serde::{Deserialize, Serialize};

/// The rows `recent` answers with per page.
pub(crate) const RECENT_PAGE: usize = 50;

/// The newest rows the hub reads at start.
pub(crate) const RECENT_KEEP: usize = 100;

/// How a session's process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Left {
    /// Its log ends in `fiber_exited` or `rewound`.
    Exited,
    /// Anything else: the process died.
    Crashed,
}

/// One line of `recent.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentRow {
    /// The session.
    pub session_id: SessionId,
    /// When it exited, in milliseconds since the epoch.
    pub ts: u64,
    /// The project's key: the name of its `projects/<key>` directory.
    pub project: String,
    /// The workspace path.
    pub workspace: String,
    /// The session's name, or `""` when it never had a status.
    pub name: String,
    /// How it ended.
    pub how: Left,
    /// What it stopped on: its last `session_status` payload, absent when
    /// it never wrote one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SessionStatus>,
}

impl RecentRow {
    /// Whether the session exited waiting on a person: the hub lists it
    /// as waiting (`docs/invocation.md`, "Lifecycle").
    pub(crate) fn waiting(&self) -> bool {
        self.how == Left::Exited && self.status.as_ref().is_some_and(is_waiting)
    }

    /// Whether the session was a delegate: its status names a parent.
    /// Delegates are never in the feed, so `recent` skips them too.
    pub(crate) fn delegate(&self) -> bool {
        self.status.as_ref().is_some_and(is_delegate)
    }

    /// Whether the session's directory is still there.
    pub(crate) fn dir_exists(&self, home: &Path) -> bool {
        log::dir_exists(&log::session_dir(home, &self.project, &self.session_id))
    }
}

/// Whether `status` is waiting on a person.
pub(crate) fn is_waiting(status: &SessionStatus) -> bool {
    matches!(status.state, contract::events::SessionState::Waiting { .. })
}

/// Whether `status` names a parent: the session is a delegate.
pub(crate) fn is_delegate(status: &SessionStatus) -> bool {
    status.parent.is_some()
}

/// What can go wrong appending an exited row: the session's log is
/// unaffected, only its `recent.jsonl` listing.
#[derive(Debug, thiserror::Error)]
pub enum RecentError {
    /// Writing `recent.jsonl` failed.
    #[error("{}: {source}", path.display())]
    Write {
        /// The file the row could not be appended to.
        path: PathBuf,
        /// Why the append failed.
        source: io::Error,
    },
}

impl RecentError {
    /// The code from `docs/errors.md`: a filesystem failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Write { .. } => ErrorCode::IoFailed,
        }
    }
}

/// Appends `row` to `home`'s `recent.jsonl` as one write of one line on a
/// file opened for append, created mode 0600 (`docs/state.md`, "Concurrent
/// access").
pub fn append(home: &Path, row: &RecentRow) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(row)?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path(home))?;
    file.write_all(&bytes)
}

/// Every row in file order; a line that does not parse is skipped, and a
/// missing file is empty.
pub(crate) fn read_all(home: &Path) -> Vec<RecentRow> {
    let Ok(text) = fs::read_to_string(path(home)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The newest row of each session, newest first.
pub(crate) fn newest_first(rows: Vec<RecentRow>) -> Vec<RecentRow> {
    let mut seen = BTreeSet::new();
    rows.into_iter()
        .rev()
        .filter(|row| seen.insert(row.session_id.clone()))
        .collect()
}

/// Why a page could not be answered.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PageError {
    /// `before` names no session in the list.
    UnknownBefore,
}

/// Every exited session, newest first, newest row per session only:
/// delegates' rows and rows whose directory is gone are skipped; with
/// `project`, only that key.
pub(crate) fn listed(home: &Path, project: Option<&str>) -> Vec<RecentRow> {
    newest_first(read_all(home))
        .into_iter()
        .filter(|row| project.is_none_or(|key| row.project == key))
        .filter(|row| !row.delegate())
        .filter(|row| row.dir_exists(home))
        .collect()
}

/// One page of exited sessions and the query's total, from one read of
/// `recent.jsonl`: a row written mid-answer cannot make them disagree.
/// The page starts after `before`'s row; the total counts every row the
/// pages walk, whatever `before` and the page hold.
pub(crate) fn page_with_total(
    home: &Path,
    before: Option<&str>,
    project: Option<&str>,
    running: &BTreeSet<String>,
) -> Result<(Vec<RecentRow>, usize), PageError> {
    let listed = listed(home, project);
    let start = match before {
        None => 0,
        Some(before) => {
            listed
                .iter()
                .position(|row| row.session_id.0 == before)
                .ok_or(PageError::UnknownBefore)?
                + 1
        }
    };
    let total = listed
        .iter()
        .filter(|row| !running.contains(&row.session_id.0))
        .count();
    let rows = listed
        .into_iter()
        .skip(start)
        .filter(|row| !running.contains(&row.session_id.0))
        .take(RECENT_PAGE)
        .collect();
    Ok((rows, total))
}

/// The rows the hub seeds its feed from at start: of the newest
/// [`RECENT_KEEP`] rows, each session's newest, when it is crashed or
/// exited waiting, not a delegate, and its directory is still there.
pub(crate) fn seeds(home: &Path) -> Vec<RecentRow> {
    let rows = match fs::File::open(path(home)) {
        Ok(file) => tail_with(file),
        Err(_) => Vec::new(),
    };
    newest_first(rows)
        .into_iter()
        .filter(|row| !row.delegate())
        .filter(|row| row.how == Left::Crashed || row.waiting())
        .filter(|row| row.dir_exists(home))
        .collect()
}

/// The newest [`RECENT_KEEP`] parsed rows of an open `recent.jsonl`, in
/// file order, newest last: read backward from the tail, so the head is
/// never touched. A line that does not parse is skipped and does not
/// count toward the window; a final line with no newline is a write in
/// progress and is ignored. An empty file or any seek or read error is
/// empty. The seam the counted-reader test drives.
fn tail_with(file: impl Read + Seek) -> Vec<RecentRow> {
    let mut file = file;
    let end = match file.seek(SeekFrom::End(0)) {
        Ok(0) | Err(_) => return Vec::new(),
        Ok(end) => end,
    };
    let (newest_first, _) = super::prompt_history::read_back(
        &mut file,
        end,
        super::prompt_history::CHUNK,
        RECENT_KEEP,
        |line| serde_json::from_slice(line).ok(),
    )
    .unwrap_or_default();
    newest_first.into_iter().rev().collect()
}

/// How many directory lookups `feed` made: every `follow` connects
/// first, so a stale socket leaves this at 0. Test-only: nextest runs
/// each test in its own process, so the count is race-free.
#[cfg(test)]
pub(crate) static LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// How many logged-directory lookups resume, rejoin and the granted
/// callers made. Test-only, as above.
#[cfg(test)]
pub(crate) static LOGGED_LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// Finds session `id`'s directory under any project: its project key
/// and the directory, over [`log::find_session`]. The feed's lookup:
/// a session that crashed before writing a line still gets its crashed
/// row.
pub(crate) fn lookup(home: &Path, id: &SessionId) -> Option<(String, PathBuf)> {
    #[cfg(test)]
    LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    log::find_session(home, id)
}

/// Finds session `id`'s logged directory under any project: its project
/// key and the directory, over [`log::find_logged_session`]. The
/// lookup resume, rejoin and the granted callers share, so a log-less
/// directory cannot mask a later log.
pub(crate) fn lookup_logged(home: &Path, id: &SessionId) -> Option<(String, PathBuf)> {
    #[cfg(test)]
    LOGGED_LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    log::find_logged_session(home, id)
}

fn path(home: &Path) -> PathBuf {
    home.join("recent.jsonl")
}

/// Appends `dir`'s exited row to `home`'s `recent.jsonl`: the session's
/// last `session_status` and the clock's time, under its exited name.
/// Writes nothing and returns `Ok(())` when `dir` is not a directory, so
/// a session deleted before it exits leaves nothing behind.
pub fn append_exited(
    home: &Path,
    dir: &Path,
    workspace: &Path,
    status: Option<SessionStatus>,
    clock: &dyn Clock,
) -> Result<(), RecentError> {
    if !log::dir_exists(dir) {
        return Ok(());
    }
    let name = |path: Option<&Path>| {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let row = RecentRow {
        session_id: SessionId(name(Some(dir))),
        ts: contract::clock::wall_ms(clock.wall()),
        // `projects/<key>/sessions/<id>`: the key names the grandparent.
        project: name(dir.parent().and_then(Path::parent)),
        workspace: workspace.to_string_lossy().into_owned(),
        name: status
            .as_ref()
            .map(|status| status.name.clone())
            .unwrap_or_default(),
        how: Left::Exited,
        status,
    };
    // A row that cannot be written loses only the listing, not the log.
    append(home, &row).map_err(|source| RecentError::Write {
        path: path(home),
        source,
    })
}

#[cfg(test)]
#[path = "recent_tests.rs"]
mod tests;
