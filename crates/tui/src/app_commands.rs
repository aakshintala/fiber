//! The input box's completion panels, the built-in commands and the key
//! map overlay (`docs/tui.md`, "Keys", "Bindings", "Slash commands",
//! "Quit").

use std::mem;

use serde_json::json;

use super::{App, Effect, Kind, Link, Phase, mint, session_command};
use crate::keymap;
use crate::keys::Key;
use crate::slash::{self, SHOWN};

/// What the notice says when a command needs a session and none is
/// attached.
const NO_SESSION: &str = "No session on screen.";

/// The open completion panel's rows as drawn, and which is selected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Completions {
    /// At most [`SHOWN`] rows.
    pub(crate) lines: Vec<String>,
    /// The selected row among `lines`, when one is selectable.
    pub(crate) selected: Option<usize>,
}

impl App {
    /// The completion panel above the input line, while one is open with
    /// rows and the approval panel is closed.
    pub(crate) fn completions(&self) -> Option<Completions> {
        if self.panel().is_some() {
            return None;
        }
        let (all, selectable): (Vec<String>, bool) = if self.slash_open() {
            let rows = slash::filter(&self.slash_rows, self.slash_query());
            (rows.into_iter().map(slash::Row::line).collect(), true)
        } else {
            return None;
        };
        if all.is_empty() {
            return None;
        }
        let start = slash::window_start(self.selected);
        let lines: Vec<String> = all.into_iter().skip(start).take(SHOWN).collect();
        let selected = selectable.then(|| self.selected.saturating_sub(start));
        Some(Completions { lines, selected })
    }

    /// Whether the `/` panel is open: the draft starts with `/`, holds no
    /// whitespace, and Esc has not closed the panel for it.
    fn slash_open(&self) -> bool {
        !self.slash_closed
            && self.draft.starts_with('/')
            && !self.draft.contains(char::is_whitespace)
    }

    /// The text after the `/`.
    fn slash_query(&self) -> &str {
        self.draft.get(1..).unwrap_or_default()
    }

    /// The names of the `/` panel's rows, in order.
    fn slash_names(&self) -> Vec<String> {
        slash::filter(&self.slash_rows, self.slash_query())
            .into_iter()
            .map(|row| row.name.clone())
            .collect()
    }

    /// Keeps the panels in step with the draft after every key: the `/`
    /// panel may open again once the draft no longer starts with `/`, and
    /// the `@` panel closes once its `@` is gone or the query holds
    /// whitespace.
    pub(super) fn edited(&mut self) {
        if !self.draft.starts_with('/') {
            self.slash_closed = false;
        }
    }

    /// Types one character into the draft.
    pub(super) fn type_char(&mut self, ch: char) -> Effect {
        self.draft.push(ch);
        self.selected = 0;
        Effect::None
    }

    /// Deletes the draft's last character.
    pub(super) fn backspace(&mut self) -> Effect {
        self.draft.pop();
        self.selected = 0;
        Effect::None
    }

    /// A key for the open completion panel; `None` when no panel is open
    /// or the key is the input box's.
    pub(super) fn completion_key(&mut self, key: &Key) -> Option<Effect> {
        let names = if self.slash_open() {
            self.slash_names()
        } else {
            return None;
        };
        let chosen = names.get(self.selected).cloned();
        match key {
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => {
                self.selected = self
                    .selected
                    .saturating_add(1)
                    .min(names.len().saturating_sub(1));
            }
            Key::Esc => self.slash_closed = true,
            Key::Tab | Key::Enter => {
                let chosen = chosen?;
                if *key == Key::Tab {
                    self.draft = format!("/{chosen} ");
                } else {
                    self.draft = format!("/{chosen}");
                    return Some(self.on_enter());
                }
            }
            Key::Char(_)
            | Key::Backspace
            | Key::CtrlC
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::BackTab
            | Key::F1
            | Key::CtrlO => return None,
        }
        Some(Effect::None)
    }

