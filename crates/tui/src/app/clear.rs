//! `/clear`'s pending start and close lifecycle (`docs/tui.md`, "Clearing").

use contract::SessionId;
use serde_json::json;

use super::{App, Effect, Kind, mint, session_command};
use crate::home::Level;
use crate::input::Draft;

/// A `/clear` start waiting for its answer: the cleared session, the
/// start's id, the credential its args named, and whether its close goes
/// with `now`.
#[derive(Debug, Clone)]
pub(super) struct ClearPending {
    /// The session to close after the start is accepted.
    pub(super) old: SessionId,
    /// The id of the pending start.
    pub(super) command_id: String,
    /// The credential state the start carries.
    pub(super) credential: Option<Option<String>>,
    /// Whether the close drops a wait.
    pub(super) now: bool,
}

/// The `/clear` start and close correlations.
#[derive(Debug, Default)]
pub(super) struct Clear {
    /// The start awaiting its answer.
    pub(super) pending: Option<ClearPending>,
    /// The close awaiting its answer.
    pub(super) close: Option<String>,
}

impl App {
    /// `/clear`: starts a fresh session in the attached session's
    /// workspace, in place. The `start` carries the old session's model,
    /// its thinking level when it has one, and its credential label; the
    /// screen switches only once the hub answers with a `session_id`,
    /// and the old session is sent `close` only then. The phase stays
    /// attached to the old session until that answer.
    pub(super) fn clear(&mut self) -> Effect {
        if self.clear.pending.is_some() {
            self.draft.clear();
            return Effect::None;
        }
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        // With no status folded yet the terminal names nothing: keep
        // the draft for the retry once the first status lands.
        let Some(status) = self.panel_state.status().cloned() else {
            return Effect::None;
        };
        // A wait holds the polite close: ask first, in the overlay, and
        // send nothing yet.
        if let Some((name, waiting)) = self.clear_wait(&session) {
            return self.open_clear(session, name, waiting);
        }
        self.clear_send(status, session, false)
    }

    /// The wait holding `session`, if it waits on one: its title and
    /// kind.
    fn clear_wait(&self, session: &SessionId) -> Option<(String, String)> {
        let row = self.home.as_ref()?.sessions.row(session)?;
        let waiting = row.waiting.as_ref()?;
        let kind = waiting.split(':').next()?.to_owned();
        Some((crate::home::title(row), kind))
    }

    /// Sends the clear `start` for `old` from `status`: `now` stages
    /// whether its `close` drops the wait. The pending draft is empty,
    /// so no first prompt goes out.
    pub(super) fn clear_send(
        &mut self,
        status: contract::events::SessionStatus,
        old: SessionId,
        now: bool,
    ) -> Effect {
        if self.clear.pending.is_some() {
            return Effect::None;
        }
        let mut overrides = Vec::new();
        if let Some(thinking) = status.thinking.as_deref() {
            overrides.push(format!(
                "{}={thinking}",
                super::model_picker::canonical_thinking_key(&status.model)
            ));
        }
        let credential = self
            .panel_state
            .clear_credential()
            .map(|credential| credential.map(str::to_owned));
        if let Some(Some(label)) = credential.as_ref() {
            let provider = status.model.split('/').next().unwrap_or(&status.model);
            overrides.push(format!("providers.\"{provider}\".credential={label}"));
        }
        let mut args = json!({
            "workspace": status.workspace,
            "model": status.model,
            "worktree": false,
        });
        if let Some(object) = args.as_object_mut() {
            if !overrides.is_empty() {
                object.insert("overrides".to_owned(), json!(overrides));
            }
            if self.panel_state.worktree() {
                object.insert("continues".to_owned(), json!(old.0.clone()));
            }
        }
        let id = mint();
        self.panel_state.note_start(&args);
        let line = json!({"id": id, "command": "start", "args": args}).to_string();
        self.draft.clear();
        let effect = match self.outbox.send(
            id.clone(),
            Kind::Start,
            crate::input::Draft::default(),
            line,
        ) {
            Some(line) => Effect::Send(vec![line]),
            None => Effect::None,
        };
        self.clear.pending = Some(ClearPending {
            old,
            command_id: id,
            credential,
            now,
        });
        effect
    }

