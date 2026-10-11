//! Home's foot question: deleting a session through the hub, and the
//! stops and deletes waiting for their answers.

use std::collections::HashMap;

use ratatui::layout::Rect;
use serde_json::{Map, Value, json};

use super::super::reconnect::UNANSWERED;
use super::super::{App, Effect, Link, mint, session_command};
use super::refusal;
use crate::home::{Level, Spot, State, dependents};
use crate::keys::Key;
use crate::mouse::TargetId;
use crate::view::max_question_scroll;
use contract::SessionId;

mod exit;

/// A stop or delete waiting for its answer: a refusal becomes the row's
/// note and a notice, an accepted delete drops the row.
pub(super) enum Ask {
    /// A `close` with `now` for a live row: stopping one session.
    Stop(SessionId),
    /// A `delete` for an exited row, cascading when the question named
    /// the sessions it would remove.
    Delete(SessionId),
}

/// The foot's question: deleting an exited session through the hub.
/// Delete is permanent, so the terminal asks first, naming the session
/// and everything `--cascade` would add.
pub(super) enum Prompt {
    /// Asking to delete `id`: without `expect` the plain question, with
    /// it the cascade question naming the sessions the delete removes.
    /// `scroll` shows later wrapped rows past the screen.
    Delete {
        id: SessionId,
        expect: Option<Vec<SessionId>>,
        scroll: usize,
    },
    /// Asking to quit while sessions work: Enter leaves them running,
    /// `c` closes them all now, Esc stays. It stores nothing: the
    /// question recomputes from the current rows on every frame.
    Quit,
}

impl Prompt {
    /// The session the question asks about; none for quitting.
    fn id(&self) -> Option<&SessionId> {
        match self {
            Prompt::Delete { id, .. } => Some(id),
            Prompt::Quit => None,
        }
    }
}

/// The foot's question, the stops and deletes waiting for their
/// answers, and the quit question's closes.
#[derive(Default)]
pub(super) struct Questions {
    /// The foot's question, while one is open: deleting a session.
    prompt: Option<Prompt>,
    /// The stops and deletes waiting for their answers, by command id.
    asks: HashMap<String, Ask>,
    /// The sessions `close` went out for from the quit question, with
    /// their close ids: a close never written keeps its resume line.
    closing: Vec<(SessionId, String)>,
}

impl Questions {
    /// The foot's question, while one is open.
    pub(super) fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }
}

impl App {
    /// The delete question's keys: Enter deletes, Esc keeps the
    /// session, Up and Down scroll its wrapped rows past the screen,
    /// Ctrl+C passes through to quit, and anything else is swallowed.
    pub(super) fn question_key(&mut self, key: &Key) -> Option<Effect> {
        match key {
            Key::Enter => Some(self.send_delete()),
            Key::Esc => {
                self.close_prompt();
                Some(Effect::None)
            }
            Key::Up => {
                self.scroll_question(false);
                Some(Effect::None)
            }
            Key::Down => {
                self.scroll_question(true);
                Some(Effect::None)
            }
            Key::CtrlC => None,
            Key::Char(_)
            | Key::Backspace
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
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => Some(Effect::None),
        }
    }

    /// Settles every delete waiting after a dropped connection: a
    /// delete is a hub command and is not resent, so each one settles
    /// as unanswered, in id order. With no home it does nothing.
    pub(super) fn settle_deletes(&mut self) {
        let Some(home) = self.home.as_ref() else {
            return;
        };
        let mut deletes: Vec<String> = home
            .questions
            .asks
            .iter()
            .filter(|(_, ask)| matches!(ask, Ask::Delete(_)))
            .map(|(id, _)| id.clone())
            .collect();
        deletes.sort();
        let payload =
            Map::from_iter([("message".to_owned(), Value::String(UNANSWERED.to_owned()))]);
        for id in deletes {
            if let Some(ask) = self.take_ask(&id) {
                self.answer_ask(ask, false, &payload);
            }
        }
    }

