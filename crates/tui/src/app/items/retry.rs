//! The delegate subscription reconciler: one wish per delegate session
//! and its retry (`docs/tui.md`, "Swapped views": a delegate that has not
//! bound its socket yet answers `session_not_found`; while its parent
//! lists it running, the terminal asks again).

use std::time::{Duration, Instant};

use contract::SessionId;

use crate::app::App;
use crate::home::Level;

/// How long a `session_not_found` refusal waits before the subscribe goes
/// out again: picked, not measured.
pub(super) const RETRY: Duration = Duration::from_millis(500);

/// Whether `now` reached `retry_at`.
pub(super) fn retry_due(now: Instant, retry_at: Instant) -> bool {
    now >= retry_at
}

/// What the terminal wants a delegate session's connection at: `full`
/// while its view is open, else `summary`.
#[derive(Debug, Clone)]
pub(in crate::app) struct Want {
    /// When a refused subscribe goes out again; `None` sends on the next
    /// reconciliation.
    pub(in crate::app) retry_at: Option<Instant>,
    /// Lowering past leaving the parent: it skips the running check, so
    /// it waits behind a `full` still in flight.
    pub(in crate::app) detached: bool,
}

impl Want {
    /// A wish for an open view.
    pub(in crate::app) fn full() -> Self {
        Self {
            retry_at: None,
            detached: false,
        }
    }

    /// A wish for a closed view.
    pub(in crate::app) fn summary() -> Self {
        Self {
            retry_at: None,
            detached: false,
        }
    }
}

impl App {
    /// A delegate subscribe was refused: `session_not_found` while the
    /// parent lists it running waits [`RETRY`] from the frame's injected
    /// time and goes out again; without a frame time (only tests that
    /// drive no clock) or any other code, the wish drops and an open view
    /// shows the message.
    pub(in crate::app) fn subscribe_refused(
        &mut self,
        session: SessionId,
        code: &str,
        message: &str,
    ) {
        if code == "session_not_found"
            && self.delegate_running(&session)
            && self.connected()
            && let Some(now) = self.motion().now_instant()
        {
            let want = self
                .items
                .wants
                .entry(session)
                .or_insert_with(Want::summary);
            want.retry_at = now.checked_add(RETRY);
            return;
        }
        self.items.wants.remove(&session);
        if self.open_delegate_session().as_ref() == Some(&session)
            && let Some(open) = self.items.open.as_mut()
        {
            open.message = Some(format!("Could not open: {message}"));
        }
    }

    /// Whether the attached fold lists `session` as a running delegate.
    fn delegate_running(&self, session: &SessionId) -> bool {
        self.panel_state
            .running_delegates()
            .iter()
            .any(|(started, _)| started.delegate_session_id == *session)
    }

    /// The open fiber delegate's session, if any.
    fn open_delegate_session(&self) -> Option<SessionId> {
        let open = self.items.open.as_ref()?;
        self.items
            .jobs
            .get(&open.job_id)
            .and_then(|record| record.delegate.as_ref())
            .filter(|delegate| delegate.harness == "fiber")
            .map(|delegate| delegate.session.clone())
    }

    /// Reconciles every wish on this loop step, in order: the delegate no
    /// longer running or the link not up drops it; a subscribe in flight
    /// waits; the held level equalling the wanted one drops it; a later
    /// `retry_at` waits; otherwise one subscribe goes out and its
    /// `retry_at` clears.
    pub(crate) fn items_due(&mut self, now: Instant) -> Vec<String> {
        self.items.last_due = Some(now);
        let sessions: Vec<SessionId> = self.items.wants.keys().cloned().collect();
        let mut send = Vec::new();
        for session in sessions {
            let Some(want) = self.items.wants.get(&session).cloned() else {
                continue;
            };
            let wanted = if self.open_delegate_session().as_ref() == Some(&session) {
                Level::Full
            } else {
                Level::Summary
            };
            if !want.detached && (!self.delegate_running(&session) || !self.connected()) {
                self.items.wants.remove(&session);
                continue;
            }
            if !self.connected() {
                self.items.wants.remove(&session);
                continue;
            }
            if self.subscribe_pending(&session) {
                continue;
            }
            // A satisfied wish drops: the held level is the wanted one.
            // A scheduled retry is exempt, since the hub just refused the
            // held level as `session_not_found` and it is known stale.
            if want.retry_at.is_none() && self.subscribed_level(&session) == Some(wanted) {
                self.items.wants.remove(&session);
                continue;
            }
            if want
                .retry_at
                .is_some_and(|retry_at| !retry_due(now, retry_at))
            {
                continue;
            }
            self.items.wants.remove(&session);
            send.push(self.subscribe(&session, wanted));
        }
        send
    }

    /// The earliest retry moment among wishes with no subscribe in flight:
    /// what the loop arms the tick with after the frame. A wish in flight
    /// or already satisfied never contributes a wake, so the tick never
    /// re-arms on a passed deadline.
    pub(in crate::app) fn items_wake(&self) -> Option<Instant> {
        let mut wake: Option<Instant> = None;
        for (session, want) in self.items.wants.iter() {
            let Some(retry_at) = want.retry_at else {
                continue;
            };
            if self.items.last_due.is_some_and(|last| retry_at <= last) {
                continue;
            }
            if self.subscribe_pending(session) {
                continue;
            }
            // No satisfied check: an entry carrying a retry is never
            // satisfied, since the hub just refused its held level as
            // `session_not_found`. An entry in flight asks no wake, so
            // the tick never re-arms on a passed deadline.
            wake = Some(wake.map_or(retry_at, |wake: Instant| wake.min(retry_at)));
        }
        wake
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
