//! A session's history across the sessions it continues
//! (`docs/events.md`, "Rewind"): the chain of logs, root first, that
//! `session_started.forked_from` links. Only `log` opens the session
//! directories here (`docs/architecture.md`, "The call rules").

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use contract::{Envelope, Seq, SessionId};

use crate::{EVENTS, Error, io_at};

/// One log in a session's history: the session, its directory, and the
/// last line of it the history holds. The last segment's is `None`: the
/// whole own log. Any other segment's is the child's `forked_from.seq`,
/// inclusive: the history holds the lines with `seq <= to`.
#[derive(Debug)]
pub struct Segment {
    /// The session, from its directory name.
    pub session_id: SessionId,
    /// The session's directory.
    pub dir: PathBuf,
    /// The last line of this log the history holds; `None` holds all of it.
    pub to: Option<Seq>,
}

/// The chain of logs `dir`'s session continues, root first and its own log
/// last with `to: None`. A missing parent log is [`Error::NotFound`]; a
/// pointer cycle, or a first line that is not `session_started`, is
/// [`Error::Pointer`].
pub fn history(dir: &Path) -> Result<Vec<Segment>, Error> {
    let mut segments = Vec::new();
    let mut seen = HashSet::new();
    let mut at = dir.to_path_buf();
    let mut to: Option<Seq> = None;
    loop {
        let id = at
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or_else(|| Error::Pointer {
                path: at.join(EVENTS),
                reason: "the session directory has no name".to_owned(),
            })?;
        // The walk stops on a repeat: a pointer cycle never resolves.
        if !seen.insert(id.clone()) {
            return Err(Error::Pointer {
                path: at.join(EVENTS),
                reason: "the forked_from chain repeats a session".to_owned(),
            });
        }
        let forked = first_fork(&at)?;
        segments.push(Segment {
            session_id: SessionId(id),
            dir: at.clone(),
            to,
        });
        let Some((parent, seq)) = forked else {
            break;
        };
        // The parent lives beside the child, in the same `sessions/`
        // directory: a rewind continues in the old session's workspace.
        let Some(sessions) = at.parent() else {
            return Err(Error::Pointer {
                path: at.join(EVENTS),
                reason: "the session directory has no parent".to_owned(),
            });
        };
        at = sessions.join(&parent.0);
        to = Some(seq);
    }
    segments.reverse();
    Ok(segments)
}

/// [`history`] of `dir` with the last segment's `to` set to `point`: the
/// history of any session on the chain, the old session or an ancestor,
/// folded to `point`.
pub fn history_to(dir: &Path, point: Seq) -> Result<Vec<Segment>, Error> {
    let mut segments = history(dir)?;
    let Some(last) = segments.last_mut() else {
        return Err(Error::Pointer {
            path: dir.join(EVENTS),
            reason: "the session has no log".to_owned(),
        });
    };
    last.to = Some(point);
    Ok(segments)
}

impl Segment {
    /// This log's durable lines from `seq` `from` up to `to` inclusive, in
    /// order. Empty when `from` is past `to`. Reads only to `to` and
    /// stops: lines past the point are never parsed (`docs/events.md`,
    /// "Resume"). Fails [`Error::Pointer`] when the log ends before
    /// `to`: the pointer names a line the log does not hold.
    pub fn lines(&self, from: u64) -> Result<Vec<Envelope>, Error> {
        let end = self.to.as_ref().map_or(u64::MAX, |to| to.0);
        if from > end {
            return Ok(Vec::new());
        }
        let offsets = crate::offsets::Offsets::scan(&self.dir, end.saturating_add(1))?;
        // The log ends before `to`: nothing past its last line is in
        // this session's history.
        if let Some(to) = &self.to
            && offsets.count() <= to.0
        {
            return Err(Error::Pointer {
                path: self.dir.join(EVENTS),
                reason: format!("session {} ends before seq {}", self.session_id.0, to.0),
            });
        }
        // The scan stopped at `to`, so the table holds nothing past it.
        offsets.range(from, usize::MAX)
    }
}

/// The last complete line of the log in `dir`: `None` when the log is
/// missing or empty, when it holds no complete line, or when that line
/// does not parse. Bytes after the last `\n` are torn and are dropped.
pub fn last_line(dir: &Path) -> Option<Envelope> {
    let path = dir.join(EVENTS);
    let file = File::open(&path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        return None;
    }
    let line = crate::scan::last_complete_line(file, len)?;
    serde_json::from_slice(&line).ok()
}

/// What `dir`'s first line forks from: the parent session and the last
/// line taken from it. `None` for a root session. A missing log is
/// [`Error::NotFound`]; anything else that is not a `session_started`
/// first line is [`Error::Pointer`}.
fn first_fork(dir: &Path) -> Result<Option<(SessionId, Seq)>, Error> {
    let path = dir.join(EVENTS);
    let file = File::open(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound(dir.to_owned())
        } else {
            io_at(&path)(error)
        }
    })?;
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
        path: dir.join(EVENTS),
        reason: "the log's first line is not an event".to_owned(),
    })?;
    if line.kind != "session_started" {
        return Err(Error::Pointer {
            path: dir.join(EVENTS),
            reason: "the log's first line is not session_started".to_owned(),
        });
    }
    let forked = line.payload.get("forked_from").and_then(|from| {
        let session = from.get("session_id")?.as_str()?;
        let seq = from.get("seq")?.as_u64()?;
        Some((SessionId(session.to_owned()), Seq(seq)))
    });
    // A `forked_from` that does not read as a pointer is corrupt, not absent.
    if line.payload.get("forked_from").is_some() && forked.is_none() {
        return Err(Error::Pointer {
            path: dir.join(EVENTS),
            reason: "the log's forked_from is not a session and a seq".to_owned(),
        });
    }
    Ok(forked)
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