    /// A row's ✕: `close` with `now` for a live session, stopping it,
    /// with a `summary` subscribe first when this connection holds
    /// nothing for it; the delete question for an exited row. With the
    /// link down nothing goes out. A refusal of the close becomes the
    /// row's note and a notice.
    pub(super) fn stop_or_ask(&mut self, key: u64) -> Effect {
        if !self.on_home() || self.link != Link::Up {
            return Effect::None;
        }
        let row = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .cloned();
        let Some(row) = row else {
            return Effect::None;
        };
        if row.state == State::Unreadable {
            self.notices.push(
                "Cannot attach: this session's schema is newer than this terminal reads."
                    .to_owned(),
            );
            return Effect::None;
        }
        if row.left.is_some() {
            self.ask_delete(key);
            return Effect::None;
        }
        let session = row.id.clone();
        let mut lines = Vec::new();
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.subs.expected(&session).is_none())
        {
            lines.push(self.subscribe(&session, Level::Summary));
        }
        let id = mint();
        lines.push(session_command(&id, "close", &session, Some(json!({"now": true}))).to_string());
        if let Some(home) = self.home.as_mut() {
            home.questions.asks.insert(id, Ask::Stop(session));
        }
        Effect::Send(lines)
    }

    /// Opens the delete question for the row with `key`. Delete through
    /// the hub is permanent, so the terminal asks first, naming the
    /// session.
    fn ask_delete(&mut self, key: u64) {
        let id = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .map(|row| row.id.clone());
        if let (Some(home), Some(id)) = (self.home.as_mut(), id) {
            home.questions.prompt = Some(Prompt::Delete {
                id,
                expect: None,
                scroll: 0,
            });
        }
    }

    /// Closes the foot's question, keeping the session.
    fn close_prompt(&mut self) {
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = None;
        }
    }

    /// Scrolls the delete question one row: Up toward its first rows,
    /// Down toward its later ones, clamped to the wrapped rows past the
    /// screen. Scrolling a short question changes nothing drawn, and
    /// Down past the end holds, so one Up steps back one row.
    fn scroll_question(&mut self, down: bool) {
        let max = self
            .home_screen()
            .map(|screen| {
                max_question_scroll(
                    self,
                    &screen,
                    Rect::new(0, 0, self.screen.width(), self.screen.height()),
                )
            })
            .unwrap_or(0);
        if let Some(home) = self.home.as_mut()
            && let Some(Prompt::Delete { scroll, .. }) = home.questions.prompt.as_mut()
        {
            let clamped = (*scroll).min(max);
            *scroll = if down {
                clamped.saturating_add(1).min(max)
            } else {
                clamped.saturating_sub(1)
            };
        }
    }

    /// Backspace, or Delete through `home_edit`, on a focused row: the
    /// delete question when it exited. On a focused live or unreadable
    /// row it does nothing, and on any other focused stop it is
    /// swallowed; with the box focused the draft keeps the key.
    pub(super) fn delete_key(&mut self) -> Option<Effect> {
        let key = match self.focus {
            None => return None,
            Some(TargetId::Home(Spot::Entry(key) | Spot::Stop(key))) => key,
            Some(_) => return Some(Effect::None),
        };
        if self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .is_some_and(|row| row.left.is_some())
        {
            self.ask_delete(key);
        }
        Some(Effect::None)
    }

    /// Enter in the delete question: `delete` for its session, cascading
    /// with the confirmed set from the cascade question. With the link
    /// down nothing goes out and the question stays.
    fn send_delete(&mut self) -> Effect {
        let ask = self
            .home
            .as_ref()
            .and_then(|home| match &home.questions.prompt {
                Some(Prompt::Delete { id, expect, .. }) => Some((id.clone(), expect.clone())),
                Some(Prompt::Quit) | None => None,
            });
        let Some((id, expect)) = ask else {
            return Effect::None;
        };
        if self.link != Link::Up {
            return Effect::None;
        }
        let command = mint();
        let line = match &expect {
            None => json!({"id": command, "command": "delete", "args": {"session": id.0}}),
            Some(expect) => json!({
                "id": command,
                "command": "delete",
                "args": {
                    "session": id.0,
                    "cascade": true,
                    "expect": expect.iter().map(|session| &session.0).collect::<Vec<_>>(),
                },
            }),
        }
        .to_string();
        if let Some(home) = self.home.as_mut() {
            home.questions.asks.insert(command, Ask::Delete(id));
        }
        Effect::Send(vec![line])
    }

    /// Takes the stop or delete waiting on command `id`, if any.
    pub(super) fn take_ask(&mut self, id: &str) -> Option<Ask> {
        self.home
            .as_mut()
            .and_then(|home| home.questions.asks.remove(id))
    }

    /// A refusal shows on the row, in place of its detail.
    fn note(&mut self, id: &SessionId, note: String) {
        if let Some(home) = self.home.as_mut() {
            home.sessions.note(id, note);
        }
    }

    /// Answers a stop or delete. An accepted delete drops the row and
    /// asks the first `recent` page again. A dependents refusal, or a
    /// stale cascade refusal, reopens the cascade question naming the
    /// set the delete would remove now; every other refusal, and one
    /// naming no other session, becomes the row's note and a notice.
    pub(super) fn answer_ask(
        &mut self,
        ask: Ask,
        accepted: bool,
        payload: &serde_json::Map<String, Value>,
    ) -> Vec<String> {
        match ask {
            Ask::Stop(session) => {
                if !accepted {
                    let message = refusal(payload);
                    self.note(&session, message.clone());
                    self.notices.push(message);
                }
                Vec::new()
            }
            Ask::Delete(id) => {
                if accepted {
                    self.clear_prompt(&id);
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.remove(&id);
                    }
                    return self.ask_recent_first();
                }
                let (code, message) = refusal_parts(payload);
                if code == "session_has_dependents" || code == "stale_request" {
                    // The hub names the root too, so the row goes first
                    // and the set deduplicates keeping first occurrence.
                    let mut expect = vec![id.clone()];
                    for session in dependents(&message) {
                        if !expect.contains(&session) {
                            expect.push(session);
                        }
                    }
                    if expect.len() > 1 {
                        if let Some(home) = self.home.as_mut() {
                            home.questions.prompt = Some(Prompt::Delete {
                                id,
                                expect: Some(expect),
                                scroll: 0,
                            });
                        }
                        return Vec::new();
                    }
                }
                self.note(&id, message.clone());
                self.notices.push(message);
                self.clear_prompt(&id);
                Vec::new()
            }
        }
    }

    /// Closes the foot's question when it asks about `id`: an answer for
    /// an older question leaves a newer one open.
    fn clear_prompt(&mut self, id: &SessionId) {
        if let Some(home) = self.home.as_mut()
            && home
                .questions
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.id() == Some(id))
        {
            home.questions.prompt = None;
        }
    }
}

/// A refusal's code and message.
fn refusal_parts(payload: &serde_json::Map<String, Value>) -> (&str, String) {
    let code = payload
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    (code, refusal(payload))
}
