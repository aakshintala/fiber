//! The input box's completion panels, the built-in commands and the key
//! map overlay (`docs/tui.md`, "Keys", "Bindings", "Slash commands",
//! "Quit").

use std::mem;
use std::time::Instant;

use contract::Envelope;
use contract::events::OpeningMessage;
use serde_json::json;

use super::{App, Effect, Kind, Link, Phase, mint, read, session_command};
use crate::keymap;
use crate::keys::Key;
use crate::slash::{self, SHOWN};

/// What the notice says when a command needs a session and none is
/// attached.
const NO_SESSION: &str = "No session on screen.";

/// The `@` panel's state.
#[derive(Debug)]
pub(super) struct FilePanel {
    /// The byte offset of the `@` in the draft.
    anchor: usize,
    /// The latest search result: matching paths, or why there are none.
    result: Option<Result<Vec<String>, String>>,
}

/// The `/` and `@` panels' and the key map overlay's state.
#[derive(Debug)]
pub(super) struct Overlays {
    /// The `/` list: built-in commands, then the attached session's skills.
    slash_rows: Vec<slash::Row>,
    /// Esc closed the `/` panel for this draft.
    slash_closed: bool,
    /// The selected row of the open completion panel.
    selected: usize,
    /// The `@` panel, while open.
    files: Option<FilePanel>,
    /// Bumped when the `@` panel opens and on every query change, never
    /// reset; a search result tagged with another is stale.
    generation: u64,
    /// The key map overlay's top row, while it is open.
    keymap: Option<usize>,
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            slash_rows: slash::rows(&[]),
            slash_closed: false,
            selected: 0,
            files: None,
            generation: 0,
            keymap: None,
        }
    }
}

/// The open completion panel's rows as drawn, and which is selected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Completions {
    /// At most [`SHOWN`] rows.
    pub(crate) lines: Vec<String>,
    /// The selected row among `lines`, when one is selectable.
    pub(crate) selected: Option<usize>,
}

impl App {
    /// Handles one key at `now`, read from the injected clock.
    pub(crate) fn on_key(&mut self, key: Key, now: Instant) -> Effect {
        let effect = self.route_key(key, now);
        self.edited();
        effect
    }

    /// The completion panel's height in rows; 0 while none is open.
    pub(super) fn completion_rows(&self) -> usize {
        self.completions()
            .map_or(0, |completions| completions.lines.len())
    }

    /// The completion panel above the input line, while one is open with
    /// rows and the approval panel is closed.
    pub(crate) fn completions(&self) -> Option<Completions> {
        if self.panel().is_some() {
            return None;
        }
        let (all, selectable): (Vec<String>, bool) = if self.slash_open() {
            let rows = slash::filter(&self.overlays.slash_rows, self.slash_query());
            (rows.into_iter().map(slash::Row::line).collect(), true)
        } else {
            match self
                .overlays
                .files
                .as_ref()
                .and_then(|panel| panel.result.as_ref())
            {
                Some(Ok(paths)) => (paths.clone(), true),
                Some(Err(error)) => (vec![format!("No files: {error}")], false),
                None => return None,
            }
        };
        if all.is_empty() {
            return None;
        }
        let start = slash::window_start(self.overlays.selected);
        let lines: Vec<String> = all.into_iter().skip(start).take(SHOWN).collect();
        let selected = selectable.then(|| self.overlays.selected.saturating_sub(start));
        Some(Completions { lines, selected })
    }

    /// Whether the `/` panel is open: the draft starts with `/`, holds no
    /// whitespace, and Esc has not closed the panel for it.
    fn slash_open(&self) -> bool {
        !self.overlays.slash_closed
            && self.draft.starts_with('/')
            && !self.draft.contains(char::is_whitespace)
    }

    /// The text after the `/`.
    fn slash_query(&self) -> &str {
        self.draft.get(1..).unwrap_or_default()
    }

    /// The names of the `/` panel's rows, in order.
    fn slash_names(&self) -> Vec<String> {
        slash::filter(&self.overlays.slash_rows, self.slash_query())
            .into_iter()
            .map(|row| row.name.clone())
            .collect()
    }

    /// The paths the `@` panel offers, in order.
    fn file_paths(&self) -> &[String] {
        match self
            .overlays
            .files
            .as_ref()
            .and_then(|panel| panel.result.as_ref())
        {
            Some(Ok(paths)) => paths,
            Some(Err(_)) | None => &[],
        }
    }

    /// The `@` panel's query: the text after its `@`.
    fn file_query(&self) -> Option<&str> {
        let anchor = self.overlays.files.as_ref()?.anchor;
        self.draft.get(anchor.saturating_add(1)..)
    }

    /// Whether the `@` panel is open.
    pub(crate) fn files_open(&self) -> bool {
        self.overlays.files.is_some()
    }

    /// The launch directory, which the `@` panel lists.
    pub(crate) fn workspace(&self) -> &std::path::Path {
        &self.workspace
    }

    /// The current search generation.
    pub(crate) fn generation(&self) -> u64 {
        self.overlays.generation
    }

