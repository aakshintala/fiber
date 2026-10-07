//! The steering queue above the input box, and its selection
//! (`docs/tui.md`, "Steering").
//!
//! The queue is the latest `steering_queue` for the attached session, so
//! every attached client sees and edits the same one. Selecting a row
//! stashes the draft and loads the row's text; clearing the selection puts
//! the stash back. A selection is held by the row's `steer` command id, so
//! a queue that reorders keeps it on the same message.
//!
//! A child of `app`, so the keys and commands that act on the queue sit
//! here beside it.

use contract::events::SteeringQueue;

use super::{App, Effect, Kind, Link, mint, session_command};
use crate::format;
use crate::input::Draft;
use crate::keys::Key;
use serde_json::json;

/// One queued message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Queued {
    text: String,
    /// The `steer` that sent it; `None` for Fiber's own message, which
    /// cannot be selected.
    command_id: Option<String>,
}

/// The queue, the selected row and the draft it stashed.
#[derive(Debug, Default)]
pub(crate) struct Steering {
    rows: Vec<Queued>,
    /// The selected row's command id.
    selected: Option<String>,
    /// The draft from before the selection.
    stash: Draft,
}

impl Steering {
    /// Replaces the queue with `queue`. A selected row that left it clears
    /// the selection, and the stash returns to `draft`.
    pub(crate) fn fold(&mut self, queue: &SteeringQueue, draft: &mut Draft) {
        self.rows = queue
            .messages
            .iter()
            .map(|message| Queued {
                text: crate::app::text_of(&message.content),
                command_id: message.sender.command_id.as_ref().map(|id| id.0.clone()),
            })
            .collect();
        if self
            .selected
            .as_ref()
            .is_some_and(|id| self.at(id).is_none())
        {
            self.clear(draft);
        }
    }

    /// Whether a row is selected.
    pub(crate) fn is_selected(&self) -> bool {
        self.selected.is_some()
    }

    /// The selectable rows' indices, oldest first.
    fn selectable(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.command_id.is_some())
            .map(|(at, _)| at)
    }

    /// Where the row sent by `id` is.
    fn at(&self, id: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.command_id.as_deref() == Some(id))
    }

    /// The selected row's index.
    fn selected_at(&self) -> Option<usize> {
        self.selected.as_deref().and_then(|id| self.at(id))
    }

    /// ⌥↑: with nothing selected the newest row, else the next older one,
    /// stopping at the oldest.
    pub(crate) fn up(&mut self, draft: &mut Draft) {
        let next = match self.selected_at() {
            None => self.selectable().next_back(),
            Some(now) => self.selectable().rev().find(|at| *at < now),
        };
        if let Some(at) = next {
            self.select(at, draft);
        }
    }

    /// ⌥↓: the next newer row; past the newest, the selection clears.
    pub(crate) fn down(&mut self, draft: &mut Draft) {
        let Some(now) = self.selected_at() else {
            return;
        };
        let next = self.selectable().find(|at| *at > now);
        match next {
            Some(at) => {
                self.select(at, draft);
            }
            None => self.clear(draft),
        }
    }

    /// Selects the row at `at` and loads its text into `draft`, stashing
    /// the draft when nothing was selected; false when the row cannot be
    /// selected.
    pub(crate) fn select(&mut self, at: usize, draft: &mut Draft) -> bool {
        let Some(row) = self.rows.get(at) else {
            return false;
        };
        let Some(id) = row.command_id.clone() else {
            return false;
        };
        let text = row.text.clone();
        if self.selected.is_none() {
            self.stash = std::mem::take(draft);
        }
        draft.set(&text);
        self.selected = Some(id);
        true
    }

    /// Clears the selection, the stash back in `draft`.
    pub(crate) fn clear(&mut self, draft: &mut Draft) {
        if self.selected.take().is_some() {
            *draft = std::mem::take(&mut self.stash);
        }
    }

    /// Enter with a row selected: the row's command id to drop before the
    /// edited text is sent, and the selection cleared without the stash.
    /// The stash is for the caller to restore after the send.
    pub(crate) fn amend(&mut self) -> Option<(String, Draft)> {
        let id = self.selected.take()?;
        Some((id, std::mem::take(&mut self.stash)))
    }

    /// The command ids ⌥X drops: the selected row's, or with none selected
    /// every row's that has one.
    pub(crate) fn to_drop(&self) -> Vec<String> {
        match &self.selected {
            Some(id) => vec![id.clone()],
            None => self
                .rows
                .iter()
                .filter_map(|row| row.command_id.clone())
                .collect(),
        }
    }

    /// The command id of the row at `at`, when it has one.
    pub(crate) fn id_at(&self, at: usize) -> Option<String> {
        self.rows.get(at).and_then(|row| row.command_id.clone())
    }

    /// One line per row at `width`, oldest first: "↳ <text>", the selected
    /// row "▸ <text>", each cut to the width.
    pub(crate) fn lines(&self, width: u16) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let mark = if row.command_id.is_some() && row.command_id == self.selected {
                    '▸'
                } else {
                    '↳'
                };
                let line = format!("{mark} {}", row.text.replace('\n', " "));
                format::cut(&line, usize::from(width))
            })
            .collect()
    }
}

