//! Home's data: what the terminal knows about where it was launched,
//! the session list's rows, and what home draws (`docs/tui.md`, "Home",
//! "The session list"). Later parts add the subscriptions and the picker
//! state.

use std::collections::HashMap;
use std::path::PathBuf;

use contract::events::{SessionState, SessionStatus, WaitingKind};
use contract::{Envelope, SessionId};
use serde_json::Value;

/// A click target on home: one row's line, by the key [`Sessions`]
/// gave it when it first appeared, its ✕ in the last column, the scope
/// toggle heading the list, the workspace chip opening the workspace
/// picker, and one picker row by its index in the list fixed at open.
/// The worktree switch lands in a later part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// A session row: clicking it, or Enter on it while focused, opens
    /// the session.
    Entry(u64),
    /// A row's ✕ in its last column: stopping a live session, or the
    /// delete question for an exited one. Unreadable rows have none.
    Stop(u64),
    /// The scope toggle heading the list: clicking it, or Enter on it,
    /// shows every project or only the launch one.
    Toggle,
    /// The workspace chip: clicking it opens the workspace picker.
    Workspace,
    /// A picker row, by its index in the list fixed at open.
    Pick(usize),
}

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
    /// `model`: the chip's model, unset on a fresh install with no key.
    pub model: Option<String>,
    /// `thinking`: the chip's level, unset for the default.
    pub thinking: Option<String>,
    /// `tui.logo_glyph`: the one-row logo's mark, "⌇" or "≈". The
    /// four-row logo's wave is drawn pixels, and never changes.
    pub logo_glyph: String,
}

/// What home draws, built by [`crate::app::App::home_screen`].
pub(crate) struct HomeScreen {
    /// Fiber's version, for the logo.
    pub(crate) version: String,
    /// The glyph before the name in the one-row logo.
    pub(crate) glyph: String,
    /// The chip row, left to right: each chip's text, and its click
    /// target when it has one.
    pub(crate) chips: Vec<(Option<Spot>, String)>,
    /// The toggle line heading the scope toggle: how many wait in other
    /// projects, or how to scope back down.
    pub(crate) toggle: Option<String>,
    /// The rows: their keys, their lines, and whether they end in a ✕.
    pub(crate) rows: Vec<(u64, String, bool)>,
    /// The rejected `start`'s message, above the box until the next
    /// `start` goes out.
    pub(crate) blockers: Vec<String>,
    /// The workspace picker above the box: its list, fixed at open, and
    /// the selected index.
    pub(crate) picker: Option<(Vec<String>, usize)>,
    /// The foot hint, or the quit hint while Ctrl+C is armed.
    pub(crate) foot: String,
    /// The input box shows its placeholder.
    pub(crate) placeholder: bool,
}

/// A subscription level on this connection: `summary` carries only the
/// latest `session_status`, `full` folds the whole stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    /// Only the latest status.
    Summary,
    /// The whole stream.
    Full,
}

/// What this connection holds, or will hold once its in-flight
/// subscribes are answered, per session. The accepted level changes
/// only on acceptance: answering removes the id from the in-flight
/// list either way, and a rejection leaves the accepted level.
#[derive(Default)]
pub(crate) struct Subs {
    accepted: HashMap<SessionId, Level>,
    inflight: Vec<(String, SessionId, Level)>,
}

impl Subs {
    /// Records a subscribe sent with `id` for `session` at `level`.
    pub(crate) fn sent(&mut self, id: String, session: SessionId, level: Level) {
        self.inflight.push((id, session, level));
    }

    /// The level a new subscribe for `session` must differ from: the
    /// last in-flight subscribe's level, else the accepted one.
    pub(crate) fn expected(&self, session: &SessionId) -> Option<Level> {
        self.inflight
            .iter()
            .rev()
            .find(|(_, id, _)| id == session)
            .map(|(_, _, level)| *level)
            .or_else(|| self.accepted.get(session).copied())
    }

