//! The input box's completion panels, the commands Enter and Esc send,
//! the built-in commands and the key map overlay (`docs/tui.md`, "Keys",
//! "Bindings", "Slash commands", "Quit").

use std::path::PathBuf;
use std::time::Instant;

use contract::SessionId;
use contract::events::{CommandAccepted, CommandResult};
use serde_json::json;

use super::{App, Effect, Kind, Link, Phase, mint, session_command};
use crate::editor::Target;
use crate::input::Draft;
use crate::keymap;
use crate::keys::{Edit, Key};
use crate::shell;
use crate::slash::{self, SHOWN};

/// What the notice says when a command needs a session and none is
/// attached.
const NO_SESSION: &str = "No session on screen.";

/// The `@` panel's state.
#[derive(Debug)]
pub(super) struct FilePanel {
    /// The draft position of the `@`.
    anchor: usize,
    /// The latest search result: matching paths, or why there are none.
    result: Option<Result<Vec<String>, String>>,
}

/// The `/` and `@` panels' and the key map overlay's state.
#[derive(Debug)]
pub(super) struct Overlays {
    /// The `/` list: built-in commands, then the attached session's
    /// `commands` answer.
    slash_rows: Vec<slash::Row>,
    /// The id of the latest `commands` sent; only its answer fills the list.
    commands_id: Option<String>,
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
            commands_id: None,
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

/// The draft's content as a prompt's `content` argument: text runs and
/// image parts in order, each run one text part. `SentPart` serializes as
/// the wire part, `type` with the part's fields, so the content converts
/// directly. Every part serializes, so the fallback never runs.
pub(super) fn content_arg(draft: &Draft) -> serde_json::Value {
    let content =
        serde_json::to_value(draft.content()).unwrap_or(serde_json::Value::Array(Vec::new()));
    json!({ "content": content })
}

impl App {
    /// Handles one key at `now`, read from the injected clock. A recall
    /// waiting for a page waits on only through ↑ and the keys that move
    /// the view.
    pub(crate) fn on_key(&mut self, key: Key, now: Instant) -> Effect {
        if !matches!(
            key,
            Key::Up | Key::PageUp | Key::PageDown | Key::End | Key::CtrlO
        ) {
            self.history.cancel();
        }
        let effect = self.route_key(key, now);
        self.edited();
        self.settle();
        effect
    }

    /// Handles one key that edits the draft, the approval panel first.
    /// Nothing while the key map is open, while navigating with the panel
    /// closed, or in the Ctrl+R panel. A recall waiting for a page waits
    /// no more.
    pub(crate) fn on_edit(&mut self, edit: Edit) -> Effect {
        self.armed_at = None;
        self.history.cancel();
        if let Some(effect) = self.config_view_edit(&edit) {
            return effect;
        }
        // Delete on a focused home row asks to delete it when it
        // exited, ahead of the focus early return below.
        if let Some(effect) = self.home_edit(&edit) {
            return effect;
        }
        if self.overlays.keymap.is_some() {
            return Effect::None;
        }
        if let Some(effect) = self.offer_edit(&edit) {
            return effect;
        }
        if let Some(effect) = self.find_edit(&edit) {
            return effect;
        }
        // A request on the panel takes the edit ahead of the prompt search.
        if self.panel().is_none() && (self.search_edit(&edit) || self.focus.is_some()) {
            return Effect::None;
        }
        crate::input::route(edit, &mut self.draft, &mut self.queue);
        self.overlays.selected = 0;
        let effect = self.query_changed();
        self.settle();
        effect
    }