    /// Runs the draft when its first word is a built-in command; `None`
    /// when it is not one, and the draft goes out as typed.
    pub(super) fn built_in(&mut self) -> Option<Effect> {
        let draft = self.draft.trim();
        let (head, rest) = draft.split_once(char::is_whitespace).unwrap_or((draft, ""));
        let name = head.strip_prefix('/')?;
        if !slash::is_built_in(name) {
            return None;
        }
        let rest = rest.trim().to_owned();
        let effect = match name {
            "home" | "new" => {
                self.go_home();
                Effect::None
            }
            "handoff" => {
                let args = (!rest.is_empty()).then(|| json!({ "instructions": rest }));
                self.send_command("handoff", args)
            }
            "reload" => self.send_command("reload", None),
            "close" => self.close(),
            "quit" => Effect::Quit,
            "approvals" => {
                self.draft.clear();
                self.open_first()
            }
            // `?` and `help`.
            _ => {
                self.draft.clear();
                self.keymap = Some(0);
                Effect::None
            }
        };
        Some(effect)
    }

    /// Returns to the screen before a session: the conversation and the
    /// draft cleared, the old session left running. While a `start` waits
    /// for its answer only the draft is cleared.
    fn go_home(&mut self) {
        self.draft.clear();
        if matches!(self.phase, Phase::Pending { .. }) {
            return;
        }
        self.phase = Phase::Starting;
        self.turns.clear();
        self.slash_rows = slash::rows(&[]);
        self.follow();
    }

    /// The attached session, when the command can go out: with none
    /// attached, a notice and the draft cleared; with the link not up, the
    /// draft stays.
    fn command_session(&mut self) -> Option<(contract::SessionId, bool)> {
        let Phase::Attached { session, busy } = &self.phase else {
            self.notice = Some(NO_SESSION.to_owned());
            self.draft.clear();
            return None;
        };
        (self.link == Link::Up).then(|| (session.clone(), *busy))
    }

    /// Sends `command` to the attached session. A rejection returns the
    /// draft.
    fn send_command(&mut self, command: &str, args: Option<serde_json::Value>) -> Effect {
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let id = mint();
        let line = session_command(&id, command, &session, args).to_string();
        self.pending
            .insert(id, (Kind::Command, mem::take(&mut self.draft)));
        Effect::Send(vec![line])
    }

    /// `/close`: `cancel` during a turn, then `close`, then home.
    fn close(&mut self) -> Effect {
        let Some((session, busy)) = self.command_session() else {
            return Effect::None;
        };
        let mut lines = Vec::new();
        if busy {
            let id = mint();
            lines.push(session_command(&id, "cancel", &session, None).to_string());
            self.pending.insert(id, (Kind::Cancel, String::new()));
        }
        let id = mint();
        lines.push(session_command(&id, "close", &session, None).to_string());
        self.pending
            .insert(id, (Kind::Command, mem::take(&mut self.draft)));
        self.go_home();
        Effect::Send(lines)
    }

    /// The key map overlay's top row, while it is open.
    pub(crate) fn keymap_top(&self) -> Option<usize> {
        self.keymap
    }

    /// A key while the key map is open: ↑ ↓ PageUp PageDown scroll it, Esc
    /// closes it, other keys do nothing.
    pub(super) fn keymap_key(&mut self, key: &Key) {
        let Some(top) = self.keymap else {
            return;
        };
        let height = self.conversation_height();
        let page = height.saturating_sub(1).max(1);
        let total: usize = keymap::lines()
            .iter()
            .map(|line| crate::view::rows(ratatui::text::Line::raw(line.as_str()), self.width))
            .sum();
        let last = total.saturating_sub(height);
        self.keymap = match key {
            Key::Esc => None,
            Key::Up => Some(top.saturating_sub(1)),
            Key::Down => Some(top.saturating_add(1).min(last)),
            Key::PageUp => Some(top.saturating_sub(page)),
            Key::PageDown => Some(top.saturating_add(page).min(last)),
            Key::Char(_)
            | Key::Backspace
            | Key::Enter
            | Key::CtrlC
            | Key::End
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlO => Some(top),
        };
    }
}

#[cfg(test)]
#[path = "app_commands_tests.rs"]
mod tests;
