//! The dropped connection (`docs/tui.md`, "A dropped connection"): the
//! backoff between attempts, the banner that counts them, which failure
//! says so in a notice, and what a new connection sends again.
//!
//! A client that lost its connection resends every command it had no
//! answer for, with the same id (`docs/invocation.md`, "The command
//! line"). Hub commands are not resent: the hub keeps no ids, so a resent
//! `start` could start a second session. They settle as unanswered.

use std::time::Duration;

use contract::ErrorCode;
use contract::SessionId;
use serde_json::Value;

use super::{App, Link, Phase};
use crate::home::Level;
use crate::link::Line;

/// The delay after the first failure; each later one doubles it.
const FIRST: Duration = Duration::from_millis(500);

/// The longest delay between attempts.
const CAP: Duration = Duration::from_secs(30);

/// What a hub command with no answer when the connection dropped settles
/// with.
pub(super) const UNANSWERED: &str = "No answer before the connection was lost.";

/// The connection's history since the hub was last reached, and the
/// commands written that have no answer yet.
#[derive(Debug, Default)]
pub(crate) struct Reconnect {
    /// Failed connects and ended connections since the hub was last
    /// reached.
    failures: u32,
    /// The hub was reached once in this run.
    was_up: bool,
    /// Session commands written and not yet answered, in the order they
    /// went out; no id appears twice.
    kept: Vec<Kept>,
}

/// One session command written with no answer yet.
#[derive(Debug, Clone)]
struct Kept {
    /// The command's id.
    id: String,
    /// The session it names.
    session: SessionId,
    /// The line as written, sent again unchanged.
    line: String,
}

impl App {
    /// One more failure: the delay before the next attempt, doubling from
    /// half a second to at most thirty. A hub refused for its schema is
    /// never tried again.
    pub(crate) fn next_retry(&mut self) -> Option<Duration> {
        if self.link == Link::Refused {
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
        (self.link == Link::Down && failures >= 1).then(|| {
            let attempt = failures.saturating_add(u32::from(!self.reconnect.was_up));
            format!("Connection lost · reconnecting (attempt {attempt})…")
        })
    }

    /// A line was written to the hub: a session command other than
    /// `subscribe` and `commands`, which a reconnect sends afresh, is kept
    /// until its answer arrives.
    pub(crate) fn wrote(&mut self, line: &str) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let (Some(id), Some(session)) = (
            value.get("id").and_then(Value::as_str),
            value.get("session_id").and_then(Value::as_str),
        ) else {
            return;
        };
        if matches!(
            value.get("command").and_then(Value::as_str),
            Some("subscribe" | "commands")
        ) || self.reconnect.kept.iter().any(|kept| kept.id == id)
        {
            return;
        }
        self.reconnect.kept.push(Kept {
            id: id.to_owned(),
            session: SessionId(session.to_owned()),
            line: line.to_owned(),
        });
    }

    /// A command's answer, from the hub or a session: its line is no
    /// longer kept.
    pub(super) fn answered_line(&mut self, line: &Line) {
        let (kind, payload) = match line {
            Line::Hub(hub) => (hub.kind.as_str(), &hub.payload),
            Line::Session(envelope) => (envelope.kind.as_str(), &envelope.payload),
        };
        if !matches!(kind, "command_accepted" | "command_rejected") {
            return;
        }
        let Some(id) = payload.get("command_id").and_then(Value::as_str) else {
            return;
        };
        self.reconnect.kept.retain(|kept| kept.id != id);
    }

    /// A session refused command `id`: a `duplicate_command` is a resent
    /// copy of one the session already holds, so the entry closes with no
    /// notice; any other refusal returns its draft.
    pub(super) fn refused(&mut self, id: &str, code: &ErrorCode, message: String) {
        if *code == ErrorCode::DuplicateCommand {
            self.pending.remove(id);
        } else {
            self.rejected(id, message);
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
        let again = self.reconnect.was_up && self.link != Link::Up;
        self.hub_reached();
        self.reconnect.was_up = true;
        if !again {
            return Vec::new();
        }
        if let Phase::Pending { command_id } = &self.phase {
            let id = command_id.clone();
            self.rejected(&id, UNANSWERED.to_owned());
        }
        self.home_reset();
        let mut lines = Vec::new();
        let attached = self.session().cloned();
        if let Some(session) = &attached {
            let draft = std::mem::take(&mut self.draft);
            lines.extend(self.open_session(session.clone(), None));
            self.draft = draft;
        }
        let mut subscribed: Vec<SessionId> = Vec::new();
        for kept in self.reconnect.kept.clone() {
            if attached.as_ref() != Some(&kept.session) && !subscribed.contains(&kept.session) {
                lines.push(self.subscribe(&kept.session, Level::Summary));
                subscribed.push(kept.session);
            }
            lines.push(kept.line);
        }
        lines
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