    /// A key the draft takes: a character or Backspace, which may open or
    /// search the `@` panel, or ↑ ↓ by wrapped row and then through earlier
    /// prompts. `None` for any other key.
    pub(super) fn draft_key(&mut self, key: &Key) -> Option<Effect> {
        match key {
            Key::Char(ch) => Some(self.type_char(*ch)),
            Key::Backspace => Some(self.backspace()),
            Key::Up | Key::Down => self.recall_key(key),
            Key::Enter
            | Key::Esc
            | Key::CtrlC
            | Key::CtrlO
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlF => None,
        }
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
        if let Some(search) = self.search_panel() {
            return Some(search);
        }
        let (all, selectable): (Vec<String>, bool) = if self.slash_open() {
            let rows = slash::filter(&self.overlays.slash_rows, &self.slash_query());
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
        let text = self.draft.expand();
        !self.overlays.slash_closed && text.starts_with('/') && !text.contains(char::is_whitespace)
    }

    /// The text after the `/`.
    fn slash_query(&self) -> String {
        self.draft.expand().get(1..).unwrap_or_default().to_owned()
    }

    /// The names of the `/` panel's rows, in order.
    fn slash_names(&self) -> Vec<String> {
        slash::filter(&self.overlays.slash_rows, &self.slash_query())
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

    /// The `@` panel's query: the text after its `@` up to whitespace.
    fn file_query(&self) -> Option<String> {
        let anchor = self.overlays.files.as_ref()?.anchor;
        self.draft.mention(anchor).map(|(query, _)| query)
    }

    /// Whether the `@` panel is open.
    pub(crate) fn files_open(&self) -> bool {
        self.overlays.files.is_some()
    }

    /// The workspace in use: the attached session's row workspace on
    /// home, else the launch directory.
    pub(crate) fn workspace(&self) -> PathBuf {
        if self.home.is_some() {
            self.home_workspace()
        } else {
            self.workspace.clone()
        }
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
        self.settle();
    }

    /// Keeps the panels in step with the draft after every key: the `/`
    /// panel may open again once the draft no longer starts with `/`, and
    /// the `@` panel closes once its `@` is gone or the cursor leaves the
    /// query, the text from the `@` to whitespace.
    pub(super) fn edited(&mut self) {
        self.history.sync(&self.draft.expand());
        if !self.draft.expand().starts_with('/') {
            self.overlays.slash_closed = false;
        }
        if let Some(panel) = &self.overlays.files
            && self.draft.mention(panel.anchor).is_none()
        {
            self.overlays.files = None;
        }
    }

    /// Types one character at the cursor. An `@` at the draft's start or
    /// after whitespace opens the `@` panel.
    pub(super) fn type_char(&mut self, ch: char) -> Effect {
        let opens = ch == '@' && self.draft.after_space();
        self.draft.insert(ch);
        self.overlays.selected = 0;
        if opens {
            self.overlays.files = Some(FilePanel {
                anchor: self.draft.position().saturating_sub(1),
                result: None,
            });
            self.overlays.generation = self.overlays.generation.saturating_add(1);
            return Effect::ListFiles;
        }
        self.query_changed()
    }

    /// Deletes the piece before the cursor.
    pub(super) fn backspace(&mut self) -> Effect {
        self.draft.backspace();
        self.overlays.selected = 0;
        self.query_changed()
    }

    /// After an edit: a new search while the `@` panel stays open.
    fn query_changed(&mut self) -> Effect {
        self.edited();
        let Some(query) = self.file_query() else {
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
                    let end = self
                        .draft
                        .mention(panel.anchor)
                        .map_or(panel.anchor, |(_, end)| end);
                    self.draft.replace(panel.anchor..end, &format!("{chosen} "));
                } else if *key == Key::Tab {
                    self.draft.set(&format!("/{chosen} "));
                } else {
                    self.draft.set(&format!("/{chosen}"));
                    return Some(self.on_enter());
                }
            }
            // Ctrl+V never reaches the draft behind an open panel.
            Key::CtrlV => {}
            Key::Char(_)
            | Key::Backspace
            | Key::CtrlC
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::BackTab
            | Key::F1
            | Key::CtrlO
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlF => return None,
        }
        Some(Effect::None)
    }

