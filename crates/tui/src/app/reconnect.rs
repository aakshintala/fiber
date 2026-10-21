//! The dropped connection (`docs/tui.md`, "A dropped connection"): the
//! backoff between attempts, the banner that counts them, which failure
//! says so in a notice, and what a new connection sends again.
//!
//! Session commands written with no answer are resent with the same id
//! (`docs/invocation.md`, "The command line"). Hub commands are not resent:
//! the hub keeps no ids, so a resent `start` could start a second session.
//! They settle as unanswered.

use std::collections::HashSet;
use std::time::Duration;

use contract::ErrorCode;
use contract::SessionId;
use serde_json::Value;

use super::{App, Link, Phase};
use crate::home::Level;

/// The delay after the first failure; each later one doubles it.
const FIRST: Duration = Duration::from_millis(500);

/// The longest delay between attempts.
const CAP: Duration = Duration::from_secs(30);

/// What a hub command with no answer when the connection dropped settles
/// with.
pub(super) const UNANSWERED: &str = "No answer before the connection was lost.";

/// The connection's history since the hub was last reached.
#[derive(Debug, Default)]
pub(crate) struct Reconnect {
    /// Failed connects and ended connections since the hub was last
    /// reached.
    failures: u32,
    /// The hub was reached once in this run.
    was_up: bool,
    /// `sessions` is due on the next outgoing after a reconnect.
    sessions_due: bool,
    /// The `sessions` command waiting for its answer, if one is out.
    sessions: Option<String>,
    /// Sessions with a `session_status` after `sessions` was sent: their
    /// rows stay even when the answer omits them.
    fresh: HashSet<SessionId>,
}

impl App {
    /// One more failure: the delay before the next attempt, doubling from
    /// half a second to at most thirty. A hub refused for its schema is
    /// never tried again.
    pub(crate) fn next_retry(&mut self) -> Option<Duration> {
        if self.outbox.link == Link::Refused {
            return None;
        }
        self.reconnect.failures = self.reconnect.failures.saturating_add(1);
        let shift = self.reconnect.failures.saturating_sub(1);
        let factor = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
        Some(FIRST.saturating_mul(factor).min(CAP))
    }

    /// The banner in the working line's place while the terminal
    /// reconnects. Before the hub was first reached, the first connect was
    /// attempt 1, so the first retry is attempt 2.
    pub(crate) fn banner(&self) -> Option<String> {
        let failures = self.reconnect.failures;
        (self.outbox.link == Link::Down && failures >= 1).then(|| {
            let attempt = failures.saturating_add(u32::from(!self.reconnect.was_up));
            format!("Connection lost · reconnecting (attempt {attempt})…")
        })
    }

    /// A session refused command `id`: a `duplicate_command` is a resent
    /// copy of one the session already holds, so the entry closes with no
    /// notice; a refusal for a choice from a dropped session stays
    /// silent with it; any other refusal returns its draft.
    pub(super) fn refused(&mut self, id: &str, code: &ErrorCode, message: String) {
        if *code == ErrorCode::DuplicateCommand {
            self.outbox.answered(id);
            // A resent copy the session already holds drops the waits
            // its first answer waits on, but keeps the send's pending
            // notice: the held copy may still apply, so its claim
            // stands. `model_picker_rejected` would dismiss it.
            self.model_picker.awaiting.remove(id);
            self.model_picker.only_pending.remove(id);
            self.model_picker.sent.remove(id);
        } else if self.model_picker_rejected(id) {
            let code = crate::format::code(code);
            self.rejected(id, &code, message);
        } else {
            // The choice belonged to a dropped session: its wait went
            // with its notice, so the row closes with no further notice.
            self.outbox.answered(id);
        }
    }

    /// The hub answered `hub_hello`: the count starts again.
    pub(super) fn hub_reached(&mut self) {
        self.reconnect.failures = 0;
    }

