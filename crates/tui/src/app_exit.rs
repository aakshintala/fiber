//! Quitting with sessions working: the quit question, closing all,
//! and the resume lines (`docs/tui.md`, "Quit", "On exit"). This module
//! is `App`'s exit: a child of its home, whose rows and levels it reads.

use serde_json::{Value, json};

use super::super::{App, Effect, Link, Phase, mint, session_command};
use super::Prompt;
use crate::home::{Level, quit_line};
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
            home.prompt = Some(Prompt::Quit);
        }
        self.armed_at = None;
        Effect::None
    }

    /// The quit hint, or the quit question naming the sessions working
    /// now: it recomputes from the current rows on every frame, so a
    /// session ending or starting updates the question as it shows.
    /// Home's foot and the conversation screen's hint line draw it.
    pub(crate) fn hint_text(&self) -> String {
        if !self.quit_open() {
            return super::super::QUIT_HINT.to_owned();
        }
        let working = self.working();
        quit_line(working.len(), self.elsewhere(&working))
    }

    /// `c` in the quit question, for the sessions working at the key
    /// press: `close` with `now` each, with a `summary` subscribe first
    /// where this connection holds nothing. With nothing working, or the
    /// link down, it quits and closes nothing: with no hub stream nothing
    /// is written and no `write_failed` runs.
    pub(super) fn close_all(&mut self) -> Effect {
        let working = self.working();
        if working.is_empty() || self.link != Link::Up {
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
                home.closing.push((session.clone(), id));
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
            home.closing.retain(|(_, close)| !ids.contains(close));
        }
    }

    /// The sessions working now, in list order, the attached one last
    /// when it has no feed row: live rows running a turn, a job or a
    /// delegate, or waiting on the person, and the attached session while
    /// its turn runs. Unreadable rows never count, and neither do rows
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
        // The attached session counts once: its feed row already names
        // it, and without one it is still working.
        if let Phase::Attached {
            session,
            busy: true,
        } = &self.phase
            && !working.contains(session)
            && !home.sessions.live().iter().any(|row| row.id == *session)
        {
            working.push(session.clone());
        }
        working
    }

    /// One resume line per live session whose stop was not sent: every
    /// live feed row outside `closing`, in list order, and the attached
    /// session when it has no row and is not closing. Empty without
    /// home. `run` writes them after restoring the terminal.
    pub(crate) fn exit_lines(&self) -> Vec<String> {
        let Some(home) = self.home.as_ref() else {
            return Vec::new();
        };
        let mut lines: Vec<String> = home
            .sessions
            .live()
            .iter()
            .filter(|row| !home.closing.iter().any(|(session, _)| *session == row.id))
            .map(|row| crate::home::exit_line(&row.id))
            .collect();
        // The attached session without a feed row is still live: without
        // one it never left, and a row would have listed it.
        if let Phase::Attached { session, .. } = &self.phase
            && home.sessions.row(session).is_none()
            && !home.closing.iter().any(|(closed, _)| closed == session)
        {
            lines.push(crate::home::exit_line(session));
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
#[path = "app_exit_tests.rs"]
mod tests;
