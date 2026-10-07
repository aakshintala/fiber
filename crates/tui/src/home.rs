//! Home's data: what the terminal knows about where it was launched,
//! the session list's rows, and what home draws (`docs/tui.md`, "Home",
//! "The session list"). Later parts add the subscriptions and the picker
//! state.

use std::path::PathBuf;

use contract::events::{SessionState, SessionStatus, WaitingKind};
use contract::{Envelope, SessionId};
use serde_json::Value;

/// What the terminal knows about where it was launched.
pub struct Launch {
    /// The launch directory; `start`'s default workspace.
    pub workspace: PathBuf,
    /// Its project key (`docs/state.md`, "Projects").
    pub project: String,
    /// The launch directory is inside a git repository.
    pub git: bool,
    /// `tui.hover`: with it off, mouse mode 1003 is never sent and nothing
    /// is tinted under the pointer.
    pub hover: bool,
    /// Fiber's version, for the logo.
    pub version: String,
}

/// What home draws, built by [`crate::app::App::home_screen`].
pub(crate) struct HomeScreen {
    /// Fiber's version, for the logo.
    pub(crate) version: String,
    /// The glyph before the name in the one-row logo.
    pub(crate) glyph: String,
    /// The chip row, left to right.
    pub(crate) chips: Vec<String>,
    /// The rows: their keys, their lines, and whether they end in a ✕.
    pub(crate) rows: Vec<(u64, String, bool)>,
    /// The foot hint, or the quit hint while Ctrl+C is armed.
    pub(crate) foot: String,
    /// The input box shows its placeholder.
    pub(crate) placeholder: bool,
}

/// What a session is doing, as its row's glyph names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// A turn is running.
    Working,
    /// A model call waits to retry.
    Retrying,
    /// Waiting on the person.
    Waiting,
    /// No turn is running, and jobs are.
    Jobs,
    /// Nothing is in flight.
    Idle,
    /// The status cannot be read: its schema is newer, or it does not
    /// parse. The row says "cannot attach".
    Unreadable,
}

/// How a session ended, as its row's glyph names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Left {
    /// Its log ends in `fiber_exited` or `rewound`.
    Exited,
    /// The process died.
    Crashed,
}

/// One session on home: a feed row while live, a recent row once exited.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    /// The click target's key, set by [`Sessions`]: unique, never reused,
    /// kept through status updates, `left`, and a move from `recent` to
    /// the feed.
    pub(crate) key: u64,
    /// The session.
    pub(crate) id: SessionId,
    /// Its name, or its first prompt when it has none; the id when empty.
    pub(crate) name: String,
    /// The workspace path.
    pub(crate) workspace: String,
    /// The project's key.
    pub(crate) project: String,
    /// What the session is doing.
    pub(crate) state: State,
    /// How it ended; `None` while live.
    pub(crate) left: Option<Left>,
    /// What it waits on: "approval: \<summary\>" or "question:
    /// \<summary\>". A left row whose last status waited keeps it.
    pub(crate) waiting: Option<String>,
    /// Its spend so far, in US dollars; left out of the line at zero.
    pub(crate) spend: f64,
    /// What a refusal says about the row, in place of its detail.
    pub(crate) note: Option<String>,
}

/// The session list: live rows in feed order, then exited rows newest
/// first. A session id is in at most one of the two.
#[derive(Default)]
pub(crate) struct Sessions {
    feed: Vec<Row>,
    recent: Vec<Row>,
    next_key: u64,
}

impl Sessions {
    /// Folds a `session_status` into the rows: an id already listed keeps
    /// its position and its key, a recent row moves into the feed, and a
    /// new id goes after the rows already there. A status means the
    /// session is live, so it clears `left`: a resumed session.
    pub(crate) fn status(&mut self, row: Row) {
        if let Some(existing) = self.feed.iter_mut().find(|feed| feed.id == row.id) {
            let key = existing.key;
            *existing = row;
            existing.key = key;
            existing.left = None;
            return;
        }
        if let Some(at) = self.recent.iter().position(|recent| recent.id == row.id) {
            let key = self.recent.get(at).map_or(0, |recent| recent.key);
            self.recent.remove(at);
            let mut row = row;
            row.key = key;
            row.left = None;
            self.feed.push(row);
            return;
        }
        let mut row = row;
        row.key = self.fresh();
        row.left = None;
        self.feed.push(row);
    }

    /// Marks `id` left, keeping its position; an unknown id changes
    /// nothing.
    pub(crate) fn left(&mut self, id: &SessionId, how: Left) {
        for row in self.feed.iter_mut().chain(self.recent.iter_mut()) {
            if row.id == *id {
                row.left = Some(how);
                return;
            }
        }
    }

    /// Folds a `recent` page into the rows: the first page replaces them,
    /// an older page appends. Neither adds an id already in the feed or,
    /// for an older page, already listed; a first page listing an id
    /// again keeps its key.
    pub(crate) fn recent(&mut self, rows: Vec<Row>, first: bool) {
        if first {
            let old = std::mem::take(&mut self.recent);
            let mut kept = Vec::with_capacity(rows.len());
            for mut row in rows {
                if self.feed.iter().any(|feed| feed.id == row.id)
                    || kept.iter().any(|keep: &Row| keep.id == row.id)
                {
                    continue;
                }
                row.key = old
                    .iter()
                    .find(|recent| recent.id == row.id)
                    .map_or_else(|| self.fresh(), |recent| recent.key);
                kept.push(row);
            }
            self.recent = kept;
        } else {
            for mut row in rows {
                if self.feed.iter().any(|feed| feed.id == row.id)
                    || self.recent.iter().any(|recent| recent.id == row.id)
                {
                    continue;
                }
                row.key = self.fresh();
                self.recent.push(row);
            }
        }
    }