    /// Runs the draft when its first word is a built-in command; `None`
    /// when it is not one, and the draft goes out as typed. A draft
    /// holding an image is never a built-in command: Enter sends it as a
    /// prompt.
    pub(super) fn built_in(&mut self) -> Option<Effect> {
        if self.draft.has_image() {
            return None;
        }
        let draft = self.draft.expand();
        let draft = draft.trim();
        let (head, rest) = draft.split_once(char::is_whitespace).unwrap_or((draft, ""));
        let name = head.strip_prefix('/')?;
        if !slash::is_built_in(name) {
            return None;
        }
        let rest = rest.trim().to_owned();
        let effect = match name {
            "home" | "new" => self.leave(),
            "resume" => self.resume_list(),
            "panel" => {
                self.draft.clear();
                self.toggle_panel()
            }
            "handoff" => {
                let args = (!rest.is_empty()).then(|| json!({ "instructions": rest }));
                self.send_command("handoff", args)
            }
            "name" => self.send_command("name", Some(json!({ "text": rest }))),
            "reload" => self.send_command("reload", None),
            "close" => self.close(),
            "quit" => self.quit(),
            "approvals" => {
                self.draft.clear();
                self.open_first()
            }
            "settings" => self.open_config_view(super::ConfigView::Settings),
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
    pub(super) fn go_home(&mut self) {
        self.draft.clear();
        self.close_find();
        if matches!(self.phase, Phase::Pending { .. }) {
            return;
        }
        self.phase = Phase::Starting;
        self.clear_selection();
        self.screen.clear();
        self.panel_state.reset();
        self.offer = crate::offer::Offer::default();
        self.overlays.slash_rows = slash::rows(&[]);
        self.overlays.commands_id = None;
    }

    /// The attached session, when the command can go out: with none
    /// attached, a notice and the draft cleared; with the link not up, the
    /// draft stays.
    pub(super) fn command_session(&mut self) -> Option<(contract::SessionId, bool)> {
        let Phase::Attached { session, busy } = &self.phase else {
            self.notices.push(NO_SESSION.to_owned());
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
        let draft = std::mem::take(&mut self.draft);
        self.pending.insert(id, (Kind::Command, draft));
        Effect::Send(vec![line])
    }

    /// `/close`: one `close` with `now`, whether the session is busy or
    /// idle, then home.
    fn close(&mut self) -> Effect {
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let id = mint();
        let lines =
            vec![session_command(&id, "close", &session, Some(json!({"now": true}))).to_string()];
        // Home has a fresh draft: a rejected `close` gives only its notice.
        self.pending.insert(id, (Kind::Command, Draft::default()));
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
        let total: usize = keymap::lines(self.keys())
            .iter()
            .map(|line| {
                crate::view::rows(ratatui::text::Line::raw(line.as_str()), self.column_width())
            })
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
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlO
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlF => Some(top),
        };
        Some(Effect::None)
    }

    /// A `commands` line for `session`, sent on attach and after each
    /// `reloaded`; its answer, and no older one, fills the `/` list.
    pub(super) fn ask_commands(&mut self, session: &SessionId) -> String {
        let id = mint();
        let line = session_command(&id, "commands", session, None).to_string();
        self.overlays.commands_id = Some(id);
        line
    }

    /// A session's `command_accepted`: the answer to the latest `commands`
    /// sent replaces the session's rows of the `/` list.
    pub(super) fn commands_answered(&mut self, accepted: &CommandAccepted) {
        if self.overlays.commands_id.as_deref() != Some(accepted.command_id.0.as_str()) {
            return;
        }
        if let Some(CommandResult::Commands { commands }) = &accepted.result {
            self.overlays.slash_rows = slash::rows(commands);
            self.overlays.commands_id = None;
        }
    }

    /// Enter sends the draft: `start` with no session, `prompt` when
    /// idle, `steer` during a turn, and `shell` for a `!` command whenever
    /// attached. A draft holding an image sends its content as a prompt
    /// or `steer`, never as a built-in or a `!` command.
    pub(super) fn on_enter(&mut self) -> Effect {
        if let Some(effect) = self.built_in() {
            return effect;
        }
        let has_image = self.draft.has_image();
        let text = self.draft.expand();
        // A command sent after the connection is lost goes nowhere, so the
        // draft stays. An empty draft stays too, unless it holds an image.
        if self.link == Link::Down {
            return Effect::None;
        }
        if !has_image && text.trim().is_empty() {
            return Effect::None;
        }
        if self.steering.is_selected() {
            return self.amend();
        }
        let id = mint();
        let args = content_arg(&self.draft);
        let shell = (!has_image).then(|| shell::parse(&text)).flatten();
        let (kind, line) = match (&self.phase, shell) {
            (Phase::Starting | Phase::Pending { .. }, Some(_)) => {
                self.notices.push("Start a session first.".to_owned());
                return Effect::None;
            }
            (Phase::Pending { .. }, None) => return Effect::None,
            (Phase::Starting, None) => {
                let args = self.start_args();
                let line = json!({"id": id, "command": "start", "args": args});
                (Kind::Start, line)
            }
            (Phase::Attached { session, .. }, Some((command, send))) => {
                (Kind::Shell, shell::command(&id, session, command, send))
            }
            (Phase::Attached { session, busy }, None) => {
                let (kind, command) = if *busy {
                    (Kind::Steer, "steer")
                } else {
                    (Kind::Prompt, "prompt")
                };
                (kind, session_command(&id, command, session, Some(args)))
            }
        };
        if kind == Kind::Start {
            self.phase = Phase::Pending {
                command_id: id.clone(),
            };
        }
        let draft = std::mem::take(&mut self.draft);
        self.pending.insert(id, (kind, draft));
        let line = line.to_string();
        if self.link == Link::Up {
            Effect::Send(vec![line])
        } else {
            // An Enter before the hub connects is held, not lost: `start`
            // goes out once the hub speaks `hub_hello`.
            self.held.push(line);
            Effect::None
        }
    }

    /// The first prompt of a session `start` made, carrying `draft`, sent
    /// after its `subscribe` so the session counts this terminal before
    /// the prompt. A rejection returns the draft to an empty box.
    pub(super) fn first_prompt(&mut self, session: &SessionId, draft: Draft) -> String {
        let id = mint();
        let args = content_arg(&draft);
        let line = session_command(&id, "prompt", session, Some(args)).to_string();
        self.pending.insert(id, (Kind::Prompt, draft));
        line
    }

    /// Esc with nothing open interrupts the turn: `cancel`, only when busy.
    pub(super) fn on_esc(&mut self) -> Effect {
        let session = match &self.phase {
            Phase::Attached {
                session,
                busy: true,
            } if self.connected() => session.clone(),
            Phase::Starting | Phase::Pending { .. } | Phase::Attached { .. } => {
                return Effect::None;
            }
        };
        let id = mint();
        let line = session_command(&id, "cancel", &session, None).to_string();
        self.pending.insert(id, (Kind::Cancel, Draft::default()));
        Effect::Send(vec![line])
    }

    /// Ctrl+G: the paste token beside the cursor, or else the whole draft,
    /// every token expanded. A recall waiting for a page waits no more.
    pub(super) fn open_in_editor(&mut self) -> Effect {
        if let Some(number) = self.draft.token_at_cursor() {
            return self.open_token(number);
        }
        self.history.cancel();
        Effect::Editor {
            target: Target::Draft,
            text: self.draft.expand(),
        }
    }

    /// Paste token `number`'s text in the editor; nothing when the draft
    /// holds no such token. A recall waiting for a page waits no more.
    pub(super) fn open_token(&mut self, number: usize) -> Effect {
        self.history.cancel();
        match self.draft.token_text(number) {
            Some(text) => Effect::Editor {
                target: Target::Token(number),
                text: text.to_owned(),
            },
            None => Effect::None,
        }
    }

    /// The editor returned: its text replaces `target`'s, the whole draft's
    /// as typed with image labels relinked and the cursor at its end; an
    /// error is the notice, and the draft stays.
    pub(crate) fn editor_returned(&mut self, target: Target, result: Result<String, String>) {
        match (result, target) {
            (Err(notice), _) => self.notices.push(notice),
            (Ok(text), Target::Token(number)) => self.draft.set_token(number, &text),
            (Ok(text), Target::Draft) => self.draft.edited(&text),
            (Ok(_), Target::Item) => {}
        }
        self.overlays.selected = 0;
        self.edited();
        self.settle();
    }

    /// A clipboard image read finished: its image lands at the cursor
    /// when its ticket is running and the box holds its draft, its notice
    /// shows, and anything else is dropped silently.
    pub(crate) fn on_image(&mut self, ticket: u64, result: Result<String, String>) {
        match self.paste.land(ticket, self.draft.serial(), result) {
            super::paste::Landed::Image(data) => {
                self.draft.insert_image(data);
                self.edited();
                self.settle();
            }
            super::paste::Landed::Notice(notice) => {
                self.notices.push(notice);
                self.settle();
            }
            super::paste::Landed::Dropped => {}
        }
    }
}

#[cfg(test)]
#[path = "app_commands_tests.rs"]
mod tests;
