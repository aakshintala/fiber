//! The `/cd` move on the wire (`docs/tui.md`, "Moving workspace"):
//! acceptance switches only the session the request went out on.
//! The outbox's owner tag retains that session until settlement.

use contract::events::{CommandAccepted, CommandResult};
use contract::{ErrorCode, SessionId};

use super::App;
use super::outbox::Waiting;
use crate::home::Level;

impl App {
    /// Attaches a move's child only while its original session is on screen.
    pub(in crate::app) fn change_workspace_accepted(
        &mut self,
        old: &SessionId,
        session: &SessionId,
        accepted: &CommandAccepted,
    ) -> Option<Vec<String>> {
        self.outbox.settled(&accepted.command_id.0);
        let Some(CommandResult::Rewind { new_session_id }) = accepted.result.as_ref() else {
            return None;
        };
        if session != old || self.session() != Some(old) {
            return None;
        }
        let new = new_session_id.clone();
        let lines = vec![self.subscribe(&new, Level::Full), self.ask_commands(&new)];
        self.attach(new);
        Some(lines)
    }

    /// A duplicate resend keeps the first command's claim until it answers.
    pub(in crate::app) fn change_workspace_refused(&mut self, id: &str, code: &ErrorCode) {
        if *code != ErrorCode::DuplicateCommand {
            self.outbox.settled(id);
        }
    }

    /// A hub refusal for the waiting move: a `duplicate_command` keeps
    /// the first command's claim until it answers, any other code
    /// settles the wait. Generic settlement and its notice run on the
    /// caller side.
    pub(in crate::app) fn change_workspace_hub_refused(&mut self, id: &str, code: &str) {
        if code != "duplicate_command" {
            self.outbox.settled(id);
        }
    }

    /// An unwritten move has no answer still to come.
    pub(in crate::app) fn change_workspace_dropped(&mut self, id: &str) {
        if let Some(Waiting::ChangeWorkspace(_)) = self.outbox.waiting(id) {
            self.outbox.settled(id);
        }
    }
}

#[cfg(test)]
#[path = "change_workspace_tests.rs"]
mod tests;
