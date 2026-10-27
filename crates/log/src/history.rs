//! A session's history across the sessions it continues
//! (`docs/events.md`, "Rewind", "Change workspace"): the chain of logs,
//! root first, that `session_started.forked_from` links. Each ancestor
//! resolves in its recorded project, never the child's
//! (`docs/state.md`, "Projects"). Only `log` opens the session
//! directories here (`docs/architecture.md`, "The call rules").

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use contract::{Envelope, Seq, SessionId};

use crate::{EVENTS, Error, io_at};

/// One log in a session's history: the session, its directory, and the
/// last line of it the history holds. The last segment's is `None`: the
/// whole own log. Any other segment's is the child's `forked_from.seq`,
/// inclusive: the history holds the lines with `seq <= to`.
#[derive(Debug, PartialEq, Eq)]
pub struct Segment {
    /// The session, from its directory name.
    pub session_id: SessionId,
    /// The session's directory.
    pub dir: PathBuf,
    /// The last line of this log the history holds; `None` holds all of it.
    pub to: Option<Seq>,
}

/// Whether `value` names one plain path component, so joining it under a
/// directory cannot leave that directory.
fn single_component(value: &str) -> bool {
    let mut parts = Path::new(value).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

/// The chain of logs `dir`'s session continues, root first and its own log
/// last with `to: None`. A missing parent log, in the child's project or
/// in a recorded one, is [`Error::NotFound`]; a pointer cycle, a first
/// line that is not `session_started`, or a recorded parent session or
/// project that is not one path component, is [`Error::Pointer`].
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
        let Some((parent, seq, project)) = forked else {
            break;
        };
        // A recorded link is one component naming the parent session, and
        // one naming its project: anything else cannot name a session
        // directory, so the walk fails closed before touching the disk.
        if !single_component(&parent.0) {
            return Err(Error::Pointer {
                path: at.join(EVENTS),
                reason: "the recorded parent session is not one path component".to_owned(),
            });
        }
        at = match project {
            // The parent lives beside the child, in the same `sessions/`
            // directory: a rewind continues in the old session's
            // workspace, and a move without a project does too.
            None => {
                let Some(sessions) = at.parent() else {
                    return Err(Error::Pointer {
                        path: at.join(EVENTS),
                        reason: "the session directory has no parent".to_owned(),
                    });
                };
                sessions.join(&parent.0)
            }
            // The parent lives in its recorded project: the reader opens
            // `projects/<project>/sessions/<session_id>/` directly, never
            // the child's, and never scans other projects
            // (`docs/state.md`, "Projects"). Every step climbs stored
            // directories only: no git, no environment.
            Some(key) => {
                // The session directory, its `sessions/`, its project key
                // and `projects/`: the next ancestor is the home. The
                // projects layer is required by name, so a flat layout
                // cannot climb into system directories.
                let mut up = at.ancestors();
                up.next();
                let home = match (up.next(), up.next(), up.next(), up.next()) {
                    (Some(_), Some(_), Some(projects), Some(home))
                        if projects.file_name().and_then(|name| name.to_str())
                            == Some("projects") =>
                    {
                        home
                    }
                    _ => {
                        return Err(Error::Pointer {
                            path: at.join(EVENTS),
                            reason: "the session directory is not under projects".to_owned(),
                        });
                    }
                };
                match crate::sessions_dir_for_key(home, &key) {
                    Some(sessions) => sessions.join(&parent.0),
                    None => {
                        return Err(Error::Pointer {
                            path: at.join(EVENTS),
                            reason: "the recorded parent project is not one path component"
                                .to_owned(),
                        });
                    }
                }
            }
        };
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
        self.range(from, usize::MAX)
    }

    /// At most `max` durable lines from `from`, bounded by `to` inclusive.
    /// Scans only up to the point and parses only the returned window.
    /// Empty when `from` is past `to`; otherwise a log ending before `to`
    /// fails with [`Error::Pointer`].
    pub fn range(&self, from: u64, max: usize) -> Result<Vec<Envelope>, Error> {
        let end = self.to.as_ref().map_or(u64::MAX, |to| to.0);
        if from > end {
            return Ok(Vec::new());
        }
        let offsets = crate::offsets::Offsets::scan(&self.dir, end.saturating_add(1), |_| Ok(()))?;
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
        offsets.range(from, max)
    }
}

/// The `seq` of the last complete line of the log in `dir`: `Ok(None)`
/// only for a regular, empty log. Every other outcome without a `seq` is
/// an error naming the log's path: an open or metadata failure is
/// [`Error::Io`], a missing log included, and a log whose last line
/// gives no boundary is [`Error::Pointer`]. Reads only the one handle it
/// opened, never reopening by path: a log hidden during the read and
/// restored before the spawn fails closed instead of replaying an
/// earlier run's end. Bytes after the last `\n` are torn and are dropped.
pub fn last_seq(dir: &Path) -> Result<Option<Seq>, Error> {
    let path = dir.join(EVENTS);
    let file = File::open(&path).map_err(io_at(&path))?;
    let meta = file.metadata().map_err(io_at(&path))?;
    if !meta.file_type().is_file() {
        return Err(Error::Pointer {
            path,
            reason: "the log is not a regular file".to_owned(),
        });
    }
    if meta.len() == 0 {
        return Ok(None);
    }
    let Some(line) = crate::scan::last_complete_line(file, meta.len()) else {
        return Err(Error::Pointer {
            path,
            reason: "no complete line could be read from the log".to_owned(),
        });
    };
    let line: Envelope = serde_json::from_slice(&line).map_err(|_| Error::Pointer {
        path: path.clone(),
        reason: "the log's last line is not an event".to_owned(),
    })?;
    line.seq
        .ok_or_else(|| Error::Pointer {
            path,
            reason: "the log's last line has no seq".to_owned(),
        })
        .map(Some)
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

/// What `dir`'s first line forks from: the parent session, the last line
/// taken from it, and the parent's project key when it differs from the
/// child's (`docs/state.md`, "Projects"). `None` for a root session. A
/// missing or empty log is [`Error::NotFound`], as if it were never
/// written; anything else that is not a `session_started` first line is
/// [`Error::Pointer`}.
fn first_fork(dir: &Path) -> Result<Option<(SessionId, Seq, Option<String>)>, Error> {
    let path = dir.join(EVENTS);
    // A crash between the session directory's creation and its first
    // write leaves an empty log: there is no history to follow, so it
    // reads as missing.
    if std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(1) == 0 {
        return Err(Error::NotFound(dir.to_path_buf()));
    }
    crate::scan::first_line(dir)?
        .fork_point()
        .map_err(|_| Error::Pointer {
            path: dir.join(EVENTS),
            reason: "the log's forked_from is not a session, a seq and a project".to_owned(),
        })
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
