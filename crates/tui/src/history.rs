//! Prompt recall and search: ↑ and ↓ in the input box, and the Ctrl+R panel
//! (`docs/tui.md`, "The input box"). The entries are this session's prompts
//! as the terminal saw them, then the project's prompt history read through
//! the hub's `prompt_history`, page by page (`docs/state.md`, "Prompt
//! history").

use std::collections::HashSet;

use contract::SessionId;
use contract::events::InputItem;
use contract::shapes::{ContentPart, Origin};
use serde_json::{Value, json};

use super::commands::Completions;
use super::{App, Effect, Link, mint};
use crate::keys::{Edit, Key};
use crate::slash::{SHOWN, window_start};

/// The search panel's first line, before the query.
const SEARCH: &str = "search prompts: ";

/// What the search panel shows when nothing matches.
const NO_MATCH: &str = "no matching prompts";

/// What covers the input box or stands in for its draft: the approval
/// panel, the key map, a completion or search panel, the notice overlay,
/// and a selected steering row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cover {
    approval: bool,
    keymap: bool,
    completions: bool,
    notices: bool,
    steering: bool,
}

/// The Ctrl+R panel's state.
#[derive(Debug, Default)]
struct Search {
    /// The text typed into the panel.
    query: String,
    /// The selected match.
    selected: usize,
}

/// The recall list's sources, the paging state, browsing and search.
#[derive(Debug, Default)]
pub(super) struct History {
    /// The project key `prompt_history` names.
    project: String,
    /// Prompts the terminal saw a session take, oldest first: the session
    /// and the prompt's text.
    own: Vec<(SessionId, String)>,
    /// The hub's prompt history lines, newest first: the session that took
    /// each and its text.
    hub: Vec<(String, String)>,
    /// The `prompt_history` command waiting for its answer.
    pending: Option<String>,
    /// Whether the first page was asked for.
    started: bool,
    /// Where the next page ends, when older lines remain.
    before: Option<u64>,
    /// A rejection ended paging for this terminal run.
    ended: bool,
    /// The entry ↑ or ↓ put in the draft: its index and its text.
    browse: Option<(usize, String)>,
    /// An ↑ reached the end of what is loaded and waits for the next page.
    waiting: bool,
    /// What covered the input box once that ↑ was handled; `None` until
    /// [`App::settle`] first looks.
    cover: Option<Cover>,
    /// The Ctrl+R panel, while open.
    search: Option<Search>,
}

impl History {
    /// Sets the project key.
    pub(super) fn set_project(&mut self, project: String) {
        self.project = project;
    }

    /// Records the prompts `session` took in one `turn_started`: messages a
    /// client sent, never an extension's, another session's or Fiber's.
    pub(super) fn saw(&mut self, session: &SessionId, input: &[InputItem]) {
        for item in input {
            if let InputItem::Message {
                content, sender, ..
            } = item
                && sender.origin == Origin::Driver
            {
                self.own.push((session.clone(), text(content)));
            }
        }
    }

    /// The recall list with `session` attached: its prompts newest first,
    /// then the hub's lines from other sessions newest first. No empty
    /// entry, and no text twice.
    fn entries(&self, session: Option<&SessionId>) -> Vec<String> {
        let own = self
            .own
            .iter()
            .rev()
            .filter(|(from, _)| Some(from) == session)
            .map(|(_, text)| text);
        let hub = self
            .hub
            .iter()
            .filter(|(from, _)| session.is_none_or(|attached| attached.0 != *from))
            .map(|(_, text)| text);
        let mut seen = HashSet::new();
        own.chain(hub)
            .filter(|text| !text.is_empty() && seen.insert(text.as_str()))
            .cloned()
            .collect()
    }

    /// The search panel's matches: entries holding the query, ignoring
    /// case, in recall order.
    fn matches(&self, session: Option<&SessionId>) -> Vec<String> {
        let Some(search) = &self.search else {
            return Vec::new();
        };
        let query = search.query.to_lowercase();
        self.entries(session)
            .into_iter()
            .filter(|entry| entry.to_lowercase().contains(&query))
            .collect()
    }

    /// The next `prompt_history` line, when one may go out: the link is up,
    /// none is waiting, paging has not ended, and this is the first page or
    /// the last answer said older lines remain.
    fn fetch(&mut self, up: bool) -> Option<String> {
        let more = !self.started || self.before.is_some();
        if !up || self.pending.is_some() || self.ended || !more {
            return None;
        }
        let id = mint();
        let mut args = json!({ "project": self.project });
        if let (Some(before), Some(object)) = (self.before, args.as_object_mut()) {
            object.insert("before".to_owned(), json!(before));
        }
        let line = json!({"id": id, "command": "prompt_history", "args": args}).to_string();
        self.pending = Some(id);
        self.started = true;
        Some(line)
    }

    /// Whether the draft, as `text`, is still the entry recall put there.
    fn browsing(&self, text: &str) -> bool {
        self.browse.as_ref().is_some_and(|(_, shown)| shown == text)
    }

