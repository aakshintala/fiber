//! The `/` and `@` panels' state: the rows the session's `commands`
//! answer fills, the open panel's selection, and the `@` search's
//! generation (`docs/tui.md`, "Slash commands").

use contract::SessionId;
use contract::events::{CommandAccepted, CommandResult};

use super::{App, Effect, mint, session_command};
use crate::completion_rows::Completions;
use crate::keys::Key;
use crate::slash;

/// The `@` panel's state.
#[derive(Debug)]
pub(super) struct FilePanel {
    /// The draft position of the `@`.
    pub(super) anchor: usize,
    /// The latest search result: matching paths, or why there are none.
    pub(super) result: Option<Result<Vec<String>, String>>,
}

/// The `/` and `@` panels' state.
#[derive(Debug)]
pub(super) struct Completion {
    /// The `/` list: built-in commands, then the attached session's
    /// `commands` answer.
    pub(super) slash_rows: Vec<slash::Row>,
    /// The extension commands of the latest `commands` answer, in answer
    /// order: only its answer fills the list.
    pub(super) extensions: Vec<String>,
    /// The id of the latest `commands` sent; only its answer fills the list.
    pub(super) commands_id: Option<String>,
    /// Esc closed the `/` panel for this draft.
    pub(super) slash_closed: bool,
    /// Esc closed the `/theme ` panel for this draft.
    pub(super) theme_closed: bool,
    /// The selected row of the open completion panel.
    pub(super) selected: usize,
    /// The `@` panel, while open.
    pub(super) files: Option<FilePanel>,
    /// Bumped when the `@` panel opens and on every query change, never
    /// reset; a search result tagged with another is stale.
    pub(super) generation: u64,
}

impl Default for Completion {
    fn default() -> Self {
        Self {
            slash_rows: slash::rows(&[]),
            extensions: Vec::new(),
            commands_id: None,
            slash_closed: false,
            theme_closed: false,
            selected: 0,
            files: None,
            generation: 0,
        }
    }
}

impl Completion {
    /// A `commands` line for `session`, sent on attach and after each
    /// `reloaded`; its answer, and no older one, fills the `/` list.
    /// Until it answers, the list holds the built-ins only.
    pub(super) fn ask_commands(&mut self, session: &SessionId) -> String {
        let id = mint();
        let line = session_command(&id, "commands", session, None).to_string();
        self.slash_rows = slash::rows(&[]);
        self.extensions.clear();
        self.commands_id = Some(id);
        line
    }

    /// Whether `id` is the latest `commands` sent, still unanswered.
    pub(super) fn awaits(&self, id: &str) -> bool {
        self.commands_id.as_deref() == Some(id)
    }

    /// A session's `command_accepted`: the answer to the latest `commands`
    /// sent replaces the session's rows of the `/` list.
    pub(super) fn commands_answered(&mut self, accepted: &CommandAccepted) {
        if !self.awaits(&accepted.command_id.0) {
            return;
        }
        if let Some(CommandResult::Commands { commands }) = &accepted.result {
            self.slash_rows = slash::rows(commands);
            self.extensions = slash::extensions(commands);
            self.commands_id = None;
        }
    }
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;

impl App {
    /// The completion panel above the input line, while one is open and
    /// the approval panel is closed. An empty `/` list shows "no
    /// matches", an empty `@` result "no files match"; before an `@`
    /// search answers, no panel shows. A `/theme ` draft offers the
    /// theme names, while any match; with none, no panel shows, so Esc
    /// still interrupts and Enter still refuses the unknown name.
    pub(crate) fn completions(&self) -> Option<Completions> {
        if self.panel().is_some() {
            return None;
        }
        if let Some(search) = self.search_panel() {
            return Some(search);
        }
        if self.slash_open() {
            let query = self.slash_query();
            let all: Vec<slash::Row> = slash::filter(&self.completion.slash_rows, &query)
                .into_iter()
                .cloned()
                .collect();
            return Some(Completions::slash(all, self.completion.selected, query));
        }
        // The `@` search wins over the theme names, as it does over the
        // `/` list: an `@` past a space closes the `/` panel.
        if self.completion.files.is_none() && self.theme_open() {
            let names = self.theme_names();
            if names.is_empty() {
                return None;
            }
            return Some(Completions::files(
                &names,
                self.completion.selected,
                self.theme_query(),
            ));
        }
        let query = self.file_query().unwrap_or_default();
        match self
            .completion
            .files
            .as_ref()
            .and_then(|panel| panel.result.as_ref())
        {
            Some(Ok(paths)) => Some(Completions::files(paths, self.completion.selected, query)),
            Some(Err(error)) => Some(Completions::message(format!("No files: {error}"), query)),
            None => None,
        }
    }

