//! Quota query state for one terminal attachment (`docs/tui.md`, "The panel").

use std::time::{Duration, Instant};

use contract::{CommandId, Envelope};

use crate::view::panel::{Card, cards};
use contract::events::{CommandAccepted, CommandRejected, CommandResult, QuotaInfo};
use serde_json::json;

use super::super::{App, Effect, Link, mint, session_command};

/// The cache age in `docs/tools.md`, "Fetching".
const FRESH_FOR: Duration = Duration::from_secs(5 * 60);

/// One attachment's query: never put in the conversation's pending commands.
#[derive(Debug, Default)]
enum Ask {
    Due,
    #[default]
    Idle,
    /// The panel's own fetch-less query.
    Asked(CommandId),
    /// The quota view's `fetch: true` query.
    Fetching(CommandId),
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

    /// A refusal ends the flight but leaves the cached rows drawn: the
    /// view keeps showing its last answer under a rejected refetch.
    pub(super) fn refuse(&mut self, id: &str) {
        if matches!(&self.ask, Ask::Asked(asked) | Ask::Fetching(asked) if asked.0 == id) {
            self.ask = Ask::Refused;
        }
    }

    /// Whether a query is in flight: the quota view shows `fetching`
    /// over the cached answer until its answer lands.
    pub(crate) fn fetching(&self) -> bool {
        matches!(self.ask, Ask::Asked(_) | Ask::Fetching(_))
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
                if let Some(accepted) = super::super::read!(envelope, CommandAccepted) {
                    // Every quota answer folds into the card, whatever
                    // asked for it: the view's fetch redraws the panel
                    // card too. Only the panel's own flight is cleared by
                    // its id.
                    if let Some(CommandResult::Quota { quota, notice_at }) = accepted.result {
                        self.panel_state.quota.rows = quota;
                        self.panel_state.quota.notice_at = notice_at;
                    }
                    if matches!(&self.panel_state.quota.ask, Ask::Asked(id) | Ask::Fetching(id) if *id == accepted.command_id)
                    {
                        self.panel_state.quota.ask = Ask::Idle;
                        self.panel_state.quota.answered = self.motion().now_instant();
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
        if self.outbox.link != Link::Up {
            return None;
        }
        if self.attached_row().is_some_and(|row| row.left.is_some()) {
            return None;
        }
        let query = &self.panel_state.quota;
        if matches!(query.ask, Ask::Asked(_) | Ask::Fetching(_) | Ask::Refused) {
            return None;
        }
        let session = self.session()?.clone();
        let id = CommandId(mint());
        let line = session_command(&id.0, "quota", &session, None).to_string();
        self.panel_state.quota.ask = Ask::Asked(id);
        Some(line)
    }

    /// Opens the quota view and fetches every label's quota: the view
    /// draws the cached answer at once and redraws on the fetch's answer,
    /// which also updates the panel card. The draft is left for panel
    /// and status-line clicks; a slash command clears it before calling
    /// this method.
    pub(crate) fn quota_view(&mut self) -> Effect {
        self.open_session_view(super::super::SessionView::Quota);
        match self.quota_fetch() {
            Some(line) => Effect::Send(vec![line]),
            None => Effect::None,
        }
    }

    /// Sends one `quota` with `fetch: true`, unless a fetch is already in
    /// flight: its answer redraws the view. A panel fetch-less query in
    /// flight is replaced by the fetch, so opening the view always
    /// fetches. An explicit open retries after a refusal. Never put in
    /// the conversation's pending commands: the answer folds into the
    /// panel card's rows by its id.
    fn quota_fetch(&mut self) -> Option<String> {
        if self.outbox.link != Link::Up {
            return None;
        }
        if self.attached_row().is_some_and(|row| row.left.is_some()) {
            return None;
        }
        let query = &mut self.panel_state.quota;
        if matches!(query.ask, Ask::Fetching(_)) {
            return None;
        }
        let session = self.session()?.clone();
        let id = CommandId(mint());
        let line =
            session_command(&id.0, "quota", &session, Some(json!({"fetch": true}))).to_string();
        self.panel_state.quota.ask = Ask::Fetching(id);
        Some(line)
    }
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