    /// Whether `/clear` has a start waiting for its answer.
    pub(super) fn clear_pending(&self) -> bool {
        self.clear.pending.is_some()
    }

    /// Accepts the matching clear start and sends its follow-up commands.
    /// Another start's answer leaves the clear pending untouched.
    pub(super) fn clear_start_accepted(
        &mut self,
        id: &str,
        session: SessionId,
    ) -> Option<Vec<String>> {
        let pending = self
            .clear
            .pending
            .take_if(|pending| pending.command_id == id)?;
        Some(self.clear_started(pending, session))
    }

    /// A clear `start` was accepted: the old session's rail number moves
    /// to the new id before any later row folds, the screen clears to an
    /// empty conversation, and the old session is sent `close` only now.
    /// The pending draft is empty, so no first prompt goes out.
    fn clear_started(&mut self, pending: ClearPending, session: SessionId) -> Vec<String> {
        self.model_picker.start_model = None;
        self.rail_state
            .replace(pending.old.clone(), session.clone());
        self.panel_state.reset();
        self.panel_state.carry_clear_credential(pending.credential);
        self.attached_screen_mut().clear();
        self.attach(session.clone());
        let id = mint();
        let close = session_command(
            &id,
            "close",
            &pending.old,
            Some(json!({"now": pending.now})),
        )
        .to_string();
        self.outbox
            .insert(id.clone(), Kind::Command, Draft::default());
        self.clear.close = Some(id);
        vec![
            self.subscribe(&session, Level::Full),
            self.ask_commands(&session),
            close,
        ]
    }

    /// Forgets a clear `close` once the hub answers it, whatever the
    /// answer says: the notice already names a refusal, and an accept
    /// needs no mark.
    pub(super) fn clear_close_settled(&mut self, id: &str) {
        if self.clear.close.as_deref() == Some(id) {
            self.clear.close = None;
        }
    }

    /// A refused clear `start`: one notice with its code and message,
    /// the phase staying attached to the old session, its draft
    /// untouched, and no `close` sent. True when `id` was the clear
    /// `start`.
    pub(super) fn clear_rejected(&mut self, id: &str, code: &str, message: &str) -> bool {
        if !self.settle_clear_start(id) {
            return false;
        }
        self.notices.push(code, message.to_owned());
        true
    }

    /// A clear line lost before it was written. A lost `start` drops
    /// its intent, the phase staying attached to the old session for
    /// the retry; a lost `close` keeps the old session running, named
    /// by one notice. True when `id` was either.
    pub(super) fn clear_lost(&mut self, id: &str) -> bool {
        if self.settle_clear_start(id) {
            return true;
        }
        if self.clear.close.as_deref() == Some(id) {
            self.clear.close = None;
            self.notices.push(
                "connection_failed",
                "The link to the old session was lost before its `close` went out.".to_owned(),
            );
            return true;
        }
        false
    }

    /// Settles the clear-staged start without its refusal notice when
    /// the hooks offer takes ownership of that refusal.
    pub(super) fn settle_clear_hooks_start(&mut self, id: &str) -> bool {
        self.settle_clear_start(id)
    }

    /// Removes the matching pending clear start and its outbox entry.
    fn settle_clear_start(&mut self, id: &str) -> bool {
        let Some(_) = self
            .clear
            .pending
            .take_if(|pending| pending.command_id == id)
        else {
            return false;
        };
        self.outbox.answered(id);
        true
    }

    /// Updates the clear's pending start id when the hooks offer resends
    /// it. Returns whether a clear start is still pending.
    pub(super) fn retry_clear_start(&mut self, refused_id: &str, retry_id: String) -> bool {
        let Some(pending) = self.clear.pending.as_mut() else {
            return false;
        };
        if pending.command_id == refused_id {
            pending.command_id = retry_id;
        }
        true
    }
}

#[cfg(test)]
#[path = "clear_tests.rs"]
mod clear_tests;
