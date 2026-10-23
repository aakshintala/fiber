//! `/reviewer-setup` on the app (`docs/permissions.md`, "Setting up the
//! notes"; `docs/tui.md`, "Slash commands"): the command, the setup bound
//! to the session it opened on, and the answers and interruptions that
//! reach it.
//!
//! The terminal uses nothing another client lacks: the driver commands
//! `propose_notes` and `reload`, sent with the bound session's id, and the
//! hub's `reviewer_notes` and `save_reviewer_notes` operations through the
//! configuration seam.

use std::collections::BTreeSet;
use std::path::PathBuf;

use contract::events::{CommandAccepted, CommandRejected, CommandResult};
use contract::{Envelope, SessionId};
use serde_json::Value;

use super::{App, Effect, Kind, Phase, mint, read, session_command};
use crate::configure::{ConfigureError, NotesSaved, ReviewerNotes};
use crate::input::Draft;
use crate::keys::{Edit, Key};
use crate::reviewer_setup::{Act, Screen, Setup};

/// What the command says when the terminal was given no seam: the same
/// words the configuration views use.
const UNAVAILABLE: &str = "Not available in this terminal.";

/// What the question says while a turn runs.
const BUSY_TURN: &str = "A turn is running. Propose again when it ends.";

/// The command's read failed before any screen existed.
const NOT_LOADED: &str = "Could not read the reviewer notes: the session has not loaded yet.";
const NO_PROJECT: &str = "Could not read the reviewer notes: this session has no project.";

/// What the notice says when the setup closes because the screen moved to
/// another session.
const LEFT: &str = "The reviewer setup closed: another session is on screen. Nothing was saved.";

/// What Enter on a diff with no changed layer says.
const UNCHANGED: &str = "Nothing changed. No file was written.";

/// The setup open now, if any, and the ids of requests whose answers no
/// longer matter.
#[derive(Default)]
pub(super) struct State {
    open: Option<Open>,
    /// Request ids to settle silently when their answers arrive: dropped
    /// by Esc, a close, or a signal that the answer may never come.
    dropped: BTreeSet<String>,
}

/// A setup with the session and workspace it is bound to.
struct Open {
    setup: Setup,
    /// The session it opened on: every request goes to it.
    session: SessionId,
    /// The workspace of that session's own row: the notes it reads and
    /// writes belong to its project.
    workspace: PathBuf,
    /// The `propose_notes` waiting for its answer.
    pending: Option<String>,
}

impl App {
    /// `/reviewer-setup`: reads both layers' notes for the attached
    /// session's project and opens on topic 1's question. With no session
    /// a notice says so; with the link down nothing opens and the draft
    /// stays; with no seam, no row for the session or a failed read, one
    /// notice and nothing opens.
    pub(super) fn open_reviewer_setup(&mut self) -> Effect {
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        self.draft.clear();
        let Some(seam) = self.configure_seam() else {
            self.push_notice("io_failed", UNAVAILABLE.to_owned());
            return Effect::None;
        };
        let workspace = match self.reviewer_binding(&session) {
            Ok(workspace) => workspace,
            Err(why) => {
                self.push_notice("io_failed", why.to_owned());
                return Effect::None;
            }
        };
        match seam.reviewer_notes(&workspace) {
            Ok(notes) => self.start_reviewer_setup(Setup::open(notes), session, workspace),
            Err(error) => self.push_notice(
                "io_failed",
                format!("Could not read the reviewer notes: {}", error.message),
            ),
        }
        Effect::None
    }

    fn start_reviewer_setup(&mut self, setup: Setup, session: SessionId, workspace: PathBuf) {
        self.reviewer_setup.open = Some(Open {
            setup,
            session,
            workspace,
            pending: None,
        });
    }