    /// Keeps browsing in step with the draft, as `text`: an edit ends
    /// browsing, and any text typed ends the wait for a page.
    pub(super) fn sync(&mut self, text: &str) {
        if self.browse.is_some() && !self.browsing(text) {
            self.browse = None;
            self.cancel();
        }
        if self.browse.is_none() && !text.is_empty() {
            self.cancel();
        }
    }

    /// Ends the wait for a page: the page still joins the list when it
    /// comes, and shows nothing.
    pub(super) fn cancel(&mut self) {
        self.waiting = false;
        self.cover = None;
    }

    /// The `prompt_history` answer for `id`: its lines join the list.
    /// `false` when `id` is not the command waiting.
    fn answered(&mut self, id: &str, result: Option<&Value>) -> bool {
        if self.pending.as_deref() != Some(id) {
            return false;
        }
        self.pending = None;
        let prompts = result
            .and_then(|result| result.get("prompts"))
            .and_then(Value::as_array);
        for line in prompts.into_iter().flatten() {
            let session = line.get("session_id").and_then(Value::as_str);
            let content = line
                .get("content")
                .cloned()
                .and_then(|content| serde_json::from_value::<Vec<ContentPart>>(content).ok());
            if let (Some(session), Some(content)) = (session, content) {
                self.hub.push((session.to_owned(), text(&content)));
            }
        }
        self.before = result
            .and_then(|result| result.get("before"))
            .and_then(Value::as_u64);
        true
    }
}

impl App {
    /// Sets the project key `prompt_history` names.
    pub(crate) fn set_project(&mut self, project: String) {
        self.history.set_project(project);
    }

    /// ↑ and ↓ for the input box: by wrapped row within the draft, then
    /// through earlier prompts. `None` for any other key.
    pub(super) fn recall_key(&mut self, key: &Key) -> Option<Effect> {
        let browsing = self.history.browsing(&self.draft.expand());
        let effect = match key {
            Key::Up => {
                if self.draft.is_empty() || (!self.draft.up(self.column_width()) && browsing) {
                    self.older()
                } else {
                    Effect::None
                }
            }
            Key::Down => {
                if !self.draft.down(self.column_width()) && browsing {
                    self.newer()
                } else {
                    Effect::None
                }
            }
            Key::Char(_)
            | Key::Backspace
            | Key::Enter
            | Key::Esc
            | Key::CtrlC
            | Key::CtrlO
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlF
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => return None,
        };
        Some(effect)
    }

    /// A key for recall ahead of the completion panels: the search panel's,
    /// while it is open, and ↑ ↓ while browsing, so a recalled `/` entry
    /// browses on. `None` when neither takes it.
    pub(super) fn history_key(&mut self, key: &Key) -> Option<Effect> {
        if self.history.search.is_none() {
            let browsing = self.history.browsing(&self.draft.expand());
            return if browsing && matches!(key, Key::Up | Key::Down) {
                self.recall_key(key)
            } else {
                None
            };
        }
        let matches = self.history.matches(self.session());
        let last = matches.len().saturating_sub(1);
        let search = self.history.search.as_mut()?;
        match key {
            Key::Char(ch) => {
                search.query.push(*ch);
                search.selected = 0;
            }
            Key::Backspace => {
                search.query.pop();
                search.selected = 0;
            }
            Key::Up => search.selected = search.selected.saturating_sub(1),
            Key::Down | Key::CtrlR => search.selected = search.selected.saturating_add(1).min(last),
            Key::Enter => {
                if let Some(entry) = matches.get(search.selected) {
                    self.draft.set(entry);
                }
                self.history.search = None;
            }
            Key::Esc => self.history.search = None,
            // Keys that would change the draft behind the panel do
            // nothing while it is open.
            Key::Tab | Key::BackTab | Key::CtrlG | Key::AltUp | Key::AltDown | Key::AltX => {}
            Key::CtrlC
            | Key::CtrlO
            | Key::CtrlF
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::F1 => return None,
        }
        Some(self.more())
    }

    /// An editing key while the search panel is open: a paste joins the
    /// query, its line breaks as spaces; other editing keys do nothing.
    /// `false` while the panel is closed.
    pub(super) fn search_edit(&mut self, edit: &Edit) -> bool {
        let Some(search) = &mut self.history.search else {
            return false;
        };
        if let Edit::Paste(text) = edit {
            search.query.extend(
                text.chars()
                    .map(|ch| if ch.is_control() { ' ' } else { ch }),
            );
            search.selected = 0;
        }
        true
    }

    /// Ctrl+R: opens the search panel, and asks for the first page.
    pub(super) fn open_search(&mut self) -> Effect {
        self.history.search = Some(Search::default());
        self.more()
    }

    /// The next page while the search panel is open, one at a time until
    /// the history ends.
    fn more(&mut self) -> Effect {
        let up = self.link == Link::Up;
        let line = self
            .history
            .search
            .is_some()
            .then(|| self.history.fetch(up))
            .flatten();
        send(line.into_iter().collect())
    }

