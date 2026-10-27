//! Quitting with sessions working: the quit question, closing all,
//! and the resume lines (`docs/tui.md`, "Quit", "On exit"). This module
//! is a child of home's question module, whose closes it owns.

use serde_json::{Value, json};

use super::super::super::{App, Effect, Phase, mint, session_command};
use super::{Ask, Prompt};
use crate::home::{Level, QuitChoice, Spot, State};
use crate::keys::Key;
use crate::mouse::TargetId;
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
    /// question as it shows. It answers only for the quit prompt: the
    /// close-all question reads through its own counter.
    pub(crate) fn quit_question(&self) -> Option<(usize, usize)> {
        if !matches!(
            self.home.as_ref().and_then(|home| home.questions.prompt()),
            Some(Prompt::Quit)
        ) {
            return None;
        }
        let working = self.working();
        Some((working.len(), self.elsewhere(&working)))
    }

    /// The close-all question, while open: the sessions, working and
    /// elsewhere counts fixed when it opened. The overlay never
    /// live-updates; Enter closes the counted sessions still live.
    pub(crate) fn close_all_question(&self) -> Option<(usize, usize, usize)> {
        match self.home.as_ref().and_then(|home| home.questions.prompt()) {
            Some(Prompt::CloseAll { sessions, counts }) => {
                Some((sessions.len(), counts.0, counts.1))
            }
            _ => None,
        }
    }

    /// `/close all`, on home or over a session: asks to close every live
    /// top-level session now, after one question. With no live session it
    /// says so and opens nothing; with the link down it opens nothing and
    /// keeps the draft. It never sends a line: Enter does that.
    pub(crate) fn ask_close_all(&mut self) -> Effect {
        let sessions = self.live_sessions();
        if sessions.is_empty() {
            self.notices
                .push("session_not_found", "No sessions to close.".to_owned());
            return Effect::None;
        }
        if !self.outbox.is_up() {
            return Effect::None;
        }
        let working = self.working();
        let counts = (
            sessions.iter().filter(|id| working.contains(id)).count(),
            self.elsewhere(&sessions),
        );
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = Some(Prompt::CloseAll { sessions, counts });
        }
        Effect::None
    }

    /// The stop question, while open: the session's name and whether it
    /// was working and open elsewhere when the question opened. The
    /// overlay never live-updates.
    pub(crate) fn stop_question(&self) -> Option<(String, bool, bool)> {
        match self.home.as_ref().and_then(|home| home.questions.prompt()) {
            Some(Prompt::Stop {
                name,
                working,
                elsewhere,
                ..
            }) => Some((name.clone(), *working, *elsewhere)),
            _ => None,
        }
    }

    /// Delete on a focused home row: the stop question for a live row, or
    /// whatever [`App::delete_key`] does. A live row needs the link up and a
    /// readable status, otherwise Delete does nothing; the exited rows keep
    /// the delete question. `None` with the box focused, so the draft
    /// keeps the key.
    pub(in crate::app::home) fn delete_or_stop_key(&mut self) -> Option<Effect> {
        let key = match self.focus {
            Some(TargetId::Home(Spot::Entry(key) | Spot::Stop(key))) => key,
            _ => return self.delete_key(),
        };
        let live = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .is_some_and(|row| row.left.is_none());
        if !live {
            return self.delete_key();
        }
        self.ask_stop(key);
        Some(Effect::None)
    }

    /// Asks to stop the live session on the row with `key`: it needs the
    /// link up and a readable status, and sends nothing, Enter does that.
    /// It replaces any question open on the foot. A crashed or exited row
    /// opens nothing here.
    pub(in crate::app) fn ask_stop(&mut self, key: u64) -> Effect {
        if !self.outbox.is_up() {
            return Effect::None;
        }
        let Some(row) = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .filter(|row| row.left.is_none() && row.state != State::Unreadable)
        else {
            return Effect::None;
        };
        let id = row.id.clone();
        let name = crate::home::title(row);
        let working = self.working().contains(&id);
        let elsewhere = self.elsewhere(std::slice::from_ref(&id)) > 0;
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = Some(Prompt::Stop {
                id,
                name,
                working,
                elsewhere,
            });
        }
        Effect::None
    }

    /// Enter in the stop question: `close` with `now` for its session,
    /// with a `summary` subscribe first where this connection holds
    /// nothing, then the question closes and the rail's focus drops. With
    /// the link down nothing goes out and the question stays. A session that
    /// left while it showed gets no close and closes the question. When the
    /// stopped session is the one on screen the terminal goes home.
    fn confirm_stop(&mut self) -> Effect {
        let id = match self.home.as_ref().and_then(|home| home.questions.prompt()) {
            Some(Prompt::Stop { id, .. }) => id.clone(),
            _ => return Effect::None,
        };
        if !self.outbox.is_up() {
            return Effect::None;
        }
        if !self.live_sessions().contains(&id) {
            self.close_prompt();
            return Effect::None;
        }
        let (lines, pairs) = self.close_lines(std::slice::from_ref(&id));
        if let Some(home) = self.home.as_mut() {
            for (session, close) in pairs {
                home.questions.asks.insert(close, Ask::Stop(session));
            }
        }
        self.close_prompt();
        self.clear_rail_focus();
        if self.session() == Some(&id) {
            self.go_home();
        }
        Effect::Send(lines)
    }

    /// The live top-level sessions on this hub closing asks about, in
    /// list order with the attached one last: every live row, plus
    /// the attached session when no live row names it, unless it left
    /// after the attach.
    fn live_sessions(&self) -> Vec<SessionId> {
        let Some(home) = self.home.as_ref() else {
            return Vec::new();
        };
        let mut sessions: Vec<SessionId> = home
            .sessions
            .live()
            .iter()
            .map(|row| row.id.clone())
            .collect();
        if let Phase::Attached { session, .. } = &self.phase
            && !sessions.contains(session)
            && home.left_after_attach.as_ref() != Some(session)
        {
            sessions.push(session.clone());
        }
        sessions
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
    /// anything else, Ctrl+C included, does nothing. Under the close-all
    /// question Enter closes the counted sessions still live instead,
    /// and `c` does nothing; under the stop question Enter stops its
    /// session, and `c` does nothing. `None` unless a quit-surface
    /// question is open.
    pub(in crate::app) fn quit_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.quit_open() {
            return None;
        }
        let prompt = self.home.as_ref().and_then(|home| home.questions.prompt());
        let closing = matches!(prompt, Some(Prompt::CloseAll { .. }));
        let stopping = matches!(prompt, Some(Prompt::Stop { .. }));
        Some(match key {
            Key::Enter if closing => self.confirm_close_all(),
            Key::Enter if stopping => self.confirm_stop(),
            Key::Enter => Effect::Quit,
            Key::Char('c') if !closing && !stopping => self.close_all(),
            Key::Esc => {
                self.close_prompt();
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

    /// The quit question's choice, on home or over a session: leaving
    /// quits with the sessions running, closing all stops them first,
    /// staying closes the question. Under the close-all question the
    /// confirm row closes the counted sessions instead, staying cancels,
    /// and leaving does nothing. The stop question's confirm row stops
    /// its session, and the others cancel.
    pub(in crate::app::home) fn quit_click(&mut self, choice: QuitChoice) -> Effect {
        if !self.quit_open() {
            return Effect::None;
        }
        let prompt = self.home.as_ref().and_then(|home| home.questions.prompt());
        let stopping = matches!(prompt, Some(Prompt::Stop { .. }));
        if matches!(prompt, Some(Prompt::CloseAll { .. })) || stopping {
            return match choice {
                QuitChoice::CloseAll if stopping => self.confirm_stop(),
                QuitChoice::CloseAll => self.confirm_close_all(),
                QuitChoice::Leave | QuitChoice::Stay => {
                    self.close_prompt();
                    Effect::None
                }
            };
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
        let (lines, pairs) = self.close_lines(&working);
        if let Some(home) = self.home.as_mut() {
            for (session, id) in pairs {
                home.questions.closing.push((session, id));
            }
        }
        Effect::Exit(lines)
    }

    /// Enter in the close-all question: `close` with `now` each counted
    /// session still live, with a `summary` subscribe first where this
    /// connection holds nothing. The set is the sessions the overlay
    /// counted: one that left while it showed is skipped, and one that
    /// went live after it opened keeps running. With the link down, or
    /// nothing still live, it closes the question and sends nothing: the
    /// terminal stays open on home either way.
    fn confirm_close_all(&mut self) -> Effect {
        let open: Vec<SessionId> = match self.home.as_ref().and_then(|home| home.questions.prompt())
        {
            Some(Prompt::CloseAll { sessions, .. }) => sessions.clone(),
            _ => return Effect::None,
        };
        let live = self.live_sessions();
        let close: Vec<SessionId> = open.into_iter().filter(|id| live.contains(id)).collect();
        if close.is_empty() || !self.outbox.is_up() {
            self.close_prompt();
            return Effect::None;
        }
        let (lines, pairs) = self.close_lines(&close);
        if let Some(home) = self.home.as_mut() {
            for (session, id) in pairs {
                home.questions.asks.insert(id, Ask::Stop(session));
            }
        }
        self.close_prompt();
        self.draft.clear();
        if self.session().is_some() {
            self.go_home();
        }
        Effect::Send(lines)
    }

    /// The lines closing `sessions`: a `summary` subscribe first where
    /// this connection holds nothing, then a minted `close` with `now`
    /// each, with each session beside its close id. Both questions send
    /// the lines: the quit question records its closes in `closing`, the
    /// close-all question in `asks`. The command records its stops in
    /// `closing`, so shutdown never resumes a session it stopped.
    pub(super) fn close_lines(
        &mut self,
        sessions: &[SessionId],
    ) -> (Vec<String>, Vec<(SessionId, String)>) {
        let mut lines = Vec::new();
        let mut pairs = Vec::new();
        for session in sessions {
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
            pairs.push((session.clone(), id));
        }
        (lines, pairs)
    }

    /// Drops from `closing` every session whose `close` line was not
    /// written: a close never sent keeps its resume line on exit. A
    /// command stop that never went out ends in its stop uncertainty.
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
        self.delete_command_unsent(&ids);
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

#[cfg(test)]
#[path = "stop_tests.rs"]
mod stop_tests;