    /// A `hub_hello` this terminal reads. After a dropped connection, the
    /// lines a new connection needs: a `start` with no answer returns to
    /// the draft, home's state for the old connection resets, the attached
    /// session opens again with the draft kept, and each kept command goes
    /// out again in its order, after a `summary` subscribe for a session
    /// not on screen. Home's feed and `recent` follow from `on_line`.
    /// The link is not up yet when this runs: a second `hub_hello` on a
    /// live connection reopens nothing.
    pub(super) fn reconnected(&mut self) -> Vec<String> {
        let again = self.reconnect.was_up && !self.outbox.is_up();
        self.hub_reached();
        self.reconnect.was_up = true;
        if !again {
            return Vec::new();
        }
        // The connection is back: both connection texts ended with it,
        // while a search the drop interrupted still waits on the bar or
        // a rescan.
        self.notices
            .clear_code_except("connection_failed", &super::find::search_incomplete_text());
        if let Phase::Pending { command_id } = &self.phase {
            let id = command_id.clone();
            self.rejected(&id, "connection_failed", UNANSWERED.to_owned());
        }
        self.home_reset();
        let mut lines = Vec::new();
        let attached = self.session().cloned();
        if let Some(session) = &attached {
            let draft = std::mem::take(&mut self.draft);
            lines.extend(self.open_session(session.clone(), None, None, true));
            self.draft = draft;
        }
        let mut subscribed: Vec<SessionId> = Vec::new();
        for (session, line) in self.outbox.reconnect_lines() {
            if attached.as_ref() != Some(&session) && !subscribed.contains(&session) {
                lines.push(self.subscribe(&session, Level::Summary));
                subscribed.push(session);
            }
            lines.push(line);
        }
        self.reconnect.sessions_due = true;
        lines
    }

    /// The `sessions` snapshot due after a reconnect, once per reconnect
    /// and after `feed`: `home_outgoing` already sent `feed` and `recent`
    /// on this `on_line` tail, so this runs after it. Nothing on the first
    /// connection.
    pub(super) fn sessions_outgoing(&mut self) -> Vec<String> {
        if !self.reconnect.sessions_due {
            return Vec::new();
        }
        self.reconnect.sessions_due = false;
        let id = super::mint();
        self.reconnect.sessions = Some(id.clone());
        self.reconnect.fresh.clear();
        vec![serde_json::json!({"id": id, "command": "sessions"}).to_string()]
    }

    /// A `session_status` folded into the rows: after `sessions` was sent,
    /// its session stays even when the answer omits it.
    pub(super) fn note_feed(&mut self, session: &SessionId) {
        if self.reconnect.sessions.is_some() {
            self.reconnect.fresh.insert(session.clone());
        }
    }

    /// The link dropped: the waiting `sessions` answer, if any, is gone,
    /// so one arriving before the next `hub_hello` matches nothing and
    /// drops no rows.
    pub(super) fn sessions_dropped(&mut self) {
        self.reconnect.sessions = None;
        self.reconnect.fresh.clear();
    }

    /// The `sessions` answer with `id`: when it is the one waiting, drop
    /// every live row its running set omits, except one with a feed line
    /// after `sessions` was sent, and take the wait off. A stale id, from
    /// before a later `sessions`, changes nothing. True when handled.
    pub(super) fn sessions_accepted(&mut self, id: &str, result: &Value) -> bool {
        if self.reconnect.sessions.as_deref() != Some(id) {
            return false;
        }
        let running: HashSet<SessionId> = result
            .get("live")
            .and_then(Value::as_array)
            .map(|live| {
                live.iter()
                    .filter_map(|entry| entry.get("session_id").and_then(Value::as_str))
                    .map(|entry| SessionId(entry.to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(home) = self.home.as_mut() {
            let stale: Vec<SessionId> = home
                .sessions
                .live()
                .iter()
                .map(|row| row.id.clone())
                .filter(|session| !running.contains(session))
                .filter(|session| !self.reconnect.fresh.contains(session))
                .collect();
            for session in stale {
                home.sessions.remove(&session);
            }
        }
        self.reconnect.sessions = None;
        self.reconnect.fresh.clear();
        true
    }

    /// A rejected `sessions` with `id`: when it is the one waiting, take
    /// the wait off and keep the rows. True when handled.
    pub(super) fn sessions_rejected(&mut self, id: &str) -> bool {
        if self.reconnect.sessions.as_deref() != Some(id) {
            return false;
        }
        self.reconnect.sessions = None;
        self.reconnect.fresh.clear();
        true
    }

    /// Whether a failed connect says so in a notice: only the first of a
    /// run of failures does, and the banner counts the rest.
    pub(super) fn notice_due(&self) -> bool {
        self.reconnect.failures == 0
    }
}

#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod reconcile_tests;