    /// The search panel, drawn as the `/` panel is: its query line, then
    /// the matches in a window that keeps the selection in view.
    pub(super) fn search_panel(&self) -> Option<Completions> {
        let search = self.history.search.as_ref()?;
        let mut lines = vec![format!("{SEARCH}{}", search.query)];
        let matches = self.history.matches(self.session());
        if matches.is_empty() {
            lines.push(NO_MATCH.to_owned());
            return Some(Completions {
                lines,
                selected: None,
            });
        }
        let selected = search.selected.min(matches.len().saturating_sub(1));
        let start = window_start(selected);
        lines.extend(
            matches
                .into_iter()
                .skip(start)
                .take(SHOWN)
                .map(|entry| entry.replace('\n', " ")),
        );
        let row = selected.saturating_sub(start).saturating_add(1);
        Some(Completions {
            lines,
            selected: Some(row),
        })
    }

    /// Shows the next older entry, cursor at its end. The first recall asks
    /// for the first page; at the end of what is loaded the next page is
    /// asked for and the recall waits for it. With no more pages nothing
    /// changes.
    fn older(&mut self) -> Effect {
        let up = self.link == Link::Up;
        let mut lines = Vec::new();
        if !self.history.started {
            lines.extend(self.history.fetch(up));
        }
        let next = self.history.browse.as_ref().map_or(0, |(at, _)| at + 1);
        let entries = self.history.entries(self.session());
        if let Some(entry) = entries.get(next) {
            self.draft.set(entry);
            self.history.browse = Some((next, entry.clone()));
        } else {
            lines.extend(self.history.fetch(up));
            self.history.waiting = self.history.pending.is_some();
            self.history.cover = None;
        }
        send(lines)
    }

    /// Shows the next newer entry; past the newest, the empty draft.
    fn newer(&mut self) -> Effect {
        match self.history.browse.take() {
            Some((0, _)) | None => self.draft.clear(),
            Some((at, _)) => {
                let entries = self.history.entries(self.session());
                let at = at.saturating_sub(1);
                if let Some(entry) = entries.get(at) {
                    self.draft.set(entry);
                    self.history.browse = Some((at, entry.clone()));
                } else {
                    self.draft.clear();
                }
            }
        }
        Effect::None
    }

    /// A `command_accepted` for `id`: when it answers `prompt_history`, its
    /// lines join the list, a waiting recall shows the next entry while
    /// what covers the input box is as it was after the ↑, and an open
    /// search panel asks for the next page. `None` for another command.
    pub(super) fn history_answered(
        &mut self,
        id: &str,
        result: Option<&Value>,
    ) -> Option<Vec<String>> {
        if !self.history.answered(id, result) {
            return None;
        }
        let mut lines = Vec::new();
        let unchanged = self.history.cover.is_none_or(|cover| cover == self.cover());
        if std::mem::take(&mut self.history.waiting)
            && unchanged
            && let Effect::Send(more) = self.older()
        {
            lines.extend(more);
        }
        if let Effect::Send(more) = self.more() {
            lines.extend(more);
        }
        Some(lines)
    }

    /// What covers the input box now.
    fn cover(&self) -> Cover {
        Cover {
            approval: self.panel().is_some(),
            keymap: self.keymap_top().is_some(),
            completions: self.completions().is_some(),
            notices: self.notice_overlay().is_some(),
            steering: self.steering.is_selected(),
        }
    }

    /// Runs after every input the app handles: a key, an edit, a click, a
    /// line from the hub, a failed write or connection, the editor's or a
    /// file search's return. The first time after an ↑ starts waiting for
    /// a page it notes what covers the input box; once that differs,
    /// whatever opened or closed a panel, the recall waits no more.
    pub(super) fn settle(&mut self) {
        self.relayout();
        let height = self.conversation_height();
        self.screen.settle(height);
        self.settle_pending_turn();
        if !self.history.waiting {
            return;
        }
        let now = self.cover();
        match self.history.cover {
            None => self.history.cover = Some(now),
            Some(cover) if cover != now => self.history.cancel(),
            Some(_) => {}
        }
    }

    /// A `command_rejected` for `id`: when it rejects `prompt_history`, its
    /// message is the notice and paging ends for this terminal run.
    /// `false` for another command.
    pub(super) fn history_rejected(&mut self, id: &str, message: &str) -> bool {
        if self.history.pending.as_deref() != Some(id) {
            return false;
        }
        self.history.pending = None;
        self.history.ended = true;
        self.history.cancel();
        self.notices.push(message.to_owned());
        true
    }
}

/// An effect sending `lines`, or none when there are none.
fn send(lines: Vec<String>) -> Effect {
    if lines.is_empty() {
        Effect::None
    } else {
        Effect::Send(lines)
    }
}

/// An entry's text: the content's text parts joined with line breaks.
fn text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
