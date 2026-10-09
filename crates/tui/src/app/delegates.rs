//! The panel's Delegates card on the app side: its wheel (`docs/tui.md`,
//! "The panel": the card "scrolls on its own under the mouse wheel")
//! and its `summary` subscriptions to each running Fiber delegate
//! (`docs/tui.md`, "The panel"; `docs/invocation.md`, "A delegate is
//! reached on its own connection").

use contract::Envelope;
use contract::SessionId;
use serde_json::Value;

use super::App;
use crate::home::Level;

impl App {
    /// The wheel over the panel at screen `row`: the Delegates card's rows
    /// scroll the card, any other row the panel. The row maps to a panel
    /// row as the panel's draw places them: the first on the rect's second
    /// row, after the panel's clamped scroll.
    pub(super) fn wheel_panel(&mut self, row: u16, up: bool) {
        let Some(panel) = self.chrome().layout().and_then(|layout| layout.panel) else {
            return;
        };
        let (rows, span) = crate::view::panel::rows_and_delegates(self, panel.width);
        let height = usize::from(panel.height.saturating_sub(1));
        let skip = self
            .panel_state
            .scroll()
            .min(rows.len().saturating_sub(height));
        let over_card = row
            .checked_sub(panel.y.saturating_add(1))
            .map(|at| usize::from(at).saturating_add(skip))
            .zip(span)
            .is_some_and(|(at, span)| span.contains(&at));
        if over_card {
            self.panel_state.scroll_delegates(up);
        } else {
            self.scroll_panel(up);
        }
    }

    /// A `session_status` naming a parent, for a session other than the
    /// attached one: stored as the Delegates card's row, and taken (true),
    /// so it never becomes a home row (`docs/tui.md`, "The panel":
    /// "Delegates are not on the rail"). The attached session's own
    /// status, parented or not, folds as today.
    pub(super) fn delegate_status(&mut self, envelope: &Envelope) -> bool {
        let parented = envelope
            .payload
            .get("parent")
            .and_then(Value::as_str)
            .is_some();
        if !parented {
            return false;
        }
        if self.session() == Some(&envelope.session_id) {
            return false;
        }
        let row = crate::home::from_status(envelope);
        self.delegate_rows.insert(envelope.session_id.clone(), row);
        true
    }

    /// Each attached-session line after the fold: on the attached
    /// session's `session_status`, one `summary` subscribe per running
    /// Fiber delegate (`docs/invocation.md`, "A delegate is reached on
    /// its own connection"); on a delegate job's `job_completed`, that
    /// delegate's stored row goes, so a later run never shows a stale
    /// status (`docs/tui.md`, "The panel").
    pub(super) fn delegate_line(&mut self, envelope: &Envelope) -> Vec<String> {
        if envelope.kind == "job_completed" {
            if let Some(done) = super::read!(envelope, contract::events::JobCompleted) {
                let session = self
                    .panel_state
                    .delegate_jobs()
                    .get(&done.job_id)
                    .map(|started| started.delegate_session_id.clone());
                if let Some(session) = session {
                    self.delegate_rows.remove(&session);
                }
            }
            return Vec::new();
        }
        if envelope.kind != "session_status" {
            return Vec::new();
        }
        if !self.panel_cards().iter().any(|card| card == "delegates") {
            return Vec::new();
        }
        if self.link != super::Link::Up {
            return Vec::new();
        }
        let running: Vec<(SessionId, String)> = self
            .panel_state
            .running_delegates()
            .into_iter()
            .map(|(started, _)| (started.delegate_session_id.clone(), started.harness.clone()))
            .collect();
        let mut send = Vec::new();
        for (session, harness) in running {
            // Only a Fiber delegate streams over the hub's relay
            // (`docs/invocation.md`, "A delegate is reached on its own
            // connection").
            if harness != "fiber" {
                continue;
            }
            // A held or in-flight subscription is never sent again
            // (`docs/invocation.md`, `subscribe`).
            if self.subscribed_level(&session).is_some() {
                continue;
            }
            // A refused subscribe is tried again on the next open only.
            if !self.panel_state.try_delegate(&session) {
                continue;
            }
            // An accepted subscribe replays the latest status at once, so
            // a row cached before a reconnect never outlives the
            // re-subscribe (`docs/events.md`, `session_status`).
            self.delegate_rows.remove(&session);
            send.push(self.subscribe(&session, Level::Summary));
        }
        send
    }

    /// A delegate's latest status as a row, while its `summary`
    /// subscription lives (`docs/events.md`, `session_status`).
    pub(crate) fn delegate_row(&self, session: &SessionId) -> Option<&crate::home::Row> {
        self.delegate_rows.get(session)
    }
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;