    /// Whether the `/` panel is open: the draft starts with `/`, holds no
    /// whitespace, and Esc has not closed the panel for it.
    fn slash_open(&self) -> bool {
        let text = self.draft.expand();
        !self.completion.slash_closed
            && text.starts_with('/')
            && !text.contains(char::is_whitespace)
    }

    /// The text after the `/`.
    fn slash_query(&self) -> String {
        self.draft.expand().get(1..).unwrap_or_default().to_owned()
    }

    /// Whether the theme-name panel is open: the draft starts with
    /// `/theme `, and Esc has not closed the panel for it.
    fn theme_open(&self) -> bool {
        !self.completion.theme_closed && self.draft.expand().starts_with("/theme ")
    }

    /// The text after the `/theme ` boundary.
    fn theme_query(&self) -> String {
        self.draft
            .expand()
            .get("/theme ".len()..)
            .unwrap_or_default()
            .to_owned()
    }

    /// The theme names the `/theme ` panel offers, filtered by prefix
    /// on the text after the space: the built-ins plus the seam's file
    /// themes. None while an extension owns `theme`, when the panel
    /// stays closed and the draft goes out as its command.
    fn theme_names(&self) -> Vec<String> {
        if self
            .completion
            .extensions
            .iter()
            .any(|name| name == "theme")
        {
            return Vec::new();
        }
        let mut names: Vec<String> = crate::settings_view::BUILT_IN_THEMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        if let (Some(seam), Some(workspace)) = (self.configure_seam(), self.workspace()) {
            names.extend(seam.themes(&workspace));
        }
        let query = self.theme_query().to_lowercase();
        names
            .into_iter()
            .filter(|name| name.to_lowercase().starts_with(&query))
            .collect()
    }

    /// The names of the `/` panel's rows, in order.
    fn slash_names(&self) -> Vec<String> {
        slash::filter(&self.completion.slash_rows, &self.slash_query())
            .into_iter()
            .map(|row| row.name.clone())
            .collect()
    }

    /// The paths the `@` panel offers, in order.
    pub(super) fn file_paths(&self) -> &[String] {
        match self
            .completion
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
        let anchor = self.completion.files.as_ref()?.anchor;
        self.draft.mention(anchor).map(|(query, _)| query)
    }

    /// Whether the `@` panel is open.
    pub(crate) fn files_open(&self) -> bool {
        self.completion.files.is_some()
    }

    /// A search result tagged `generation`: shown when it is the current
    /// generation and the `@` panel is open; otherwise it is stale and
    /// changes nothing.
    pub(crate) fn on_files(&mut self, generation: u64, result: Result<Vec<String>, String>) {
        if generation != self.completion.generation {
            return;
        }
        if let Some(panel) = &mut self.completion.files {
            let len = result.as_ref().map_or(0, Vec::len);
            self.completion.selected = self.completion.selected.min(len.saturating_sub(1));
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
            self.completion.slash_closed = false;
        }
        if !self.draft.expand().starts_with("/theme ") {
            self.completion.theme_closed = false;
        }
        if let Some(panel) = &self.completion.files
            && self.draft.mention(panel.anchor).is_none()
        {
            self.completion.files = None;
        }
    }

    /// Types one character at the cursor. An `@` at the draft's start or
    /// after whitespace opens the `@` panel.
    pub(super) fn type_char(&mut self, ch: char) -> Effect {
        let opens = ch == '@' && self.draft.after_space();
        self.draft.insert(ch);
        self.completion.selected = 0;
        if opens {
            self.completion.files = Some(FilePanel {
                anchor: self.draft.position().saturating_sub(1),
                result: None,
            });
            self.completion.generation = self.completion.generation.saturating_add(1);
            return self.search_files(String::new(), true);
        }
        self.query_changed()
    }

    /// Deletes the piece before the cursor.
    pub(super) fn backspace(&mut self) -> Effect {
        self.draft.backspace();
        self.completion.selected = 0;
        self.query_changed()
    }

