//! `recent.jsonl` at the top of Fiber home (`docs/state.md`, "Recently
//! exited sessions"): one JSON line per session that exited. A session
//! appends its own row as it exits; the hub appends a crashed session's.
//! Nothing rewrites the file, and every reader skips a row whose session
//! directory is gone.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use contract::SessionId;
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

    /// Whether the session's directory is still there.
    pub(crate) fn dir_exists(&self, home: &Path) -> bool {
        session_dir(home, &self.project, &self.session_id.0).is_dir()
    }
}

/// Whether `status` is waiting on a person.
pub(crate) fn is_waiting(status: &SessionStatus) -> bool {
    matches!(status.state, contract::events::SessionState::Waiting { .. })
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

/// One page of exited sessions, newest first, newest row per session
/// only: rows whose directory is gone and sessions in `running` are
/// skipped; with `project`, only that key. The page starts after
/// `before`'s row.
pub(crate) fn page(
    home: &Path,
    before: Option<&str>,
    project: Option<&str>,
    running: &BTreeSet<String>,
) -> Result<Vec<RecentRow>, PageError> {
    let listed: Vec<RecentRow> = newest_first(read_all(home))
        .into_iter()
        .filter(|row| project.is_none_or(|key| row.project == key))
        .filter(|row| row.dir_exists(home))
        .collect();
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
    Ok(listed
        .into_iter()
        .skip(start)
        .filter(|row| !running.contains(&row.session_id.0))
        .take(RECENT_PAGE)
        .collect())
}

/// The rows the hub seeds its feed from at start: of the newest
/// [`RECENT_KEEP`] rows, each session's newest, when it is crashed or
/// exited waiting and its directory is still there.
pub(crate) fn seeds(home: &Path) -> Vec<RecentRow> {
    let rows = read_all(home);
    let kept = rows
        .into_iter()
        .rev()
        .take(RECENT_KEEP)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    newest_first(kept)
        .into_iter()
        .filter(|row| row.how == Left::Crashed || row.waiting())
        .filter(|row| row.dir_exists(home))
        .collect()
}

/// `projects/<project>/sessions/<id>` under `home`.
pub(crate) fn session_dir(home: &Path, project: &str, id: &str) -> PathBuf {
    home.join("projects")
        .join(project)
        .join("sessions")
        .join(id)
}

/// Finds session `id`'s directory under any project: its project key and
/// the directory. `None` when no project holds it.
pub(crate) fn find(home: &Path, id: &str) -> Option<(String, PathBuf)> {
    fs::read_dir(home.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let project = entry.file_name().to_string_lossy().into_owned();
            let dir = session_dir(home, &project, id);
            dir.is_dir().then_some((project, dir))
        })
}

fn path(home: &Path) -> PathBuf {
    home.join("recent.jsonl")
}

#[cfg(test)]
#[path = "recent_tests.rs"]
mod tests;
