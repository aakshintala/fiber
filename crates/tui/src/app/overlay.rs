//! What an open holds over the attached screen while the session's
//! lines stream in: the preview the screen shows, then the second `Pages`
//! the pass folds into, swapped in at its end (`docs/tui.md`, "History
//! and paging").

use contract::{Seq, SessionId};

use super::App;
use crate::window::Pages;

/// What the screen holds while an open is in flight: nothing once the
/// open ends or fails, the preview while its raise is out, the preview
/// with the pass folding into a second `Pages`, and the preview alone
/// when the pass ended abnormally, skipping the replay up to `last`.
#[derive(Debug, Default)]
pub(super) enum Overlay {
    /// No open holds the screen.
    #[default]
    None,
    /// The raise is in flight and the screen holds the preview.
    Shown {
        session: SessionId,
        busy: Option<bool>,
    },
    /// The pass folds the session's lines into the second `Pages`,
    /// boxed: the fold dwarfs the other arms.
    Folding {
        session: SessionId,
        fold: Box<Pages>,
        busy: Option<bool>,
    },
    /// The pass ended abnormally; lines at or below `last` fold nowhere.
    Kept { last: Option<Seq> },
}

impl App {
    /// Ends the overlay on every path: the fold drops, a landed pass can
    /// no longer fire, and the screen unfreezes, applying one stored
    /// resize.
    pub(super) fn end_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.landing = false;
        self.attached_screen_mut().set_frozen(false);
    }

    /// Whether the overlay folds the session's lines into a second
    /// `Pages`.
    pub(super) fn folding(&self, session: &SessionId) -> bool {
        matches!(&self.overlay, Overlay::Folding { session: folding, .. } if folding == session)
    }

    /// Whether the overlay skips the line: the kept preview already
    /// folded it.
    pub(super) fn kept_skips(&self, session: &SessionId, seq: Option<Seq>) -> bool {
        let Some(seq) = seq else {
            return false;
        };
        let kept = match &self.overlay {
            Overlay::Kept { last } => last.is_some_and(|last| seq <= last),
            Overlay::None | Overlay::Shown { .. } | Overlay::Folding { .. } => false,
        };
        kept && self.session().is_some_and(|attached| attached == session)
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
