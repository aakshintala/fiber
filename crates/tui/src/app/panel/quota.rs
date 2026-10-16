//! Quota query state for one terminal attachment (`docs/tui.md`, "The panel").

use std::time::{Duration, Instant};

use contract::{CommandId, Envelope};

use crate::view::panel::{Card, cards};
use contract::events::{CommandAccepted, CommandRejected, CommandResult, QuotaInfo};

use super::super::{App, Link, mint, session_command};

/// The cache age in `docs/tools.md`, "Fetching".
const FRESH_FOR: Duration = Duration::from_secs(5 * 60);

/// One attachment's query: never put in the conversation's pending commands.
#[derive(Debug, Default)]
enum Ask {
    Due,
    #[default]
    Idle,
    Asked(CommandId),
    Refused,
}

#[derive(Debug, Default)]
pub(crate) struct Query {
    ask: Ask,
    answered: Option<Instant>,
    /// The last answer's provider credential labels, in wire order.
    pub(crate) rows: Vec<QuotaInfo>,
    /// The session's percent-used threshold, from the same answer.
    pub(crate) notice_at: f64,
}

impl Query {
    pub(super) fn attached(&mut self) {
        *self = Self {
            ask: Ask::Due,
            ..Self::default()
        };
    }

    pub(super) fn refuse(&mut self, id: &str) {
        if matches!(&self.ask, Ask::Asked(asked) if asked.0 == id) {
            self.ask = Ask::Refused;
            self.rows.clear();
        }
    }
}

impl App {
    /// Sends on focus regardless of the last answer's age.
    pub(crate) fn quota_focus(&mut self) -> Option<String> {
        self.quota_query()
    }

    /// Sends on a keypress once the last answer is at least five minutes old.
    pub(crate) fn quota_keypress(&mut self) -> Option<String> {
        let stale = self
            .panel_state
            .quota
            .answered
            .zip(self.motion().now_instant())
            .is_some_and(|(answered, now)| now.saturating_duration_since(answered) >= FRESH_FOR);
        if stale { self.quota_query() } else { None }
    }

    pub(super) fn panel_quota(&mut self, envelope: &Envelope, ended: bool) -> Option<String> {
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = super::super::read!(envelope, CommandAccepted)
                    && matches!(&self.panel_state.quota.ask, Ask::Asked(id) if *id == accepted.command_id)
                {
                    self.panel_state.quota.ask = Ask::Idle;
                    self.panel_state.quota.answered = self.motion().now_instant();
                    self.panel_state.quota.rows.clear();
                    if let Some(CommandResult::Quota { quota, notice_at }) = accepted.result {
                        self.panel_state.quota.rows = quota;
                        self.panel_state.quota.notice_at = notice_at;
                    }
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
        if matches!(self.panel_state.quota.ask, Ask::Due) || ended {
            self.quota_query()
        } else {
            None
        }
    }

    fn quota_query(&mut self) -> Option<String> {
        if !cards(self.panel_cards(), &[]).contains(&Card::Quota) {
            return None;
        }
        if self.link != Link::Up {
            return None;
        }
        if self.attached_row().is_some_and(|row| row.left.is_some()) {
            return None;
        }
        let query = &self.panel_state.quota;
        if matches!(query.ask, Ask::Asked(_) | Ask::Refused) {
            return None;
        }
        let session = self.session()?.clone();
        let id = CommandId(mint());
        let line = session_command(&id.0, "quota", &session, None).to_string();
        self.panel_state.quota.ask = Ask::Asked(id);
        Some(line)
    }
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
