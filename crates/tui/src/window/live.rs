//! The live page's running turn (`docs/tui.md`, "The working line"): the
//! open turn and when its turn started, so the working line reads the
//! running summary's start across a page cut, never the open part's turn.

use crate::turn::Turn;

use super::Pages;

impl Pages {
    /// The live part's last turn, while it is open.
    pub(crate) fn open_turn(&self) -> Option<&Turn> {
        self.open.turns.last().filter(|turn| turn.is_open())
    }

    /// The running turn's start: the running summary's `started`, which a
    /// page cut never resets (`docs/tui.md`, "History and paging").
    pub(crate) fn running_started(&self) -> Option<u64> {
        let at = self.running()?;
        self.summaries.get(at).map(|summary| summary.started)
    }
}
