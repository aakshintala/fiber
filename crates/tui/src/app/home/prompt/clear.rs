//! `/clear` while the old session waits: the clear question, asking
//! whether to clear anyway (`docs/tui.md`, "Clearing"). This module is
//! a child of home's question module, whose closes it owns.

use super::super::super::{App, Effect};
use super::Prompt;
use crate::home::ClearChoice;
use crate::keys::Key;
use contract::SessionId;

impl App {
    /// Opens the clear question for `id`, named `name` and waiting on
    /// `waiting`: Enter clears anyway, Esc stays. It sends nothing, and
    /// the consumed command leaves the draft, so typing `/clear` again
    /// dispatches fresh instead of appending to a stale row.
    pub(crate) fn open_clear(&mut self, id: SessionId, name: String, waiting: String) -> Effect {
        self.close_delete_prompt();
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = Some(Prompt::Clear {
                id,
                name,
                waiting,
                now: false,
            });
        }
        self.draft.clear();
        Effect::None
    }

    /// A key for the clear question, once open: Enter sends the staged
    /// `start`, Esc stays. Anything else holds, keeping the draft.
    pub(in crate::app) fn clear_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.clear_open() {
            return None;
        }
        Some(match key {
            Key::Enter => self.confirm_clear(),
            Key::Esc => {
                self.close_clear();
                Effect::None
            }
            Key::Char(_)
            | Key::Backspace
            | Key::Up
            | Key::Down
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlO
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::CtrlF
            | Key::CtrlC
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => Effect::None,
        })
    }

    /// The clear question's choice: clearing sends the staged `start`,
    /// staying closes the question. A `start` already in flight sends
    /// nothing more.
    pub(in crate::app::home) fn clear_click(&mut self, choice: ClearChoice) -> Effect {
        if !self.clear_open() {
            return Effect::None;
        }
        match choice {
            ClearChoice::Clear => self.confirm_clear(),
            ClearChoice::Stay => {
                self.close_clear();
                Effect::None
            }
        }
    }

    /// Enter in the clear question: the staged `start` goes out, and its
    /// `close` will go with `now`. With no status to name, or a `start`
    /// already in flight, the question only closes.
    fn confirm_clear(&mut self) -> Effect {
        if self.clear_pending_command_id().is_some() {
            return Effect::None;
        }
        let staged = self.home.as_mut().and_then(|home| {
            let Some(Prompt::Clear { id, now, .. }) = home.questions.prompt.as_mut() else {
                return None;
            };
            *now = true;
            let status = home.sessions.row(id)?.status.clone()?;
            Some((status, id.clone()))
        });
        self.close_clear();
        let Some((status, session)) = staged else {
            return Effect::None;
        };
        self.clear_send(status, session, true)
    }

    /// Closes the clear question, keeping the session: a
    /// `Prompt::Clear` only, anything else stays as it is.
    fn close_clear(&mut self) {
        if let Some(home) = self.home.as_mut()
            && matches!(home.questions.prompt, Some(Prompt::Clear { .. }))
        {
            home.questions.prompt = None;
        }
    }
}
