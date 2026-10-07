//! Home's state on the app: the launch description, whether a `start`
//! went out, and what home draws (`docs/tui.md`, "Home"). The data it
//! draws lives in [`crate::home`]; this module is `App`'s home.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::{App, Effect, Link, Phase, mint, session_command};
use crate::focus::{Area, order};
use crate::home::{
    HomeScreen, Launch, Left, Level, Sessions, Spot, Subs, from_status, line, opening, recent_rows,
};
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use contract::SessionId;

/// Home's state: the launch description, and whether a `start` went out in
/// this run, which hides the input box's placeholder.
pub(super) struct Home {
    /// What the terminal knows about where it was launched.
    launch: Launch,
    /// A `start` went out in this run.
    prompted: bool,
    /// The session list: live rows from the feed, exited rows from `recent`.
    sessions: Sessions,
    /// `feed` and the first `recent` page went out.
    fed: bool,
    /// The `feed` command waiting for its answer.
    feed_id: Option<String>,
    /// The latest `recent` command waiting for its answer, and whether it
    /// asked the first page.
    recent_ask: Option<(String, bool)>,
    /// This connection's subscription level per session: the accepted
    /// one, and every subscribe waiting for its answer.
    subs: Subs,
    /// The session being opened: lines for it fold into its row and are
    /// dropped until the last subscribe's acknowledgement, which is the
    /// first line at the new level.
    opening: Option<Opening>,
    /// Lowering lines reconciliation queued, sent from `on_line`'s tail.
    outbox: Vec<String>,
    /// `/resume` asks the next frame to focus the list.
    focus_list: bool,
}

/// A session opening from home: its subscribes, the last one whose
/// acknowledgement ends the gate, and whether the same-level retry ran.
struct Opening {
    /// The session opening.
    session: SessionId,
    /// Every subscribe id the open sent, the retry's included.
    ids: Vec<String>,
    /// The last subscribe's id: only its rejection fails the open.
    ack: String,
    /// The one-step retry already ran.
    retried: bool,
}

impl App {
    /// Stores the launch description as home, keying prompt history by its
    /// project as the run argument did. The app keeps today's screen, so
    /// the jigs and every existing app test stay byte-identical.
    /// Stores the launch description as home, keying prompt history by its
    /// project as the run argument did. The app keeps today's screen, so
    /// the jigs and every existing app test stay byte-identical.
    pub(crate) fn set_home(&mut self, launch: Launch) {
        self.set_project(launch.project.clone());
        self.home = Some(Home {
            launch,
            prompted: false,
            sessions: Sessions::default(),
            fed: false,
            feed_id: None,
            recent_ask: None,
            subs: Subs::default(),
            opening: None,
            outbox: Vec::new(),
            focus_list: false,
        });
    }

    /// Whether home draws: home is set, no session is attached, and neither
    /// the key map nor the approval panel covers the screen.
    pub(crate) fn on_home(&self) -> bool {
        self.home.is_some()
            && self.session().is_none()
            && self.keymap_top().is_none()
            && self.panel().is_none()
    }

