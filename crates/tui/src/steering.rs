//! The steering queue above the input box, and its selection
//! (`docs/tui.md`, "Steering").
//!
//! The queue is the latest `steering_queue` for the attached session, so
//! every attached client sees and edits the same one. Selecting a row
//! stashes the draft and loads the row's text; clearing the selection puts
//! the stash back. A selection is held by the row's `steer` command id, so
//! a queue that reorders keeps it on the same message.

use contract::events::SteeringQueue;

use crate::format;

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
    stash: String,
}

impl Steering {
    /// Replaces the queue with `queue`. A selected row that left it clears
    /// the selection, and the stash returns to `draft`.
    pub(crate) fn fold(&mut self, queue: &SteeringQueue, draft: &mut String) {
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
    pub(crate) fn up(&mut self, draft: &mut String) {
        let next = match self.selected_at() {
            None => self.selectable().next_back(),
            Some(now) => self.selectable().rev().find(|at| *at < now),
        };
        if let Some(at) = next {
            self.select(at, draft);
        }
    }

    /// ⌥↓: the next newer row; past the newest, the selection clears.
    pub(crate) fn down(&mut self, draft: &mut String) {
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
    pub(crate) fn select(&mut self, at: usize, draft: &mut String) -> bool {
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
        *draft = text;
        self.selected = Some(id);
        true
    }

    /// Clears the selection, the stash back in `draft`.
    pub(crate) fn clear(&mut self, draft: &mut String) {
        if self.selected.take().is_some() {
            *draft = std::mem::take(&mut self.stash);
        }
    }

    /// Enter with a row selected: the row's command id to drop before the
    /// edited text is sent, and the selection cleared without the stash.
    /// The stash is for the caller to restore after the send.
    pub(crate) fn amend(&mut self) -> Option<(String, String)> {
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

#[cfg(test)]
#[path = "steering_tests.rs"]
mod tests;
