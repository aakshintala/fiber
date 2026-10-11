//! Home's subscriptions and the open gate: this connection's level per
//! session, the session opening and the lowering lines reconciliation
//! queued.

use serde_json::Value;

use super::super::{App, Effect, Link, Phase, mint, session_command};
use super::refusal;
use crate::home::{Level, Subs};
use contract::SessionId;

/// This connection's subscriptions: the level per session, the session
/// opening and the lowering lines reconciliation queued.
#[derive(Default)]
pub(super) struct Subscriptions {
    /// This connection's level per session: the accepted one, and every
    /// subscribe waiting for its answer.
    levels: Subs,
    /// The session being opened: lines for it fold into its row and are
    /// dropped until the last subscribe's acknowledgement, which is the
    /// first line at the new level.
    opening: Option<Opening>,
    /// Lowering lines reconciliation queued, sent from `on_line`'s tail.
    outbox: Vec<String>,
}

impl Subscriptions {
    /// Records a subscribe sent with `id` for `session` at `level`.
    pub(super) fn sent(&mut self, id: String, session: SessionId, level: Level) {
        self.levels.sent(id, session, level);
    }

    /// The level a new subscribe for `session` must differ from: the
    /// last in-flight subscribe's level, else the accepted one.
    pub(super) fn expected(&self, session: &SessionId) -> Option<Level> {
        self.levels.expected(session)
    }

    /// Answers the in-flight subscribe `id`: on acceptance the level
    /// becomes the accepted one. Some when the id was in flight, with
    /// its session, either way.
    pub(super) fn answered(&mut self, id: &str, accepted: bool) -> Option<SessionId> {
        self.levels.answered(id, accepted)
    }

    /// Whether the accepted level for `session` is `full`.
    pub(super) fn full(&self, session: &SessionId) -> bool {
        self.levels.full(session)
    }

    /// Whether a subscribe for `session` waits for its answer.
    pub(super) fn pending(&self, session: &SessionId) -> bool {
        self.levels.pending(session)
    }

    /// Opens the gate for `session`: lines for it fold into its row and
    /// are dropped until the last subscribe's acknowledgement, whose id
    /// is `ack`.
    pub(super) fn gate(&mut self, session: SessionId, ack: String) {
        self.opening = Some(Opening { session, ack });
    }

    /// Clears the old connection's levels and open gate, keeping the
    /// queued lowering lines.
    pub(super) fn reset(&mut self) {
        self.levels = Subs::default();
        self.opening = None;
    }

    /// Takes the lowering lines reconciliation queued.
    pub(super) fn take_outbox(&mut self) -> Vec<String> {
        std::mem::take(&mut self.outbox)
    }
}

/// A session opening from home and its last subscribe, whose
/// acknowledgement ends the gate.
struct Opening {
    /// The session opening.
    session: SessionId,
    /// The last subscribe's id: only its rejection fails the open.
    ack: String,
}

/// A `subscribe` line for `session` at `level`, with its id.
pub(super) fn subscribe_line(session: &SessionId, level: Level) -> (String, String) {
    let id = mint();
    let name = match level {
        Level::Summary => "summary",
        Level::Full => "full",
    };
    let line = session_command(&id, "subscribe", session, Some(serde_json::json!({"level": name})))
        .to_string();
    (id, line)
}

impl App {
    /// A `subscribe` line for `session` at `level`, recorded as sent:
    /// the expected level is the last in-flight one, so no path sends a
    /// subscribe at the level already held or asked for.
    pub(in crate::app) fn subscribe(&mut self, session: &SessionId, level: Level) -> String {
        let (id, line) = subscribe_line(session, level);
        if let Some(home) = self.home.as_mut() {
            home.subs.sent(id, session.clone(), level);
        }
        line
    }

    /// The level this connection holds or asks for `session` at, if any:
    /// a delegate is subscribed once per attachment (`docs/invocation.md`,
    /// `subscribe`).
    pub(in crate::app) fn subscribed_level(&self, session: &SessionId) -> Option<Level> {
        self.home
            .as_ref()
            .and_then(|home| home.subs.expected(session))
    }