    /// The workspace of `session`'s own row on home's list, and nothing
    /// else: the picked, chosen and launch workspaces can name another
    /// project's directory, or none on a remote home.
    fn reviewer_binding(&self, session: &SessionId) -> Result<PathBuf, &'static str> {
        let row = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.row(session))
            .ok_or(NOT_LOADED)?;
        if row.workspace.is_empty() || row.project.is_empty() {
            return Err(NO_PROJECT);
        }
        Ok(PathBuf::from(&row.workspace))
    }

    /// The open setup, if any.
    pub(crate) fn reviewer_setup(&self) -> Option<&Setup> {
        self.reviewer_setup.open.as_ref().map(|open| &open.setup)
    }

    /// Whether the setup holds the keyboard: it is open, the attached
    /// session is the one it is bound to, and no approval panel is up.
    pub(crate) fn reviewer_setup_open(&self) -> bool {
        self.reviewer_setup
            .open
            .as_ref()
            .is_some_and(|open| self.session() == Some(&open.session) && !self.queue.open())
    }

    /// A key at the setup. Ctrl+C is never taken: it falls through to quit.
    pub(super) fn reviewer_setup_key(&mut self, key: &Key) -> Option<Effect> {
        if *key == Key::CtrlC || !self.reviewer_setup_open() {
            return None;
        }
        let busy = matches!(self.phase, Phase::Attached { busy: true, .. });
        let open = self.reviewer_setup.open.as_mut()?;
        if *key == Key::Enter && open.setup.screen() == Screen::Question && busy {
            open.setup.refused(BUSY_TURN);
            return Some(Effect::None);
        }
        let act = open.setup.key(key);
        Some(self.reviewer_act(act))
    }

    /// An edit at the setup: a paste goes into the field being edited, every
    /// other edit is swallowed.
    pub(super) fn reviewer_setup_edit(&mut self, edit: &Edit) -> Effect {
        if let (Edit::Paste(text), Some(open)) = (edit, self.reviewer_setup.open.as_mut()) {
            open.setup.paste(text);
        }
        Effect::None
    }

    /// Carries out what a key or an answer asked of the app.
    fn reviewer_act(&mut self, act: Act) -> Effect {
        match act {
            Act::None => Effect::None,
            Act::Propose(args) => {
                let Some(open) = self.reviewer_setup.open.as_mut() else {
                    return Effect::None;
                };
                let id = mint();
                let args = serde_json::to_value(&args).ok();
                let line = session_command(&id, "propose_notes", &open.session, args).to_string();
                open.pending = Some(id.clone());
                match self.outbox.send(id, Kind::Command, Draft::default(), line) {
                    Some(line) => Effect::Send(vec![line]),
                    None => Effect::None,
                }
            }
            Act::Abandon => {
                self.drop_pending_request();
                Effect::None
            }
            Act::Save { global, project } => {
                let result = match (self.configure_seam(), self.bound_workspace()) {
                    (Some(seam), Some(workspace)) => {
                        seam.save_reviewer_notes(&workspace, global, project)
                    }
                    _ => Err(unavailable()),
                };
                self.finish_save(result)
            }
            Act::Reread => {
                let notes = self.read_notes();
                if let Some(open) = self.reviewer_setup.open.as_mut() {
                    open.setup.reread(notes);
                }
                Effect::None
            }
            Act::Reload => {
                self.reviewer_setup_close();
                super::commands::run_reload(self, "").unwrap_or(Effect::None)
            }
            Act::Close => {
                self.reviewer_setup_close();
                Effect::None
            }
            Act::Unchanged => {
                self.reviewer_setup_close();
                self.push_notice("notes_unchanged", UNCHANGED.to_owned());
                Effect::None
            }
        }
    }

    fn finish_save(&mut self, result: Result<NotesSaved, ConfigureError>) -> Effect {
        let act = match self.reviewer_setup.open.as_mut() {
            Some(open) => open.setup.saved(result),
            None => Act::None,
        };
        self.reviewer_act(act)
    }

    fn bound_workspace(&self) -> Option<PathBuf> {
        self.reviewer_setup
            .open
            .as_ref()
            .map(|open| open.workspace.clone())
    }

    fn read_notes(&self) -> Result<ReviewerNotes, ConfigureError> {
        match (self.configure_seam(), self.bound_workspace()) {
            (Some(seam), Some(workspace)) => seam.reviewer_notes(&workspace),
            _ => Err(unavailable()),
        }
    }

    /// Forgets the waiting request: its answer, when it comes, settles
    /// silently.
    fn drop_pending_request(&mut self) {
        if let Some(id) = self
            .reviewer_setup
            .open
            .as_mut()
            .and_then(|open| open.pending.take())
        {
            self.reviewer_setup.dropped.insert(id);
        }
    }

    /// Closes the setup, saving nothing; a request still waiting is
    /// dropped.
    pub(crate) fn reviewer_setup_close(&mut self) {
        self.drop_pending_request();
        self.reviewer_setup.open = None;
    }

    /// The hook on `on_session`: an answer to the waiting `propose_notes`
    /// from the bound session reaches the setup, and one to a dropped
    /// request settles with no notice. True when the line was one of those.
    pub(super) fn reviewer_setup_settled(&mut self, envelope: &Envelope) -> bool {
        if !matches!(
            envelope.kind.as_str(),
            "command_accepted" | "command_rejected"
        ) {
            return false;
        }
        let Some(id) = envelope.payload.get("command_id").and_then(Value::as_str) else {
            return false;
        };
        if self.reviewer_setup.dropped.remove(id) {
            self.outbox.answered(id);
            return true;
        }
        let Some(open) = self.reviewer_setup.open.as_mut() else {
            return false;
        };
        if open.pending.as_deref() != Some(id) || open.session != envelope.session_id {
            return false;
        }
        if envelope.kind == "command_accepted" {
            let Some(accepted) = read!(envelope, CommandAccepted) else {
                return false;
            };
            let Some(CommandResult::ProposeNotes {
                evidence,
                proposed,
                error,
            }) = accepted.result
            else {
                return false;
            };
            open.setup.proposed(&evidence, &proposed, error.as_deref());
        } else {
            let Some(rejected) = read!(envelope, CommandRejected) else {
                return false;
            };
            open.setup.refused(&rejected.message);
        }
        open.pending = None;
        self.outbox.answered(id);
        true
    }

    /// The hook on `attach`: the bound session again is a reconnect, which
    /// may have lost the waiting request's answer; another session closes
    /// the setup.
    pub(super) fn reviewer_setup_attached(&mut self, session: &SessionId) {
        let Some(open) = self.reviewer_setup.open.as_ref() else {
            return;
        };
        if open.session == *session {
            self.reviewer_setup_stream_lost(session);
        } else {
            self.reviewer_setup_leave();
        }
    }

    /// The hook on going home: the setup closes as it does for another
    /// session.
    pub(super) fn reviewer_setup_left(&mut self) {
        self.reviewer_setup_leave();
    }

    /// Closes the setup because the screen left its session, with a notice
    /// that nothing was saved.
    fn reviewer_setup_leave(&mut self) {
        if self.reviewer_setup.open.is_none() {
            return;
        }
        self.reviewer_setup_close();
        self.push_notice("session_not_found", LEFT.to_owned());
    }

    /// The hook on a stream ending or reopening for `session`: a waiting
    /// request's answer may never come, so the question reopens with its
    /// answer kept. With nothing waiting nothing changes.
    pub(super) fn reviewer_setup_stream_lost(&mut self, session: &SessionId) {
        let Some(open) = self.reviewer_setup.open.as_mut() else {
            return;
        };
        if open.session != *session || open.pending.is_none() {
            return;
        }
        open.setup.lost();
        self.drop_pending_request();
    }
}

fn unavailable() -> ConfigureError {
    ConfigureError {
        code: contract::ErrorCode::IoFailed,
        message: UNAVAILABLE.to_owned(),
    }
}

#[cfg(test)]
#[path = "reviewer_setup_tests.rs"]
mod tests;