    /// The rows home draws: the feed rows in feed order, then the recent
    /// rows. Scoped, only the launch project's rows.
    pub(crate) fn shown(&self, project: &str, scoped: bool) -> Vec<&Row> {
        self.feed
            .iter()
            .chain(self.recent.iter())
            .filter(|row| !scoped || row.project == project)
            .collect()
    }

    /// The next key: unique, never reused, even after a row drops.
    fn fresh(&mut self) -> u64 {
        let key = self.next_key;
        self.next_key = self.next_key.saturating_add(1);
        key
    }
}

/// A feed row from a `session_status` envelope. "Cannot read" is a newer
/// `schema_version` than this terminal reads, or a payload that does not
/// parse: the row is [`State::Unreadable`].
pub(crate) fn from_status(envelope: &Envelope) -> Row {
    let status = (envelope.schema_version == contract::SCHEMA_VERSION)
        .then(|| {
            serde_json::from_value::<SessionStatus>(Value::Object(envelope.payload.clone())).ok()
        })
        .flatten();
    let Some(status) = status else {
        return Row {
            key: 0,
            id: envelope.session_id.clone(),
            name: String::new(),
            workspace: String::new(),
            project: String::new(),
            state: State::Unreadable,
            left: None,
            waiting: None,
            spend: 0.0,
            note: None,
        };
    };
    let (state, waiting) = match &status.state {
        SessionState::Streaming | SessionState::Tool { .. } => (State::Working, None),
        SessionState::Retrying => (State::Retrying, None),
        SessionState::Waiting { waiting } => {
            let kind = match waiting.kind {
                WaitingKind::Approval => "approval",
                WaitingKind::Question => "question",
            };
            (State::Waiting, Some(format!("{kind}: {}", waiting.summary)))
        }
        SessionState::Jobs => (State::Jobs, None),
        SessionState::Idle => (State::Idle, None),
    };
    Row {
        key: 0,
        id: envelope.session_id.clone(),
        name: status.name,
        workspace: status.workspace,
        project: status.project,
        state,
        left: None,
        waiting,
        spend: status.spend.cost.unwrap_or(0.0) + status.spend.subscription_cost,
        note: None,
    }
}

/// The rows of a `recent` answer's `result`: `{"sessions":[…]}`, each
/// with its session, project, workspace, name, how it ended, and its last
/// status when it wrote one. A row without a session id is skipped.
pub(crate) fn recent_rows(result: &Value) -> Vec<Row> {
    result
        .get("sessions")
        .and_then(Value::as_array)
        .map(|sessions| {
            sessions
                .iter()
                .filter_map(|session| {
                    let id = session.get("session_id").and_then(Value::as_str)?;
                    let status = session.get("status").and_then(|status| {
                        serde_json::from_value::<SessionStatus>(status.clone()).ok()
                    });
                    let (state, waiting, spend) = match &status {
                        None => (State::Idle, None, 0.0),
                        Some(status) => {
                            let (state, waiting) = match &status.state {
                                SessionState::Streaming | SessionState::Tool { .. } => {
                                    (State::Working, None)
                                }
                                SessionState::Retrying => (State::Retrying, None),
                                SessionState::Waiting { waiting } => {
                                    let kind = match waiting.kind {
                                        WaitingKind::Approval => "approval",
                                        WaitingKind::Question => "question",
                                    };
                                    (State::Waiting, Some(format!("{kind}: {}", waiting.summary)))
                                }
                                SessionState::Jobs => (State::Jobs, None),
                                SessionState::Idle => (State::Idle, None),
                            };
                            (
                                state,
                                waiting,
                                status.spend.cost.unwrap_or(0.0) + status.spend.subscription_cost,
                            )
                        }
                    };
                    let how = match session.get("how").and_then(Value::as_str) {
                        Some("crashed") => Left::Crashed,
                        _ => Left::Exited,
                    };
                    Some(Row {
                        key: 0,
                        id: SessionId(id.to_owned()),
                        name: session
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        workspace: session
                            .get("workspace")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        project: session
                            .get("project")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        state,
                        left: Some(how),
                        waiting,
                        spend,
                        note: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A row's line: the glyph, the name or the id, the detail, and the
/// workspace's last path segment outside the launch project, joined by
/// two spaces with empty parts left out.
pub(crate) fn line(row: &Row, launch_project: &str) -> String {
    // The working states share one still glyph: a still glyph, not the
    // spinner; upgrade when the working line's timer lands (see #686).
    let glyph = match (&row.left, &row.state) {
        (Some(Left::Exited), _) => "○",
        (Some(Left::Crashed), _) => "✗",
        (None, State::Working | State::Retrying | State::Jobs) => "●",
        (None, State::Waiting) => "!",
        (None, State::Idle) => "✓",
        (None, State::Unreadable) => "?",
    };
    let name = if row.name.is_empty() {
        row.id.0.as_str()
    } else {
        row.name.as_str()
    };
    let mut detail = row.note.clone().unwrap_or_default();
    if detail.is_empty() {
        if row.state == State::Unreadable {
            detail = "cannot attach".to_owned();
        } else {
            let mut parts = Vec::new();
            if let Some(waiting) = &row.waiting {
                parts.push(waiting.clone());
            }
            if row.spend > 0.0 {
                parts.push(crate::format::money(row.spend));
            }
            detail = parts.join(" · ");
        }
    }
    let mut segment = String::new();
    if row.project != launch_project {
        segment = row
            .workspace
            .split('/')
            .rfind(|part| !part.is_empty())
            .unwrap_or_default()
            .to_owned();
    }
    [glyph.to_owned(), name.to_owned(), detail, segment]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