    /// Whether a subscribe for `session` waits for its answer.
    pub(in crate::app) fn subscribe_pending(&self, session: &SessionId) -> bool {
        self.home
            .as_ref()
            .is_some_and(|home| home.subs.pending(session))
    }

    /// Leaves the session on screen for home: the conversation cleared
    /// with the session left running, then lowered to `summary` when this
    /// connection holds it at `full` and its row is live. `/close` keeps
    /// `go_home` and lowers nothing: a command for an exited session
    /// would resume it.
    pub(in crate::app) fn leave(&mut self) -> Effect {
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

    /// Opens the row with `key` while home draws.
    pub(super) fn open_row(&mut self, key: u64) -> Effect {
        if !self.on_home() {
            return Effect::None;
        }
        self.open_entry(key)
    }

    /// Opens the row with `key`: the subscribes its level needs, then the
    /// session's commands. The conversation clears as going home does,
    /// and the gate holds until the last subscribe is answered.
    pub(in crate::app) fn open_entry(&mut self, key: u64) -> Effect {
        if self.link != Link::Up || matches!(self.phase, Phase::Pending { .. }) {
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
        Effect::Send(self.open_session(session, expected))
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
            .and_then(|home| home.subs.opening.as_ref())
            .is_some_and(|opening| opening.ack == id)
    }

    /// Lowers `session` to `summary` when this connection holds it at
    /// `full` without showing it: not the attached one, nothing in
    /// flight, and its row live. Untouched live sessions get no
    /// subscribe: their rows come from the hub's feed, which the hub
    /// reads over its own connections, and subscribing each one would
    /// send a command to every live session.
    pub(super) fn reconcile(&mut self, session: &SessionId) {
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
                home.subs.outbox.push(line);
            }
        }
    }

    /// A rejection of the open's last subscribe fails the open: home again
    /// when still attached, with the message as the row's note and a
    /// notice. A rejection of an earlier step changes nothing by itself.
    fn fail_open(&mut self, message: String) -> Vec<String> {
        let session = self
            .home
            .as_mut()
            .and_then(|home| home.subs.opening.take())
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

    /// Folds one hub subscribe acknowledgement into the levels: a
    /// refusal becomes the row's note, and a refusal of the open's last
    /// subscribe fails the open. Some with the lines to send when `id`
    /// was an in-flight subscribe, else `None`.
    pub(super) fn subscribe_answer(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &serde_json::Map<String, Value>,
    ) -> Option<Vec<String>> {
        if let Some(session) = self.answered(id, accepted) {
            if !accepted {
                let code = payload
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.subscribe_refused(session, code, &refusal(payload));
            }
            if !accepted && self.is_ack(id) {
                return Some(self.fail_open(refusal(payload)));
            }
            return Some(Vec::new());
        }
        None
    }

    /// Folds an accepted subscribe on `session`'s stream past the gate:
    /// a session held at `full` reconciles first, then the open's last
    /// subscribe ends the gate and its line goes on to `on_line`. Some
    /// with the lines to send, or `None` when the gate ended.
    pub(super) fn subscribe_accepted(
        &mut self,
        session: &SessionId,
        id: &str,
    ) -> Option<Vec<String>> {
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.subs.full(session))
        {
            self.reconcile(session);
        }
        if self.is_ack(id) {
            if let Some(home) = self.home.as_mut() {
                home.subs.opening = None;
            }
            return None;
        }
        Some(Vec::new())
    }

    /// Whether `session` is opening: its lines fold into its row and are
    /// dropped until the last subscribe's acknowledgement.
    pub(super) fn gated(&self, session: &SessionId) -> bool {
        self.home
            .as_ref()
            .and_then(|home| home.subs.opening.as_ref())
            .is_some_and(|opening| opening.session == *session)
    }
}
