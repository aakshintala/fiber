//! The prune scan (`docs/invocation.md`, "Deleting and pruning"): every
//! read and lock prune needs on a session directory. `cli` keeps the
//! decisions; this module only reads.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use contract::shapes::Worktree;
use contract::{Envelope, Seq, SessionId};
use serde_json::Value;

use crate::{EVENTS, Error, LOCK, io_at};

/// A session named by its first line: its id, directory, workspace and the
/// session it continues, when each is known.
pub struct Started {
    /// The session's id, from its directory name.
    pub id: SessionId,
    /// The session's directory.
    pub dir: PathBuf,
    /// `payload.workspace` when it is a string.
    pub workspace: Option<String>,
    /// `payload.worktree.path` when it is a string.
    pub worktree: Option<String>,
    /// `payload.forked_from.session_id` when it is a string.
    pub forked_from: Option<SessionId>,
}

/// Every started session in one project's `sessions` directory, sorted
/// by id: each directory whose first line is a readable
/// `session_started`. A directory that cannot be read gives nothing, not
/// an error. A symlinked directory is not a session, and neither is a
/// missing directory.
pub fn started_sessions_in(sessions: &Path) -> Vec<Started> {
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        // `file_type` does not follow a link: a linked directory is
        // not a session.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let dir = entry.path();
        let Some(view) = started_in(&dir) else {
            continue;
        };
        let id = SessionId(entry.file_name().to_string_lossy().into_owned());
        let forked_from = view.forked_from();
        found.push(Started {
            id,
            dir,
            workspace: view.workspace,
            worktree: view.worktree,
            forked_from,
        });
    }
    found.sort_by(|a: &Started, b: &Started| a.id.0.cmp(&b.id.0));
    found
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
        found.extend(started_sessions_in(&project.path().join("sessions")));
    }
    found.sort_by(|a: &Started, b: &Started| a.id.0.cmp(&b.id.0));
    found
}

/// A session's first line as a `session_started`: the lenient view both
/// walks share. [`SessionStarted::parse`] reads what the scans accept;
/// [`first_line`] reads what history needs; [`started_first`] reads what
/// resume needs: its workspace, repository and delegate bit.
pub struct SessionStarted {
    /// `payload.workspace` when it is a string.
    pub workspace: Option<String>,
    /// `payload.worktree.repository` when its worktree is an object
    /// naming one as a string.
    pub repository: Option<String>,
    /// `payload.worktree.path` when its worktree is an object naming
    /// one as a string.
    pub worktree: Option<String>,
    /// Whether `payload.parent` is present and not null: a delegate's.
    /// A parent of any other shape counts, so a malformed one is never
    /// resumed.
    pub delegate: bool,
    /// `payload.parent.session_id` when it is a string: the session that
    /// delegated this one, if any.
    parent: Option<SessionId>,
    /// `payload.forked_from`, raw: the two pointer policies read it
    /// differently.
    forked_from: Option<Value>,
    /// `payload.worktree`, raw: the tri-state policy reads it strictly.
    worktree_raw: Option<Value>,
}

