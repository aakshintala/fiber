//! Quitting with sessions working: the quit question, closing all,
//! and the resume lines (`docs/tui.md`, "Quit", "On exit"). This module
//! is a child of home's question module, whose closes it owns.

use serde_json::{Value, json};

use super::super::super::{App, Effect, Phase, mint, session_command};
use super::Prompt;
use crate::home::{Level, QuitChoice};
use crate::keys::Key;
use contract::SessionId;

impl App {
    /// Quits, asking first while sessions work: with no session working
    /// the terminal exits without asking. Opening the question clears
    /// the armed Ctrl+C.
    pub(crate) fn quit(&mut self) -> Effect {
        if self.home.is_none() || self.working().is_empty() {
            return Effect::Quit;
        }
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = Some(Prompt::Quit);
        }
        self.armed_at = None;
        Effect::None
    }

    /// The quit question, while open: how many sessions work and of them
    /// how many are also open elsewhere. It recomputes from the current
    /// rows on every frame, so a session ending or starting updates the
    /// question as it shows.
    pub(crate) fn quit_question(&self) -> Option<(usize, usize)> {
        if !self.quit_open() {
            return None;
        }
        let working = self.working();
        Some((working.len(), self.elsewhere(&working)))
    }

    /// The quit hint: shown while a first Ctrl+C waits for its second.
    /// The quit question draws as an overlay, not through the hint.
    /// While the rail holds focus and no quit waits, the rail hint
    /// shows instead: an armed quit still wins when both hold.
    pub(crate) fn hint_text(&self) -> String {
        if self.rail_state.focused().is_some() && self.armed_at.is_none() {
            crate::app::rail::RAIL_HINT.to_owned()
        } else {
            super::super::super::QUIT_HINT.to_owned()
        }
    }

    /// The quit question's keys, on home or not: Enter leaves working
    /// sessions running, `c` closes them all now, Esc stays, and
    /// anything else, Ctrl+C included, does nothing. `None` unless the
    /// quit question is open.
    pub(in crate::app) fn quit_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.quit_open() {
            return None;
        }
        match key {
            Key::Enter => Some(Effect::Quit),
            Key::Char('c') => Some(self.close_all()),
            Key::Esc => {
                self.close_prompt();
                Some(Effect::None)
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
            | Key::AltDigit(_) => Some(Effect::None),
        }
    }

    /// The quit question's choice, on home or over a session: leaving
    /// quits with the sessions running, closing all stops them first,
    /// staying closes the question.
    pub(in crate::app::home) fn quit_click(&mut self, choice: QuitChoice) -> Effect {
        if !self.quit_open() {
            return Effect::None;
        }
        match choice {
            QuitChoice::Leave => Effect::Quit,
            QuitChoice::CloseAll => self.close_all(),
            QuitChoice::Stay => {
                self.close_prompt();
                Effect::None
            }
        }
    }

    /// `c` in the quit question, for the sessions working at the key
    /// press: `close` with `now` each, with a `summary` subscribe first
    /// where this connection holds nothing. With nothing working, or the
    /// link down, it quits and closes nothing: with no hub stream nothing
    /// is written and no `write_failed` runs.
    fn close_all(&mut self) -> Effect {
        let working = self.working();
        if working.is_empty() || !self.outbox.is_up() {
            return Effect::Quit;
        }
        let mut lines = Vec::new();
        for session in &working {
            if self
                .home
                .as_ref()
                .is_some_and(|home| home.subs.expected(session).is_none())
            {
                lines.push(self.subscribe(session, Level::Summary));
            }
            let id = mint();
            lines.push(
                session_command(&id, "close", session, Some(json!({"now": true}))).to_string(),
            );
            if let Some(home) = self.home.as_mut() {
                home.questions.closing.push((session.clone(), id));
            }
        }
        Effect::Exit(lines)
    }

    /// Drops from `closing` every session whose `close` line was not
    /// written: a close never sent keeps its resume line on exit.
    pub(crate) fn home_unsent(&mut self, unsent: &[String]) {
        let ids: Vec<String> = unsent
            .iter()
            .filter_map(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
            })
            .collect();
        if let Some(home) = self.home.as_mut() {
            home.questions
                .closing
                .retain(|(_, close)| !ids.contains(close));
        }
    }

    /// The sessions working now, in list order, the attached one last
    /// when its row does not already list it: live rows running a turn,
    /// a job or a delegate, or waiting on the person, and the attached
    /// session while its turn runs, whatever its feed row says.
    /// Unreadable rows never count, and neither do rows
    /// that left.
    fn working(&self) -> Vec<SessionId> {
        let Some(home) = self.home.as_ref() else {
            return Vec::new();
        };
        let mut working: Vec<SessionId> = home
            .sessions
            .live()
            .iter()
            .filter(|row| {
                matches!(
                    row.state,
                    crate::home::State::Working
                        | crate::home::State::Retrying
                        | crate::home::State::Waiting
                        | crate::home::State::Jobs
                ) || (row.state == crate::home::State::Idle && (row.jobs > 0 || row.delegates > 0))
            })
            .map(|row| row.id.clone())
            .collect();
        // The attached session counts once: a row already listing it
        // keeps its position, and otherwise it appends last even when
        // its feed row reads idle.
        if let Phase::Attached {
            session,
            busy: true,
        } = &self.phase
            && !working.contains(session)
        {
            working.push(session.clone());
        }
        working
    }

    /// One resume line per live session whose stop was not sent: every
    /// live feed row outside `closing`, in list order, and the attached
    /// session when it is not closing and no live row already names it.
    /// Empty without home. `run` writes them after restoring the
    /// terminal.
    pub(crate) fn exit_lines(&self) -> Vec<String> {
        let Some(home) = self.home.as_ref() else {
            return Vec::new();
        };
        let mut lines: Vec<String> = home
            .sessions
            .live()
            .iter()
            .filter(|row| {
                !home
                    .questions
                    .closing
                    .iter()
                    .any(|(session, _)| *session == row.id)
            })
            .map(|row| crate::home::exit_line(&row.id, home.launch.location.hub()))
            .collect();
        // The attached session is live while attached: it never left,
        // and a resumed session's row still reads exited until the feed
        // update marks it live. Without this it gets no resume line when
        // quitting before that update lands. An exit that arrived after
        // the attach instead marks it dead, and it gets no line.
        if let Phase::Attached { session, .. } = &self.phase
            && !home.sessions.live().iter().any(|row| row.id == *session)
            && !home
                .questions
                .closing
                .iter()
                .any(|(closed, _)| closed == session)
            && home.left_after_attach.as_ref() != Some(session)
        {
            lines.push(crate::home::exit_line(session, home.launch.location.hub()));
        }
        lines
    }

    /// Of `working`, how many are also open elsewhere: a session's
    /// `clients` on its status, less this connection's own `full`
    /// connection to it when it holds one. A session with no row, the
    /// attached one not yet in the feed, counts as not elsewhere.
    fn elsewhere(&self, working: &[SessionId]) -> usize {
        let Some(home) = self.home.as_ref() else {
            return 0;
        };
        working
            .iter()
            .filter(|id| {
                home.sessions.row(id).is_some_and(|row| {
                    row.clients
                        .saturating_sub(u32::from(home.subs.full(&row.id)))
                        > 0
                })
            })
            .count()
    }
}

#[cfg(test)]
#[path = "exit_tests.rs"]
mod tests;
