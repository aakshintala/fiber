//! Quota query state for one terminal attachment (`docs/tui.md`, "The panel").

use std::time::{Duration, Instant};

use contract::Envelope;
use contract::events::{CommandAccepted, CommandRejected};

use super::super::{App, Link, mint, session_command};

/// The cache age in `docs/tools.md`, "Fetching".
const FRESH_FOR: Duration = Duration::from_secs(5 * 60);

/// One attachment's query: never put in the conversation's pending commands.
#[derive(Debug, Default)]
pub(super) struct Query {
    asked: Option<String>,
    due: bool,
    refused: bool,
    answered: Option<Instant>,
}

impl Query {
    pub(super) fn attached(&mut self) {
        *self = Self {
            due: true,
            ..Self::default()
        };
    }

    pub(super) fn refuse(&mut self, id: &str) {
        if self.asked.as_deref() == Some(id) {
            self.asked = None;
            self.refused = true;
        }
    }
}

impl App {
    /// Sends on focus regardless of age, or on a keypress once the last
    /// answer is at least five minutes old. Nothing polls.
    pub(crate) fn quota_poke(&mut self, focus: bool) -> Option<String> {
        let stale = self
            .panel_state
            .quota
            .answered
            .zip(self.motion().now_instant())
            .is_some_and(|(answered, now)| now.saturating_duration_since(answered) >= FRESH_FOR);
        if focus || stale {
            self.quota_query()
        } else {
            None
        }
    }

    pub(super) fn panel_quota(&mut self, envelope: &Envelope, ended: bool) -> Option<String> {
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = super::super::read!(envelope, CommandAccepted)
                    && self.panel_state.quota.asked.as_deref()
                        == Some(accepted.command_id.0.as_str())
                {
                    self.panel_state.quota.asked = None;
                    self.panel_state.quota.answered = self.motion().now_instant();
                }
            }
            "command_rejected" => {
                if let Some(rejected) = super::super::read!(envelope, CommandRejected)
                    && let Some(id) = rejected.command_id
                {
                    self.panel_state.quota.refuse(&id.0);
                }
            }
            _ => {}
        }
        if self.panel_state.quota.due || ended {
            self.quota_query()
        } else {
            None
        }
    }

    fn quota_query(&mut self) -> Option<String> {
        if !self.panel_cards().iter().any(|card| card == "quota") {
            return None;
        }
        if self.link != Link::Up {
            return None;
        }
        if self.attached_row().is_some_and(|row| row.left.is_some()) {
            return None;
        }
        let query = &self.panel_state.quota;
        if query.asked.is_some() || query.refused {
            return None;
        }
        let session = self.session()?.clone();
        let id = mint();
        let line = session_command(&id, "quota", &session, None).to_string();
        self.panel_state.quota.due = false;
        self.panel_state.quota.asked = Some(id);
        Some(line)
    }
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
