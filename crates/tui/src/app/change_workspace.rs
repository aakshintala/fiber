//! The `/cd` move on the wire (`docs/tui.md`, "Moving workspace"): each
//! `change_workspace` command's pending correlation, its acceptance's
//! switch, and its refusal and loss settlement.
//!
//! Owns its state; the `App` seam sending the command and handling its
//! answers reaches it only through the methods below. Each pending entry
//! retains the session on screen when its command went out, so a second
//! `/cd` never drops the first move's claim: an accept for another
//! command, a late or duplicate answer, or one for a session no longer
//! on screen never switches the display. Reuse `Kind::Command`
//! rather than changing the generic outbox types.

use contract::events::{CommandAccepted, CommandResult};
use contract::{ErrorCode, SessionId};

use super::App;
use crate::home::Level;

/// A `change_workspace` request waiting for its answer.
#[derive(Debug)]
struct Pending {
    /// The command id the answer names.
    id: String,
    /// The session on screen when the command went out.
    old: SessionId,
}

/// The `/cd` send state: the commands on the wire, if any.
#[derive(Debug, Default)]
pub(crate) struct ChangeWorkspace {
    pending: Vec<Pending>,
}

impl ChangeWorkspace {
    /// Notes the `change_workspace` request on the wire for `old`,
    /// keeping every earlier claim: each `/cd` answers on its own id.
    pub(super) fn begin(&mut self, id: String, old: SessionId) {
        self.pending.push(Pending { id, old });
    }

    /// Takes the pending request `id` answers, leaving every other
    /// command's claim in place.
    fn take(&mut self, id: &str) -> Option<Pending> {
        let at = self.pending.iter().position(|pending| pending.id == id)?;
        Some(self.pending.remove(at))
    }

    /// Drops the pending request for `id` unless the refusal is a
    /// `duplicate_command`: a resent copy the session already holds
    /// leaves the first answer still to come, so its claim stands.
    fn refused(&mut self, id: &str, code: &ErrorCode) {
        if *code == ErrorCode::DuplicateCommand {
            return;
        }
        self.pending.retain(|pending| pending.id != id);
    }

    /// Drops the pending request for a line that never went out: with
    /// no kept line the hub never answers it, so no later answer can
    /// settle it.
    fn dropped(&mut self, id: &str) {
        self.pending.retain(|pending| pending.id != id);
    }
}

impl App {
    /// A `command_accepted` answering a pending `change_workspace`
    /// attaches the new session with `full` and asks its commands, as a
    /// `rewind` accept does, without a first prompt. The display switches
    /// only when the answer names a pending id, reads as the move's
    /// accepted result, and the screen still shows the session the
    /// command went out on: a late, duplicate, or stale answer, or one
    /// for another command, settles nothing and sends nothing.
    pub(in crate::app) fn change_workspace_accepted(
        &mut self,
        session: &SessionId,
        accepted: &CommandAccepted,
    ) -> Option<Vec<String>> {
        let pending = self.change_workspace.take(&accepted.command_id.0)?;
        let Some(CommandResult::Rewind { new_session_id }) = accepted.result.as_ref() else {
            return None;
        };
        if *session != pending.old || self.session() != Some(&pending.old) {
            return None;
        }
        let new = new_session_id.clone();
        // A new attachment asks the new session's commands, as a clear
        // `start` accept does for its session.
        let lines = vec![self.subscribe(&new, Level::Full), self.ask_commands(&new)];
        self.attach(new);
        Some(lines)
    }

    /// A `command_rejected` for a pending `change_workspace` drops its
    /// claim, so a late accept for it never attaches: the generic path
    /// still shows the refusal's notice, and the screen stays where it
    /// is. A `duplicate_command` keeps the claim, as above.
    pub(in crate::app) fn change_workspace_refused(&mut self, id: &str, code: &ErrorCode) {
        self.change_workspace.refused(id, code);
    }

    /// A hub-relayed `command_rejected` for a sent `change_workspace`
    /// drops its claim as the session's own refusal does. Always false,
    /// so the answer keeps its generic settlement and notice; a
    /// `duplicate_command` keeps the claim, as above. Only a duplicate
    /// refusal reads the code: any other code drops the claim the same
    /// way a lost line does, with no parse.
    pub(in crate::app) fn change_workspace_hub_refused(&mut self, id: &str, code: &str) -> bool {
        if code != "duplicate_command" {
            self.change_workspace.dropped(id);
        }
        false
    }

    /// A command line lost before it was written: a lost
    /// `change_workspace` keeps nothing, since the hub never answers a
    /// line it never got.
    pub(in crate::app) fn change_workspace_dropped(&mut self, id: &str) {
        self.change_workspace.dropped(id);
    }
}

#[cfg(test)]
#[path = "change_workspace_tests.rs"]
mod tests;