    /// Answers the in-flight subscribe `id`: on acceptance the level
    /// becomes the accepted one. Some when the id was in flight, with
    /// its session, either way.
    pub(crate) fn answered(&mut self, id: &str, accepted: bool) -> Option<SessionId> {
        let at = self.inflight.iter().position(|(sent, _, _)| sent == id)?;
        let (_, session, level) = self.inflight.remove(at);
        if accepted {
            self.accepted.insert(session.clone(), level);
        }
        Some(session)
    }

    /// Whether the accepted level for `session` is `full`.
    pub(crate) fn full(&self, session: &SessionId) -> bool {
        self.accepted.get(session) == Some(&Level::Full)
    }

    /// Whether a subscribe for `session` waits for its answer.
    pub(crate) fn pending(&self, session: &SessionId) -> bool {
        self.inflight.iter().any(|(_, id, _)| id == session)
    }
}

/// The subscribes opening a session sends from `expected`: none or
/// `summary` needs one `full`; `full` needs `summary` then `full`, since
/// the hub resumes an exited session at the level last held and a lone
/// `full` would be refused as already at that level.
const ONE_FULL: [Level; 1] = [Level::Full];
const SUMMARY_THEN_FULL: [Level; 2] = [Level::Summary, Level::Full];

pub(crate) fn opening(expected: Option<Level>) -> &'static [Level] {
    match expected {
        Some(Level::Full) => &SUMMARY_THEN_FULL,
        _ => &ONE_FULL,
    }
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
    /// Jobs running, delegates excluded: an idle row with jobs counts as
    /// working for the quit question.
    pub(crate) jobs: u32,
    /// Delegates running: an idle row with delegates counts as working.
    pub(crate) delegates: u32,
    /// The `full` connections attached, as the latest `clients` line
    /// counts them: less this connection's own, how many open elsewhere.
    pub(crate) clients: u32,
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
    /// The scope toggle showed every project: unscoped.
    show_all: bool,
    /// The last `recent` answer carried rows: an older page may exist.
    more: bool,
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
    /// again keeps its key. An empty answer ends paging.
    pub(crate) fn recent(&mut self, rows: Vec<Row>, first: bool) {
        self.more = !rows.is_empty();
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

    /// The live feed rows, in feed order.
    pub(crate) fn live(&self) -> Vec<&Row> {
        self.feed.iter().filter(|row| row.left.is_none()).collect()
    }

    /// Flips the scope toggle: showing every project, or only the launch
    /// one.
    pub(crate) fn toggle(&mut self) {
        self.show_all = !self.show_all;
    }

    /// Whether the scope toggle showed every project.
    pub(crate) fn show_all(&self) -> bool {
        self.show_all
    }

    /// The feed rows outside `project` scope hides, and of them how many
    /// wait on the person.
    pub(crate) fn hidden(&self, project: &str) -> (usize, usize) {
        let hidden: Vec<&Row> = self
            .feed
            .iter()
            .filter(|row| row.project != project)
            .collect();
        let waiting = hidden.iter().filter(|row| row.waiting.is_some()).count();
        (hidden.len(), waiting)
    }

    /// Whether an older `recent` page may exist: the last answer carried
    /// rows.
    pub(crate) fn more(&self) -> bool {
        self.more
    }

    /// The last recent row, whose id pages the older `recent` rows.
    pub(crate) fn last_recent(&self) -> Option<&Row> {
        self.recent.last()
    }

    /// The row for `id`, live or exited.
    pub(crate) fn row(&self, id: &SessionId) -> Option<&Row> {
        self.feed
            .iter()
            .chain(self.recent.iter())
            .find(|row| row.id == *id)
    }

    /// The row with `key`, live or exited; a key naming no row opens
    /// nothing.
    pub(crate) fn by_key(&self, key: u64) -> Option<&Row> {
        self.feed
            .iter()
            .chain(self.recent.iter())
            .find(|row| row.key == key)
    }

    /// A refusal shows on the row, in place of its detail, as well as a
    /// notice; an unknown id changes nothing.
    pub(crate) fn note(&mut self, id: &SessionId, note: String) {
        for row in self.feed.iter_mut().chain(self.recent.iter_mut()) {
            if row.id == *id {
                row.note = Some(note);
                return;
            }
        }
    }

    /// Drops the row for `id`, live or exited: an accepted delete. A
    /// first-page `recent` asked again fills the gap.
    pub(crate) fn remove(&mut self, id: &SessionId) {
        self.feed.retain(|row| row.id != *id);
        self.recent.retain(|row| row.id != *id);
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
            jobs: 0,
            delegates: 0,
            clients: 0,
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
        jobs: status.jobs,
        delegates: status.delegates,
        clients: status.clients,
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
                    // A row without a last status ran nothing: no jobs,
                    // delegates or clients.
                    let (jobs, delegates, clients) = match &status {
                        None => (0, 0, 0),
                        Some(status) => (status.jobs, status.delegates, status.clients),
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
                        jobs,
                        delegates,
                        clients,
                        note: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The scope toggle's line: how many wait in other projects while
/// scoped, or how to scope back down while showing everything.
pub(crate) fn toggle_line(waiting: usize, show_all: bool) -> String {
    if show_all {
        "show this project only".to_owned()
    } else {
        format!("{waiting} waiting in other projects · show all")
    }
}

/// The quit question on the foot: how many sessions work, and of them
/// how many are also open elsewhere. With some working it asks: "2
/// sessions working · enter leave them running · c close all · esc
/// stay".
pub(crate) fn quit_line(working: usize, elsewhere: usize) -> String {
    let sessions = if working == 1 {
        "1 session working".to_owned()
    } else {
        format!("{working} sessions working")
    };
    let elsewhere = if elsewhere == 0 {
        String::new()
    } else if elsewhere == 1 {
        ", 1 also open elsewhere".to_owned()
    } else {
        format!(", {elsewhere} also open elsewhere")
    };
    format!("{sessions}{elsewhere} · enter leave them running · c close all · esc stay")
}

/// A resume line on exit: the session's id and the command resuming
/// it. The terminal is restored first, and one line prints per live
/// session.
pub(crate) fn exit_line(id: &SessionId) -> String {
    format!("{}  fiber resume {}", id.0, id.0)
}

/// The delete question on the foot: the session and that deleting is
/// permanent. Deleting through the hub is permanent, so the terminal
/// asks first, naming the session.
pub(crate) fn delete_line(row: &Row) -> String {
    format!(
        "Delete {} ({})? It cannot be undone · enter delete · esc keep",
        title(row),
        row.id.0
    )
}

/// The cascade question on the foot: the session and every session
/// `--cascade` would add, named and counted. The hub names them in its
/// refusal, and the terminal asks again with the set it would remove.
pub(crate) fn cascade_line(row: &Row, others: &[SessionId]) -> String {
    let names: Vec<&str> = others.iter().map(|id| id.0.as_str()).collect();
    let continued = if others.len() == 1 {
        format!("and 1 session that continues it: {}", names.join(", "))
    } else {
        format!(
            "and {} sessions that continue it: {}",
            others.len(),
            names.join(", ")
        )
    };
    format!(
        "Delete {} ({}) {}? It cannot be undone · enter delete all · esc keep",
        title(row),
        row.id.0,
        continued
    )
}

/// The row's name, or its id when it never had one.
fn title(row: &Row) -> &str {
    if row.name.is_empty() {
        row.id.0.as_str()
    } else {
        row.name.as_str()
    }
}

/// The sessions a hub refusal names: every `s_` and 16 lowercase hex
/// token inside backticks, once, in order. Both the dependents refusal
/// and the stale cascade refusal name the root too, so the caller puts
/// the row first and deduplicates keeping first occurrence.
pub(crate) fn dependents(message: &str) -> Vec<SessionId> {
    let mut out = Vec::new();
    let mut rest = message;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else {
            break;
        };
        let token = &rest[..close];
        rest = &rest[close + 1..];
        let id = token
            .strip_prefix("s_")
            .filter(|hex| {
                hex.len() == 16
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
            .map(|_| SessionId(token.to_owned()));
        if let Some(id) = id
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out
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