    /// What home draws, or `None` unless [`App::on_home`] holds.
    pub(crate) fn home_screen(&self) -> Option<HomeScreen> {
        let home = self.home.as_ref().filter(|_| self.on_home())?;
        let segment = home
            .launch
            .workspace
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| home.launch.workspace.display().to_string());
        Some(HomeScreen {
            version: home.launch.version.clone(),
            // A remote client has no launch directory; the picker it would
            // always show is a later ticket's.
            glyph: home.launch.logo_glyph.clone(),
            chips: vec![format!("[{segment}]"), "enter starts a session".to_owned()],
            rows: home
                .sessions
                .shown(&home.launch.project, false)
                .iter()
                .map(|row| (row.key, line(row, &home.launch.project), false))
                .collect(),
            foot: if self.armed_at.is_some() {
                super::QUIT_HINT.to_owned()
            } else {
                "↓ the session list · F1 the key map · Ctrl+C twice to quit".to_owned()
            },
            // Until the first prompt, the placeholder says "/? for
            // shortcuts"; no session exists before Enter, so opening the
            // terminal, glancing at home and leaving creates nothing.
            placeholder: self.draft.is_empty() && !home.prompted,
        })
    }

    /// The hub lines home asks for, once the link is up: `feed`, then the
    /// first `recent` page. The session list fills in when the hub's feed
    /// arrives.
    pub(super) fn home_outgoing(&mut self) -> Vec<String> {
        if self.link != Link::Up {
            return Vec::new();
        }
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        if home.fed {
            return std::mem::take(&mut home.outbox);
        }
        home.fed = true;
        let feed = mint();
        let recent = mint();
        home.feed_id = Some(feed.clone());
        home.recent_ask = Some((recent.clone(), true));
        vec![
            json!({"id": feed, "command": "feed"}).to_string(),
            json!({"id": recent, "command": "recent"}).to_string(),
        ]
    }

    /// Folds one hub line into the session list, with the lines to send;
    /// `None` means `on_line` goes on as today. A hub `session_left` and
    /// every `session_status` fold into the rows; the answers to `feed`
    /// and the latest `recent` fill the list, and their refusals are one
    /// notice. Every subscribe acknowledgement updates the levels, and
    /// while a session opens its lines fold into its row and are dropped
    /// until the last subscribe's acknowledgement. `attention` and other
    /// hub lines pass through untouched, and a status still reaches the
    /// conversation below.
    pub(super) fn home_line(&mut self, line: &Line) -> Option<Vec<String>> {
        self.home.as_ref()?;
        match line {
            Line::Hub(hub) => match hub.kind.as_str() {
                "session_left" => {
                    let id = hub.payload.get("session_id").and_then(Value::as_str)?;
                    let how = match hub.payload.get("how").and_then(Value::as_str) {
                        Some("exited") => Left::Exited,
                        Some("crashed") => Left::Crashed,
                        _ => return Some(Vec::new()),
                    };
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.left(&SessionId(id.to_owned()), how);
                    }
                    Some(Vec::new())
                }
                "command_accepted" | "command_rejected" => {
                    let id = hub.payload.get("command_id").and_then(Value::as_str)?;
                    let accepted = hub.kind.as_str() == "command_accepted";
                    if self.answered(id, accepted).is_some() {
                        if !accepted && self.is_ack(id) {
                            let (code, message) = refusal_parts(&hub.payload);
                            return Some(self.retry_or_fail(code, message));
                        }
                        return Some(Vec::new());
                    }
                    let is_feed =
                        self.home.as_ref().and_then(|home| home.feed_id.as_deref()) == Some(id);
                    if is_feed {
                        if let Some(home) = self.home.as_mut() {
                            home.feed_id = None;
                        }
                        if !accepted {
                            self.notices.push(refusal(&hub.payload));
                        }
                        return Some(Vec::new());
                    }
                    let recent = self
                        .home
                        .as_ref()
                        .and_then(|home| home.recent_ask.as_ref())
                        .filter(|(asked, _)| asked == id)
                        .map(|(_, first)| *first);
                    if let Some(first) = recent {
                        if let Some(home) = self.home.as_mut() {
                            home.recent_ask = None;
                        }
                        if accepted {
                            let rows = hub
                                .payload
                                .get("result")
                                .map(recent_rows)
                                .unwrap_or_default();
                            if let Some(home) = self.home.as_mut() {
                                home.sessions.recent(rows, first);
                            }
                        } else {
                            self.notices.push(refusal(&hub.payload));
                        }
                        return Some(Vec::new());
                    }
                    None
                }
                _ => None,
            },
            Line::Session(envelope) => {
                if envelope.kind == "session_status" {
                    let session = envelope.session_id.clone();
                    let was_left = self
                        .home
                        .as_ref()
                        .and_then(|home| home.sessions.row(&session))
                        .is_some_and(|row| row.left.is_some());
                    let row = from_status(envelope);
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.status(row);
                    }
                    if was_left {
                        self.reconcile(&session);
                    }
                    let gated = self
                        .home
                        .as_ref()
                        .and_then(|home| home.opening.as_ref())
                        .is_some_and(|opening| opening.session == session);
                    if gated {
                        return Some(Vec::new());
                    }
                    return None;
                }
                if matches!(
                    envelope.kind.as_str(),
                    "command_accepted" | "command_rejected"
                ) {
                    let accepted = envelope.kind == "command_accepted";
                    let id = envelope.payload.get("command_id").and_then(Value::as_str)?;
                    if self.answered(id, accepted).is_some() {
                        if !accepted && self.is_ack(id) {
                            let (code, message) = refusal_parts(&envelope.payload);
                            return Some(self.retry_or_fail(code, message));
                        }
                        let session = envelope.session_id.clone();
                        if accepted
                            && self
                                .home
                                .as_ref()
                                .is_some_and(|home| home.subs.full(&session))
                        {
                            self.reconcile(&session);
                        }
                        if accepted && self.is_ack(id) {
                            if let Some(home) = self.home.as_mut() {
                                home.opening = None;
                            }
                            return None;
                        }
                        return Some(Vec::new());
                    }
                }
                let gated = self
                    .home
                    .as_ref()
                    .and_then(|home| home.opening.as_ref())
                    .is_some_and(|opening| opening.session == envelope.session_id);
                if gated {
                    return Some(Vec::new());
                }
                None
            }
        }
    }

    /// A `subscribe` line for `session` at `level`, recorded as sent:
    /// the expected level is the last in-flight one, so no path sends a
    /// subscribe at the level already held or asked for.
    pub(super) fn subscribe(&mut self, session: &SessionId, level: Level) -> String {
        let (id, line) = subscribe_line(session, level);
        if let Some(home) = self.home.as_mut() {
            home.subs.sent(id, session.clone(), level);
        }
        line
    }

    /// Leaves the session on screen for home: the conversation cleared
    /// with the session left running, then lowered to `summary` when this
    /// connection holds it at `full` and its row is live. `/close` keeps
    /// `go_home` and lowers nothing: a command for an exited session
    /// would resume it.
    pub(super) fn leave(&mut self) -> Effect {
        let attached = self.session().cloned();
        self.go_home();
        let Some(session) = attached else {
            return Effect::None;
        };
        let lowers = self.link == Link::Up
            && self.home.as_ref().is_some_and(|home| {
                home.subs.expected(&session) == Some(Level::Full)
                    && home
                        .sessions
                        .row(&session)
                        .is_some_and(|row| row.left.is_none())
            });
        if lowers {
            Effect::Send(vec![self.subscribe(&session, Level::Summary)])
        } else {
            Effect::None
        }
    }

    /// Opens home at the session list: home, with the next frame
    /// focusing the list, or the input box when the list is empty.
    pub(super) fn resume_list(&mut self) -> Effect {
        let effect = self.leave();
        if let Some(home) = self.home.as_mut() {
            home.focus_list = true;
        }
        effect
    }

    /// Focuses the list after `/resume`: the first list stop among the
    /// new targets, leaving focus in the box when the list is empty.
    /// True when it ran, so the frame draws again with the focus shown.
    pub(super) fn home_drawn(&mut self, targets: &[Target]) -> bool {
        let Some(home) = self.home.as_mut() else {
            return false;
        };
        if !home.focus_list {
            return false;
        }
        home.focus_list = false;
        self.focus = order(targets, &self.regions, Area::Conversation)
            .into_iter()
            .find(|id| matches!(id, TargetId::Home(Spot::Entry(_))));
        true
    }

    /// A key for home, ahead of the key map and focus: down in an empty
    /// box focuses the first row, and down on the last drawn row focuses
    /// the next one below the fold. `None` for anything else, so the
    /// focused stops keep moving as they do on the conversation.
    pub(super) fn home_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.on_home() || !matches!(key, Key::Down | Key::Char('j')) {
            return None;
        }
        if self.focus.is_none() {
            if !self.draft.is_empty() || self.completions().is_some() {
                return None;
            }
            let first = order(&self.stops, &self.regions, Area::Conversation)
                .into_iter()
                .find(|id| matches!(id, TargetId::Home(Spot::Entry(_))));
            if let Some(id) = first {
                self.focus = Some(id);
                return Some(Effect::None);
            }
            return None;
        }
        let Some(TargetId::Home(Spot::Entry(focused))) = self.focus else {
            return None;
        };
        let ordered = order(&self.stops, &self.regions, Area::Conversation);
        let last = ordered
            .iter()
            .rev()
            .find(|id| matches!(id, TargetId::Home(Spot::Entry(_))));
        if last != Some(&TargetId::Home(Spot::Entry(focused))) {
            return None;
        }
        let next = self.home.as_ref().and_then(|home| {
            let shown = home.sessions.shown(&home.launch.project, false);
            let at = shown.iter().position(|row| row.key == focused)?;
            shown.get(at + 1).map(|row| row.key)
        });
        if let Some(key) = next {
            self.focus = Some(TargetId::Home(Spot::Entry(key)));
            return Some(Effect::None);
        }
        None
    }

    /// Clicks `spot` on home: a row opens its session.
    pub(super) fn home_click(&mut self, spot: Spot) -> Effect {
        match spot {
            Spot::Entry(key) => self.open_row(key),
        }
    }

    /// The text y copies and Ctrl+G opens for `spot`: the row's line.
    pub(super) fn home_text(&self, spot: Spot) -> Option<String> {
        match spot {
            Spot::Entry(key) => {
                let home = self.home.as_ref()?;
                let row = home.sessions.by_key(key)?;
                Some(line(row, &home.launch.project))
            }
        }
    }

    /// The workspace in use on home: the attached session's row
    /// workspace, else the launch workspace.
    pub(super) fn home_workspace(&self) -> PathBuf {
        if let Some(home) = &self.home {
            if let Some(session) = self.session()
                && let Some(row) = home.sessions.row(session)
            {
                return PathBuf::from(&row.workspace);
            }
            return home.launch.workspace.clone();
        }
        self.workspace.clone()
    }

    /// Opens the row with `key`: the subscribes its level needs, then the
    /// session's commands. The conversation clears as going home does,
    /// and the gate holds until the last subscribe is answered.
    fn open_row(&mut self, key: u64) -> Effect {
        if !self.on_home() || self.link != Link::Up || matches!(self.phase, Phase::Pending { .. }) {
            return Effect::None;
        }
        let Some(row) = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
        else {
            return Effect::None;
        };
        if row.state == crate::home::State::Unreadable {
            self.notices.push(
                "Cannot attach: this session's schema is newer than this terminal reads."
                    .to_owned(),
            );
            return Effect::None;
        }
        let session = row.id.clone();
        let expected = self
            .home
            .as_ref()
            .and_then(|home| home.subs.expected(&session));
        self.go_home();
        self.attach(session.clone());
        let mut ids = Vec::new();
        let mut lines = Vec::new();
        for level in opening(expected) {
            let (id, line) = subscribe_line(&session, *level);
            if let Some(home) = self.home.as_mut() {
                home.subs.sent(id.clone(), session.clone(), *level);
            }
            ids.push(id);
            lines.push(line);
        }
        lines.push(self.ask_commands(&session));
        let ack = ids.last().cloned().unwrap_or_default();
        if let Some(home) = self.home.as_mut() {
            home.opening = Some(Opening {
                session,
                ids,
                ack,
                retried: false,
            });
        }
        Effect::Send(lines)
    }

    /// Records the acknowledgement of the in-flight subscribe `id`:
    /// the accepted level changes only on acceptance. Some with its
    /// session when the id was in flight, either way.
    fn answered(&mut self, id: &str, accepted: bool) -> Option<SessionId> {
        self.home
            .as_mut()
            .and_then(|home| home.subs.answered(id, accepted))
    }

    /// Whether `id` is the open's last subscribe: only its rejection
    /// fails the open, and only its acceptance ends the gate.
    fn is_ack(&self, id: &str) -> bool {
        self.home
            .as_ref()
            .and_then(|home| home.opening.as_ref())
            .is_some_and(|opening| opening.ack == id)
    }

    /// Lowers `session` to `summary` when this connection holds it at
    /// `full` without showing it: not the attached one, nothing in
    /// flight, and its row live. Untouched live sessions get no
    /// subscribe: their rows come from the hub's feed, which the hub
    /// reads over its own connections, and subscribing each one would
    /// send a command to every live session.
    fn reconcile(&mut self, session: &SessionId) {
        let lowers = self.link == Link::Up
            && self.session() != Some(session)
            && self.home.as_ref().is_some_and(|home| {
                home.subs.full(session)
                    && !home.subs.pending(session)
                    && home
                        .sessions
                        .row(session)
                        .is_some_and(|row| row.left.is_none())
            });
        if lowers {
            let line = self.subscribe(session, Level::Summary);
            if let Some(home) = self.home.as_mut() {
                home.outbox.push(line);
            }
        }
    }

    /// A rejection of the open's last subscribe: a same-level refusal of
    /// a one-step open retries once with `summary` then `full`, since
    /// the hub keeps a connection's first subscribe even when rejected
    /// and replays it on resume, dropping its acknowledgement. A second
    /// same-level refusal, or any other code, fails the open: home again
    /// when still attached, with the message as the row's note and a
    /// notice. A rejection of an earlier step changes nothing by itself.
    fn retry_or_fail(&mut self, code: &str, message: String) -> Vec<String> {
        let retry = self
            .home
            .as_ref()
            .and_then(|home| home.opening.as_ref())
            .is_some_and(|opening| {
                code == "invalid_arguments" && opening.ids.len() == 1 && !opening.retried
            });
        if retry {
            let session = self
                .home
                .as_ref()
                .and_then(|home| home.opening.as_ref())
                .map(|opening| opening.session.clone());
            let Some(session) = session else {
                return Vec::new();
            };
            let mut ids = Vec::new();
            let mut lines = Vec::new();
            for level in [Level::Summary, Level::Full] {
                let (id, line) = subscribe_line(&session, level);
                if let Some(home) = self.home.as_mut() {
                    home.subs.sent(id.clone(), session.clone(), level);
                }
                ids.push(id);
                lines.push(line);
            }
            let ack = ids.last().cloned().unwrap_or_default();
            if let Some(home) = self.home.as_mut()
                && let Some(opening) = home.opening.as_mut()
            {
                opening.ids = ids;
                opening.ack = ack;
                opening.retried = true;
            }
            return lines;
        }
        let session = self
            .home
            .as_mut()
            .and_then(|home| home.opening.take())
            .map(|opening| opening.session);
        let Some(session) = session else {
            return Vec::new();
        };
        if let Some(home) = self.home.as_mut() {
            home.sessions.note(&session, message.clone());
        }
        let attached = self.session() == Some(&session);
        self.notices.push(message);
        if attached {
            self.go_home();
        }
        Vec::new()
    }

    /// The `start` args for `content`: the launch workspace on home, else
    /// the launch directory as today. Sending one hides the placeholder
    /// until the run ends.
    pub(super) fn start_args(&mut self, content: Value) -> Value {
        match &mut self.home {
            Some(home) => {
                home.prompted = true;
                json!({
                    "workspace": home.launch.workspace.display().to_string(),
                    "content": content,
                })
            }
            None => json!({
                "workspace": self.workspace.display().to_string(),
                "content": content,
            }),
        }
    }
}

/// A `subscribe` line for `session` at `level`, with its id.
fn subscribe_line(session: &SessionId, level: Level) -> (String, String) {
    let id = mint();
    let name = match level {
        Level::Summary => "summary",
        Level::Full => "full",
    };
    let line = session_command(&id, "subscribe", session, Some(json!({"level": name}))).to_string();
    (id, line)
}

/// A refusal's code and message, for the open's acknowledgement.
fn refusal_parts(payload: &serde_json::Map<String, Value>) -> (&str, String) {
    let code = payload
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    (code, refusal(payload))
}
/// A hub refusal's message, for a notice.
fn refusal(payload: &serde_json::Map<String, Value>) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("rejected")
        .to_owned()
}

#[cfg(test)]
#[path = "app_home_tests.rs"]
mod tests;