    /// After an edit: a new search while the `@` panel stays open.
    pub(super) fn query_changed(&mut self) -> Effect {
        self.edited();
        let Some(query) = self.file_query() else {
            return Effect::None;
        };
        self.completion.generation = self.completion.generation.saturating_add(1);
        self.search_files(query, false)
    }

    /// Remote panels never start a client-side walk, including while detached.
    fn search_files(&mut self, query: String, fresh: bool) -> Effect {
        if let Some(remote) = self.remote() {
            // With no local workspace no hub search runs: there is no
            // directory to list.
            let Some(workspace) = self.workspace() else {
                return Effect::None;
            };
            if let Err(error) = remote.search(&workspace, &query, fresh, self.completion.generation)
            {
                self.on_files(self.completion.generation, Err(error.to_string()));
            }
            Effect::None
        } else if fresh {
            Effect::ListFiles
        } else {
            Effect::Search {
                generation: self.completion.generation,
                query,
            }
        }
    }

    /// Closes the open `/`, `@` and `/theme ` panels, as if Esc had
    /// closed each one: the draft is unchanged, a search panel hiding
    /// the draft is untouched, and the next typed `/` still opens its
    /// panel. A panel that is not open leaves no flag behind, so a
    /// flag once set stays set. Idempotent.
    pub(crate) fn close_completion_panels(&mut self) {
        // Both conditions are read before either flag is set: a `/`
        // panel counts only while no search panel hides it, and a
        // theme panel only with a name to show, as Esc sees them.
        let slash = self.slash_open() && self.search_panel().is_none();
        let theme = self.theme_open() && !self.theme_names().is_empty();
        self.completion.files = None;
        self.completion.slash_closed |= slash;
        self.completion.theme_closed |= theme;
    }

    /// A key for the open completion panel; `None` when no panel is open
    /// or the key is the input box's.
    pub(super) fn completion_key(&mut self, key: &Key) -> Option<Effect> {
        // One open panel at a time: the `/` list, the `@` search, or the
        // `/theme ` names, which the `@` search wins over. No panel
        // without a candidate: the keys fall through, so Esc still
        // interrupts and Enter still refuses the unknown name.
        let names = if self.slash_open() {
            self.slash_names()
        } else if self.completion.files.is_some() {
            self.file_paths().to_vec()
        } else if self.theme_open() {
            let names = self.theme_names();
            if names.is_empty() {
                return None;
            }
            names
        } else {
            return None;
        };
        let theme = !self.slash_open() && self.completion.files.is_none();
        let chosen = names.get(self.completion.selected).cloned();
        match key {
            Key::Up => self.completion.selected = self.completion.selected.saturating_sub(1),
            Key::Down => {
                self.completion.selected = self
                    .completion
                    .selected
                    .saturating_add(1)
                    .min(names.len().saturating_sub(1));
            }
            Key::Esc => {
                if theme {
                    self.completion.theme_closed = true;
                } else if self.completion.files.take().is_none() {
                    self.completion.slash_closed = true;
                }
            }
            Key::Tab | Key::Enter => {
                let chosen = chosen?;
                if theme {
                    // Acceptance replaces the whole rest, spaces included.
                    self.draft.set(&format!("/theme {chosen}"));
                    self.completion.selected = 0;
                    if *key == Key::Enter {
                        return Some(self.on_enter());
                    }
                } else if let Some(panel) = self.completion.files.take() {
                    if chosen.ends_with('/')
                        && let Some((_, end)) = self.draft.mention(panel.anchor)
                    {
                        // A directory row enters the new base and keeps
                        // searching it: the `@` stays and the text after
                        // it becomes the row, with no trailing space. The
                        // panel stays open on a newer generation, and the
                        // old rows go, so Enter never inserts a stale one.
                        self.draft
                            .replace(panel.anchor.saturating_add(1)..end, &chosen);
                        self.completion.files = Some(FilePanel {
                            anchor: panel.anchor,
                            result: None,
                        });
                        self.completion.selected = 0;
                        self.completion.generation = self.completion.generation.saturating_add(1);
                        let Some(query) = self.file_query() else {
                            return Some(Effect::None);
                        };
                        return Some(self.search_files(query, false));
                    }
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
            Key::CtrlV | Key::CtrlL => {}
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
}