impl App {
    /// ⌥↑, ⌥↓ and ⌥X: `select_steering` and `drop_steering`.
    pub(super) fn steering_key(&mut self, key: &Key) -> Effect {
        if *key == Key::AltX {
            let rows = self.steering.to_drop();
            return self.steer_drop(rows);
        }
        if *key == Key::AltUp {
            self.steering.up(&mut self.draft);
        } else {
            self.steering.down(&mut self.draft);
        }
        Effect::None
    }

    /// The steering queue's rows, oldest first.
    pub(crate) fn steering(&self) -> Vec<String> {
        self.steering.lines(self.width)
    }

    /// `select_steering` on the queued row at `index`.
    pub(crate) fn select_steering(&mut self, index: usize) {
        self.steering.select(index, &mut self.draft);
    }

    /// `drop_steering` on the queued row at `index`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no drawn drop target yet: docs/tui.md places none"
        )
    )]
    pub(crate) fn drop_steering(&mut self, index: usize) -> Effect {
        let row = self.steering.id_at(index);
        self.steer_drop(row.into_iter().collect())
    }

    /// Enter with a queued row selected: `steer_drop` for the row, then
    /// `steer` with the edited text; the draft from before the selection
    /// comes back.
    pub(super) fn amend(&mut self) -> Effect {
        let (Some(session), Link::Up) = (self.session().cloned(), self.link) else {
            return Effect::None;
        };
        let Some((row, stash)) = self.steering.amend() else {
            return Effect::None;
        };
        let Effect::Send(mut lines) = self.steer_drop(vec![row]) else {
            return Effect::None;
        };
        let id = mint();
        let text = std::mem::replace(&mut self.draft, stash).expand();
        let content = json!({"content": [{"type": "text", "text": text}]});
        lines.push(session_command(&id, "steer", &session, Some(content)).to_string());
        self.pending.insert(id, (Kind::Steer, text));
        Effect::Send(lines)
    }

    /// One `steer_drop` per command id in `rows`; nothing when there are
    /// none, or no connection to send them on.
    fn steer_drop(&mut self, rows: Vec<String>) -> Effect {
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        if rows.is_empty() || self.link != Link::Up {
            return Effect::None;
        }
        let lines = rows
            .into_iter()
            .map(|row| {
                let id = mint();
                let args = json!({ "command_id": row });
                let line = session_command(&id, "steer_drop", &session, Some(args));
                self.pending.insert(id, (Kind::SteerDrop, String::new()));
                line.to_string()
            })
            .collect();
        Effect::Send(lines)
    }
}

#[cfg(test)]
#[path = "steering_tests.rs"]
mod tests;
