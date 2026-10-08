//! The prune scan (`docs/invocation.md`, "Deleting and pruning"): every
//! read and lock prune needs on a session directory. `cli` keeps the
//! decisions; this module only reads.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use contract::SessionId;
use serde_json::Value;

use crate::{EVENTS, Error, LOCK};

/// A session named by its first line: its id, directory, workspace and the
/// session it continues, when each is known.
pub struct Started {
    /// The session's id, from its directory name.
    pub id: SessionId,
    /// The session's directory.
    pub dir: PathBuf,
    /// `payload.workspace` when it is a string.
    pub workspace: Option<String>,
    /// `payload.forked_from.session_id` when it is a string.
    pub forked_from: Option<SessionId>,
}

/// Every session under `home/projects/*/sessions/`, sorted by id: each
/// directory whose first line is a readable `session_started`. A directory
/// that cannot be read gives nothing, not an error. A symlinked directory
/// is not a session, and neither is a missing `projects/`.
pub fn started_sessions(home: &Path) -> Vec<Started> {
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for project in projects.flatten() {
        let Ok(sessions) = std::fs::read_dir(project.path().join("sessions")) else {
            continue;
        };
        for entry in sessions.flatten() {
            // `file_type` does not follow a link: a linked directory is
            // not a session.
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let dir = entry.path();
            let Some((workspace, forked_from)) = started_line(&dir.join(EVENTS)) else {
                continue;
            };
            let id = SessionId(entry.file_name().to_string_lossy().into_owned());
            found.push(Started {
                id,
                dir,
                workspace,
                forked_from,
            });
        }
    }
    found.sort_by(|a: &Started, b: &Started| a.id.0.cmp(&b.id.0));
    found
}

/// The workspace and fork of the `session_started` on `log`'s first line:
/// `None` when the line does not read as one. A first line with no
/// trailing newline still counts, as `read_line` reads it.
fn started_line(log: &Path) -> Option<(Option<String>, Option<SessionId>)> {
    let mut first = String::new();
    BufReader::new(File::open(log).ok()?)
        .read_line(&mut first)
        .ok()?;
    started_from(first.as_bytes())
}

/// The workspace and fork of the `session_started` in `first`, a log's
/// first line with or without its newline: `None` when it does not read as
/// one.
pub(crate) fn started_from(first: &[u8]) -> Option<(Option<String>, Option<SessionId>)> {
    let line: Value = serde_json::from_slice(first).ok()?;
    if line.get("kind")?.as_str()? != "session_started" {
        return None;
    }
    let payload = line.get("payload")?;
    let workspace = payload
        .get("workspace")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let forked_from = payload
        .get("forked_from")
        .and_then(|from| from.get("session_id"))
        .and_then(Value::as_str)
        .map(|id| SessionId(id.to_owned()));
    Some((workspace, forked_from))
}

/// The `ts` of the last complete line of `events.jsonl` in `dir`: `None`
/// when the log is missing or empty, when the last complete line does not
/// parse, or when it holds no numeric `ts`. Bytes after the last `\n` are
/// torn and are dropped, so a file with one line and no newline gives
/// `None`.
pub fn last_ts(dir: &Path) -> Option<u64> {
    let path = dir.join(EVENTS);
    let file = File::open(&path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        return None;
    }
    last_complete_line(file, len).and_then(|line| {
        serde_json::from_slice::<Value>(&line)
            .ok()?
            .get("ts")?
            .as_u64()
    })
}

/// How far back one backward step reads.
const STEP: u64 = 64 * 1024;

/// The last complete line in `file` of `len` bytes, without its trailing
/// newline: read backwards in [`STEP`] steps, the offset strictly
/// decreasing on every step, stopping at 0.
pub(crate) fn last_complete_line(mut file: File, len: u64) -> Option<Vec<u8>> {
    let mut line_end = None;
    let mut bounds = None;
    let step_size = usize::try_from(STEP).ok()?;

    'scan: for last in (0..len).rev().step_by(step_size) {
        let end = last + 1;
        let start = end.saturating_sub(STEP);
        file.seek(SeekFrom::Start(start)).ok()?;
        let size = usize::try_from(end - start).ok()?;
        let mut chunk = vec![0_u8; size];
        file.read_exact(&mut chunk).ok()?;

        for (index, byte) in chunk.iter().enumerate().rev() {
            if *byte != b'\n' {
                continue;
            }
            let newline = start + u64::try_from(index).ok()?;
            if let Some(end) = line_end {
                bounds = Some((newline + 1, end));
                break 'scan;
            }
            line_end = Some(newline);
        }
    }

    let (start, end) = bounds.or_else(|| line_end.map(|end| (0, end)))?;
    let size = usize::try_from(end - start).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut line = vec![0_u8; size];
    file.read_exact(&mut line).ok()?;
    Some(line)
}

/// The logical bytes under `dir`: the sum of `symlink_metadata().len()`
/// over the regular files below it. Links are not followed. A missing
/// directory holds nothing.
pub fn session_bytes(dir: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let mut bytes = 0_u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_file() {
                if let Ok(meta) = std::fs::symlink_metadata(entry.path()) {
                    bytes = bytes.saturating_add(meta.len());
                }
            } else if kind.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    bytes
}

/// What is still on disk in `dir`: `None` once the directory is gone. Any
/// error other than `NotFound` is returned.
pub fn remaining(dir: &Path) -> io::Result<Option<u64>> {
    match std::fs::symlink_metadata(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(_) => Ok(Some(session_bytes(dir))),
    }
}

/// A session's lock, held until it is dropped.
pub struct SessionLock(
    #[allow(dead_code, reason = "held for its Drop: closing releases the lock")] File,
);

/// Whether `try_hold` took the session's lock.
pub enum Hold {
    /// The lock, held until the guard is dropped.
    Held(SessionLock),
    /// Another process holds the lock.
    Busy,
}

/// Takes `dir`'s lock without waiting, creating `session.lock` when it is
/// missing. This is the one write prune makes in a session directory.
pub fn try_hold(dir: &Path) -> Result<Hold, Error> {
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => Ok(Hold::Held(SessionLock(file))),
        Err(TryLockError::WouldBlock) => Ok(Hold::Busy),
        Err(TryLockError::Error(source)) => Err(Error::Io { path, source }),
    }
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