    /// A search result tagged `generation`: shown when it is the current
    /// generation and the `@` panel is open; otherwise it is stale and
    /// changes nothing.
    pub(crate) fn on_files(&mut self, generation: u64, result: Result<Vec<String>, String>) {
        if generation != self.overlays.generation {
            return;
        }
        if let Some(panel) = &mut self.overlays.files {
            let len = result.as_ref().map_or(0, Vec::len);
            self.overlays.selected = self.overlays.selected.min(len.saturating_sub(1));
            panel.result = Some(result);
        }
    }

    /// Keeps the panels in step with the draft after every key: the `/`
    /// panel may open again once the draft no longer starts with `/`, and
    /// the `@` panel closes once its `@` is gone or the query holds
    /// whitespace.
    pub(super) fn edited(&mut self) {
        if !self.draft.starts_with('/') {
            self.overlays.slash_closed = false;
        }
        if let Some(panel) = &self.overlays.files {
            let open = self
                .draft
                .get(panel.anchor..)
                .is_some_and(|rest| rest.starts_with('@') && !rest.contains(char::is_whitespace));
            if !open {
                self.overlays.files = None;
            }
        }
    }

    /// Types one character into the draft. An `@` at the draft's start or
    /// after whitespace opens the `@` panel.
    pub(super) fn type_char(&mut self, ch: char) -> Effect {
        let opens = ch == '@'
            && self
                .draft
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
        self.draft.push(ch);
        self.overlays.selected = 0;
        if opens {
            self.overlays.files = Some(FilePanel {
                anchor: self.draft.len().saturating_sub(1),
                result: None,
            });
            self.overlays.generation = self.overlays.generation.saturating_add(1);
            return Effect::ListFiles;
        }
        self.query_changed()
    }

    /// Deletes the draft's last character.
    pub(super) fn backspace(&mut self) -> Effect {
        self.draft.pop();
        self.overlays.selected = 0;
        self.query_changed()
    }

    /// After an edit: a new search while the `@` panel stays open.
    fn query_changed(&mut self) -> Effect {
        self.edited();
        let Some(query) = self.file_query().map(str::to_owned) else {
            return Effect::None;
        };
        self.overlays.generation = self.overlays.generation.saturating_add(1);
        Effect::Search {
            generation: self.overlays.generation,
            query,
        }
    }

    /// A key for the open completion panel; `None` when no panel is open
    /// or the key is the input box's.
    pub(super) fn completion_key(&mut self, key: &Key) -> Option<Effect> {
        let names = if self.slash_open() {
            self.slash_names()
        } else if self.overlays.files.is_some() {
            self.file_paths().to_vec()
        } else {
            return None;
        };
        let chosen = names.get(self.overlays.selected).cloned();
        match key {
            Key::Up => self.overlays.selected = self.overlays.selected.saturating_sub(1),
            Key::Down => {
                self.overlays.selected = self
                    .overlays
                    .selected
                    .saturating_add(1)
                    .min(names.len().saturating_sub(1));
            }
            Key::Esc => {
                if self.overlays.files.take().is_none() {
                    self.overlays.slash_closed = true;
                }
            }
            Key::Tab | Key::Enter => {
                let chosen = chosen?;
                if let Some(panel) = self.overlays.files.take() {
                    self.draft.truncate(panel.anchor);
                    self.draft.push_str(&chosen);
                    self.draft.push(' ');
                } else if *key == Key::Tab {
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
                self.overlays.keymap = Some(0);
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
        self.overlays.slash_rows = slash::rows(&[]);
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
        // Home has a fresh draft: a rejected `close` gives only its notice.
        self.pending.insert(id, (Kind::Command, String::new()));
        self.go_home();
        Effect::Send(lines)
    }

    /// The key map overlay's top row, while it is open.
    pub(crate) fn keymap_top(&self) -> Option<usize> {
        self.overlays.keymap
    }

    /// Opens the key map overlay at its top.
    pub(super) fn open_keymap(&mut self) -> Effect {
        self.overlays.keymap = Some(0);
        Effect::None
    }

    /// A key while the key map is open: ↑ ↓ PageUp PageDown scroll it, Esc
    /// closes it, other keys do nothing. `None` while it is closed.
    pub(super) fn keymap_key(&mut self, key: &Key) -> Option<Effect> {
        let top = self.overlays.keymap?;
        let height = self.conversation_height();
        let page = height.saturating_sub(1).max(1);
        let total: usize = keymap::lines()
            .iter()
            .map(|line| crate::view::rows(ratatui::text::Line::raw(line.as_str()), self.width))
            .sum();
        let last = total.saturating_sub(height);
        self.overlays.keymap = match key {
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
        Some(Effect::None)
    }

    /// `opening_message`: the session's skills join the `/` list.
    pub(super) fn opening(&mut self, envelope: &Envelope) -> bool {
        if let Some(opening) = read!(envelope, OpeningMessage) {
            self.overlays.slash_rows = slash::rows(&opening.skills);
        }
        false
    }
}

#[cfg(test)]
#[path = "app_commands_tests.rs"]
mod tests;
