//! Settlement of the confirmed `/delete` answer: remove confirmed rows,
//! preserve the Hub's refusal context, and keep stop outcomes truthful.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::app::App;
use contract::SessionId;

use super::{State, refusal, refusal_code};

impl App {
    /// Settles the command's exact-set delete: accepted removes every
    /// confirmed row; a refusal is formatted by its stable code and never
    /// retries. `None` when the answer is not this command's exact ask.
    pub(super) fn delete_command_delete_answer(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> Option<Vec<String>> {
        let (target, sessions, captured_name) = self.delete_deleting_sets(id)?;
        if accepted {
            let project = self
                .home
                .as_ref()
                .and_then(|home| home.sessions.row(&target))
                .map(|row| row.project.clone());
            if let Some(home) = self.home.as_mut() {
                for session in &sessions {
                    home.sessions.remove(session);
                }
            }
            let lines = self.ask_recent_first(project.as_deref());
            self.delete_command_finish(false);
            self.go_home();
            return Some(lines);
        }

        let code = refusal_code(payload);
        let message = refusal(payload);
        if code == "session_held" {
            let (first, more) = held_refusal_parts(payload, &target, &sessions);
            let display = self
                .home
                .as_ref()
                .and_then(|home| home.sessions.row(&first))
                .map(crate::home::title)
                .unwrap_or(captured_name);
            let notice = if more == 0 {
                format!("Not deleted: {display} is running again. Nothing was deleted.")
            } else {
                format!(
                    "Not deleted: {display} is running again and {more} more. Nothing was deleted."
                )
            };
            self.delete_command_finish(false);
            self.notices.push("session_held", notice);
            return Some(Vec::new());
        }
        if code == "stale_request" {
            let project = self
                .home
                .as_ref()
                .and_then(|home| home.sessions.row(&target))
                .map(|row| row.project.clone());
            let lines = self.ask_recent_first(project.as_deref());
            self.delete_command_finish(false);
            self.notices
                .push(&code, format!("{message} The sessions remain stopped."));
            self.go_home();
            return Some(lines);
        }

        let project = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.row(&target))
            .map(|row| row.project.clone());
        let lines = self.ask_recent_first(project.as_deref());
        self.delete_command_finish(true);
        self.notices
            .push(&code, format!("Delete failed: {message}"));
        self.go_home();
        Some(lines)
    }

    /// Settles a sent slash delete on reconnect, or drops a lost
    /// discovery silently. The exact ask is removed before generic row
    /// delete settlement can see it.
    pub(in crate::app::home::prompt) fn delete_command_disconnected(&mut self) {
        let deleting =
            self.home
                .as_ref()
                .and_then(|home| match &home.questions.delete_command.state {
                    State::Deleting {
                        command, sessions, ..
                    } => Some((command.clone(), sessions.clone())),
                    State::Idle
                    | State::Discovering { .. }
                    | State::Asked { .. }
                    | State::Stopping { .. } => None,
                });
        if let Some((command, _sessions)) = deleting {
            self.take_ask(&command);
            self.delete_command_delete_unknown();
            return;
        }
        if let Some(home) = self.home.as_mut()
            && matches!(
                home.questions.delete_command.state,
                State::Discovering { .. }
            )
        {
            home.questions.delete_command.state = State::Idle;
        }
    }

    /// Settles a stopping sequence immediately when the live hub link
    /// drops. Waiting rows are not evidence of exit; already-left rows
    /// remain excluded from the process's resume lines.
    pub(in crate::app) fn delete_command_link_lost(&mut self) {
        let waiting =
            self.home
                .as_ref()
                .and_then(|home| match &home.questions.delete_command.state {
                    State::Stopping { waiting, name, .. } => Some((waiting.clone(), name.clone())),
                    State::Idle
                    | State::Discovering { .. }
                    | State::Asked { .. }
                    | State::Deleting { .. } => None,
                });
        let Some((waiting, fallback)) = waiting else {
            return;
        };
        let display = waiting
            .first()
            .and_then(|id| {
                self.home
                    .as_ref()
                    .and_then(|home| home.sessions.row(id))
                    .map(crate::home::title)
            })
            .unwrap_or(fallback);
        let more = waiting.len().saturating_sub(1);
        let notice = if more == 0 {
            format!(
                "Not deleted: the hub link dropped before {display} stopped. Nothing was deleted."
            )
        } else {
            format!(
                "Not deleted: the hub link dropped before {display} and {more} more stopped. Nothing was deleted."
            )
        };
        self.delete_command_finish(true);
        self.notices.push("io_failed", notice);
    }

    /// Settles only this command's exact unsent delete ask, or an unsent
    /// close. An ambiguous delete write is never retried.
    pub(in crate::app::home::prompt) fn delete_command_unsent(&mut self, unsent: &[String]) {
        let deletion =
            self.home
                .as_ref()
                .and_then(|home| match &home.questions.delete_command.state {
                    State::Deleting {
                        command, sessions, ..
                    } if unsent.contains(command) => Some((command.clone(), sessions.clone())),
                    State::Idle
                    | State::Discovering { .. }
                    | State::Asked { .. }
                    | State::Stopping { .. }
                    | State::Deleting { .. } => None,
                });
        if let Some((command, _sessions)) = deletion {
            self.take_ask(&command);
            self.delete_command_delete_unknown();
            return;
        }
        let mine = self.delete_waiting_close_ids().unwrap_or_default();
        if mine.iter().any(|close| unsent.contains(close)) {
            self.delete_command_link_lost();
        }
    }

    /// The stopping command's close command ids, while stops are out.
    fn delete_waiting_close_ids(&self) -> Option<Vec<String>> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Stopping { closes, .. } => Some(closes.keys().cloned().collect()),
            State::Idle
            | State::Discovering { .. }
            | State::Asked { .. }
            | State::Deleting { .. } => None,
        }
    }

    /// The delete's answer never arrived after its stops completed: the
    /// stop evidence stands but nothing confirms the removal.
    pub(super) fn delete_command_delete_unknown(&mut self) {
        self.delete_command_finish(false);
        self.notices
            .push("indeterminate", "Delete status unknown".to_owned());
    }
}

/// The refused delete's first busy id and how many more follow. The
/// structured `held` field is trusted only as a complete, valid unique
/// list; otherwise fall back to the target and single-session wording.
fn held_refusal_parts(
    payload: &Map<String, Value>,
    target: &SessionId,
    sessions: &[SessionId],
) -> (SessionId, usize) {
    let Some(held) = payload.get("held").and_then(Value::as_array) else {
        return (target.clone(), 0);
    };
    let allowed: BTreeSet<&str> = sessions.iter().map(|session| session.0.as_str()).collect();
    if held.is_empty() {
        return (target.clone(), 0);
    }
    let mut seen = BTreeSet::new();
    let mut ids = Vec::with_capacity(held.len());
    for value in held {
        let Some(id) = value
            .as_str()
            .filter(|id| contract::is_minted_session_id(id))
        else {
            return (target.clone(), 0);
        };
        if !seen.insert(id) || !allowed.contains(id) {
            return (target.clone(), 0);
        }
        ids.push(id);
    }
    match ids.first() {
        Some(first) => (SessionId((*first).to_owned()), ids.len() - 1),
        None => (target.clone(), 0),
    }
}