impl SessionStarted {
    /// The shared read of one `session_started` payload.
    fn of(payload: &Value) -> Self {
        Self {
            workspace: payload
                .get("workspace")
                .and_then(Value::as_str)
                .map(str::to_owned),
            repository: payload
                .get("worktree")
                .and_then(|worktree| worktree.get("repository"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            worktree: payload
                .get("worktree")
                .and_then(|worktree| worktree.get("path"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            delegate: payload
                .get("parent")
                .is_some_and(|parent| !parent.is_null()),
            parent: payload
                .get("parent")
                .and_then(|parent| parent.get("session_id"))
                .and_then(Value::as_str)
                .map(|id| SessionId(id.to_owned())),
            forked_from: payload.get("forked_from").cloned(),
            worktree_raw: payload.get("worktree").cloned(),
        }
    }

    /// The view of `first`, a log's first line with or without its
    /// newline: `None` when it does not read as a `session_started`
    /// with a payload. No newline, `ts` or `seq` is needed.
    pub(crate) fn parse(first: &[u8]) -> Option<Self> {
        let line: Value = serde_json::from_slice(first).ok()?;
        if line.get("kind")?.as_str()? != "session_started" {
            return None;
        }
        Some(Self::of(line.get("payload")?))
    }

    /// `payload.parent.session_id` when it is a string: the session that
    /// delegated this one, if any.
    pub(crate) fn parent(&self) -> Option<SessionId> {
        self.parent.clone()
    }

    /// `forked_from.session_id` when it is a string: the lenient policy
    /// the scans walk by. A `seq` is not needed.
    pub(crate) fn forked_from(&self) -> Option<SessionId> {
        self.forked_from
            .as_ref()?
            .get("session_id")?
            .as_str()
            .map(|id| SessionId(id.to_owned()))
    }

    /// The recorded worktree: the strict policy the hub and the session
    /// continuation walk by. Absent (the key is missing) is `Ok(None)`;
    /// a record with an absolute-string `path`, a string `branch` and a
    /// present absolute-path `repository` is `Ok(Some)`; `null` is `Err`
    /// (the payload rules forbid `null`: an optional key is absent, never
    /// `null`), as is any other present value (every path other than
    /// specified relative paths is absolute, so a relative recorded path
    /// is foreign data).
    /// Public because the hub and the session process both validate
    /// through it.
    pub fn worktree_record(&self) -> Result<Option<Worktree>, MalformedWorktree> {
        match &self.worktree_raw {
            None => Ok(None),
            Some(Value::Null) => Err(MalformedWorktree),
            Some(raw) => match serde_json::from_value::<Worktree>(raw.clone()) {
                Ok(record)
                    if Path::new(&record.path).is_absolute()
                        && record
                            .repository
                            .as_deref()
                            .is_some_and(|repository| Path::new(repository).is_absolute()) =>
                {
                    Ok(Some(record))
                }
                _ => Err(MalformedWorktree),
            },
        }
    }

    /// What the first line forks from: the strict policy history walks
    /// by. Absent is `Ok(None)`; any present value without a string
    /// `session_id`, a `u64` `seq` and, when present, a string `project`
    /// is malformed. The project is the parent's project key
    /// (`docs/state.md`, "Projects"): absent means the parent is in the
    /// child's project.
    pub(crate) fn fork_point(
        &self,
    ) -> Result<Option<(SessionId, Seq, Option<String>)>, MalformedFork> {
        let Some(from) = self.forked_from.as_ref() else {
            return Ok(None);
        };
        match (
            from.get("session_id").and_then(Value::as_str),
            from.get("seq").and_then(Value::as_u64),
        ) {
            (Some(session), Some(seq)) => Ok(Some((
                SessionId(session.to_owned()),
                Seq(seq),
                match from.get("project") {
                    None => None,
                    Some(Value::String(project)) => Some(project.clone()),
                    Some(_) => return Err(MalformedFork),
                },
            ))),
            _ => Err(MalformedFork),
        }
    }
}

/// A `forked_from` that is present but names no session and `seq`.
#[derive(Debug)]
pub(crate) struct MalformedFork;

/// A `worktree` value that is present but is not a record with an
/// absolute-string `path`, a string `branch` and a present absolute-path
/// `repository`.
/// Public because the hub and the session process both refuse it as
/// `log_corrupt`; only named in this crate's signatures.
#[derive(Debug)]
pub struct MalformedWorktree;

/// The [`SessionStarted`] of the session in `dir`: the strict policy
/// history walks by. A missing log is [`Error::NotFound`]; anything else
/// that is not a `session_started` first line is [`Error::Pointer`].
/// Reads only the first line, never the log past it.
pub(crate) fn first_line(dir: &Path) -> Result<SessionStarted, Error> {
    let path = dir.join(EVENTS);
    let file = crate::open_log(dir)?;
    let mut first = Vec::new();
    BufReader::new(file)
        .read_until(b'\n', &mut first)
        .map_err(io_at(&path))?;
    if first.last() != Some(&b'\n') {
        return Err(Error::Pointer {
            path,
            reason: "the log's first line is missing".to_owned(),
        });
    }
    let line: Envelope = serde_json::from_slice(&first).map_err(|_| Error::Pointer {
        path: path.clone(),
        reason: "the log's first line is not an event".to_owned(),
    })?;
    if line.kind != "session_started" {
        return Err(Error::Pointer {
            path,
            reason: "the log's first line is not session_started".to_owned(),
        });
    }
    Ok(SessionStarted::of(&Value::Object(line.payload)))
}

/// What [`started_first`] found: the log's first line is no complete
/// `session_started` with a workspace, or it is corrupt.
#[derive(Debug, PartialEq, Eq)]
pub enum FirstLineError {
    /// The log is missing, empty, or its first line has no `\n` yet:
    /// the session is answered as for a missing log. Completeness is
    /// decided on bytes before any decoding, so a line torn inside a
    /// multi-byte character is incomplete, not corrupt.
    Incomplete,
    /// A read error on an existing log, bytes that are not UTF-8, a
    /// complete line that does not parse, a kind other than
    /// `session_started`, or a payload with no string workspace.
    Corrupt,
}

/// The [`SessionStarted`] of the session in `dir`: the lenient
/// first-line read requiring a string workspace. Reads only the
/// first line, never the log past it.
pub fn started_first(dir: &Path) -> Result<SessionStarted, FirstLineError> {
    let path = dir.join(EVENTS);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(FirstLineError::Incomplete);
        }
        Err(_) => return Err(FirstLineError::Corrupt),
    };
    let mut first = Vec::new();
    match BufReader::new(file).read_until(b'\n', &mut first) {
        Ok(_) => {}
        Err(_) => return Err(FirstLineError::Corrupt),
    }
    if first.is_empty() || first.last() != Some(&b'\n') {
        return Err(FirstLineError::Incomplete);
    }
    let line: Value = serde_json::from_slice(&first).map_err(|_| FirstLineError::Corrupt)?;
    if line.get("kind").and_then(Value::as_str) != Some("session_started") {
        return Err(FirstLineError::Corrupt);
    }
    let Some(payload) = line.get("payload") else {
        return Err(FirstLineError::Corrupt);
    };
    let view = SessionStarted::of(payload);
    if view.workspace.is_none() {
        return Err(FirstLineError::Corrupt);
    }
    Ok(view)
}

/// The [`SessionStarted`] of the session in `dir`: `None` when the log
/// cannot be read or its first line does not read as one. A first line
/// with no trailing newline still counts, as `read_line` reads it.
fn started_in(dir: &Path) -> Option<SessionStarted> {
    let file = crate::open_log(dir).ok()?;
    let mut first = Vec::new();
    BufReader::new(file).read_until(b'\n', &mut first).ok()?;
    SessionStarted::parse(&first)
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

/// The first `Some` that `found` gives for the complete lines of
/// `dir/events.jsonl`, read from the end backwards; bytes after the last
/// newline are torn and skipped.
pub fn last_line_where<T>(dir: &Path, found: impl FnMut(&[u8]) -> Option<T>) -> Option<T> {
    let path = dir.join(EVENTS);
    let file = File::open(&path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        return None;
    }
    let mut found = found;
    walk_back(file, len, &mut found)
}

/// Feeds the complete lines of `file`'s `len` bytes to `found` from the end
/// backwards, stopping at its first `Some`. Read backwards in [`STEP`]
/// steps, the offset strictly decreasing on every step, stopping at 0. A
/// line spanning a step boundary is handed over whole; bytes after the
/// last newline are torn and skipped, so a file with one line and no
/// newline gives `None`.
fn walk_back<T>(mut file: File, len: u64, found: &mut impl FnMut(&[u8]) -> Option<T>) -> Option<T> {
    // Bytes after the last newline seen so far, with no newline in them:
    // the torn tail, then each earlier window's partial head fused to it.
    let mut carry: Vec<u8> = Vec::new();
    let mut end = len;
    while end != 0 {
        let start = end.saturating_sub(STEP);
        file.seek(SeekFrom::Start(start)).ok()?;
        let size = usize::try_from(end - start).ok()?;
        let mut chunk = vec![0_u8; size];
        file.read_exact(&mut chunk).ok()?;
        chunk.extend_from_slice(&std::mem::take(&mut carry));
        let marks: Vec<usize> = chunk
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .map(|(index, _)| index)
            .collect();
        if marks.is_empty() {
            // No newline: the whole chunk continues an earlier line.
            carry = chunk;
        } else {
            let (Some(first), Some(last)) = (marks.first(), marks.last()) else {
                return None;
            };
            // Every segment between two newlines is complete; the head
            // continues before `start`, the tail after the last newline.
            for pair in marks.windows(2).rev() {
                let &[from, to] = pair else {
                    return None;
                };
                let line = chunk.get(from.checked_add(1)?..to)?;
                if let Some(line) = found(line) {
                    return Some(line);
                }
            }
            if start == 0 {
                // The head is the file's first line: complete.
                return found(chunk.get(..*first)?);
            }
            // The head's line ends at the first newline: kept with its
            // terminator, fused to the tail, which holds no newline, so
            // the line completes once its start is read.
            let (Some(end), Some(tail)) = (first.checked_add(1), chunk.get(last.checked_add(1)?..))
            else {
                return None;
            };
            let head = chunk.get(..end)?;
            carry = head.to_vec();
            carry.extend_from_slice(tail);
        }
        end = start;
    }
    None
}

/// The last complete line in `file` of `len` bytes, without its trailing
/// newline: read backwards in [`STEP`] steps, the offset strictly
/// decreasing on every step, stopping at 0.
pub(crate) fn last_complete_line(file: File, len: u64) -> Option<Vec<u8>> {
    walk_back(file, len, &mut |line: &[u8]| Some(line.to_owned()))
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
pub struct SessionLock {
    _file: File,
}

/// Opens `dir`'s lock file for locking: `Some` file when the lock takes,
/// `None` when another process holds it.
pub(crate) fn try_lock_file(dir: &Path) -> Result<Option<File>, Error> {
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
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(source)) => Err(Error::Io { path, source }),
    }
}

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
    match try_lock_file(dir)? {
        Some(file) => Ok(Hold::Held(SessionLock { _file: file })),
        None => Ok(Hold::Busy),
    }
}

/// Takes `dir`'s lock without waiting, only when `session.lock` already
/// exists. A missing lock file stays missing, so a dry run that probes it
/// writes nothing: `None` means no lock file, `Some` is the existing
/// file's state, held or busy.
pub fn try_hold_existing(dir: &Path) -> Result<Option<Hold>, Error> {
    let path = dir.join(LOCK);
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Io { path, source }),
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(Hold::Held(SessionLock { _file: file }))),
        Err(TryLockError::WouldBlock) => Ok(Some(Hold::Busy)),
        Err(TryLockError::Error(source)) => Err(Error::Io { path, source }),
    }
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
