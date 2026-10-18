//! What an open holds over the attached screen while the session's
//! lines stream in: the preview the screen shows, then the second `Pages`
//! the pass folds into, swapped in at its end (`docs/tui.md`, "History
//! and paging").

use contract::{Envelope, Seq, SessionId};
use serde_json::{Value, json};

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
        /// Whether the open kept the screen's rows for a reconnect.
        held: bool,
    },
    /// The pass folds the session's lines into the second `Pages`,
    /// boxed: the fold dwarfs the other arms.
    Folding {
        session: SessionId,
        fold: Box<Pages>,
        busy: Option<bool>,
        /// Whether the open kept the screen's rows for a reconnect.
        held: bool,
        /// The pass reached its boundary; the next line lands it.
        landing: bool,
    },
    /// The pass ended abnormally; lines at or below `last` fold nowhere.
    Kept { last: Option<Seq> },
    /// The raise waits on the tail read: the read before it holds the
    /// durable lines read so far.
    Reading(Tail),
}

/// What the read before the raise holds: the durable lines read so
/// far, oldest first and contiguous (`docs/tui.md`, "History and
/// paging").
#[derive(Debug, Default)]
pub(super) struct Tail {
    /// The lines read so far, oldest first.
    lines: Vec<Envelope>,
    /// Whether an answer held no line: the read reached the start.
    start: bool,
    /// Whether the read ended: failed, abandoned, or lost.
    ended: bool,
}

impl Tail {
    /// An empty tail: nothing read, the start not reached, the read open.
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Puts `older` before the held lines. Refuses a line carrying no
    /// seq, or an answer that leaves a gap: its last seq is not one
    /// below the first held line.
    pub(super) fn prepend(&mut self, older: Vec<Envelope>) -> Result<(), ()> {
        if older.is_empty() {
            self.start = true;
            return Ok(());
        }
        if older.iter().any(|line| line.seq.is_none()) {
            return Err(());
        }
        if let (Some(first), Some(last)) = (
            self.lines.first().and_then(|line| line.seq),
            older.last().and_then(|line| line.seq),
        ) && last.0.saturating_add(1) != first.0
        {
            return Err(());
        }
        self.lines.splice(..0, older);
        if self.lines.first().and_then(|line| line.seq) == Some(Seq(0)) {
            self.start = true;
        }
        Ok(())
    }

    /// The args of the next read: `{}` first, then `to_seq` just below
    /// the first held line.
    pub(super) fn next_args(&self) -> Value {
        match self.lines.first().and_then(|line| line.seq) {
            Some(first) => json!({"to_seq": first.0.saturating_sub(1)}),
            None => json!({}),
        }
    }

    /// Whether the read reached the log's start: the first held seq is
    /// zero, or an answer held no line. Tests only: production stops
    /// through `filled`.
    #[cfg(test)]
    pub(super) fn start_reached(&self) -> bool {
        self.start
    }

    /// The preview's lines: the held lines from the earliest turn
    /// start, never cut inside a turn; the whole held run when its
    /// first seq is zero; none when neither holds.
    pub(super) fn turn_lines(&self) -> Option<&[Envelope]> {
        if self.lines.first().and_then(|line| line.seq) == Some(Seq(0)) {
            return Some(&self.lines);
        }
        self.lines
            .iter()
            .position(|line| line.kind == "turn_started")
            .and_then(|at| self.lines.get(at..))
    }

    /// Whether the tail is filled: the start reached, the read ended,
    /// or the turn-start lines folded fresh report at least `height`
    /// rows. Lines cut before the first `turn_started` never count.
    pub(super) fn filled(&self, fresh: &Pages, height: usize) -> bool {
        self.start
            || self.ended
            || self
                .turn_lines()
                .is_some_and(|_| fresh.index().total() >= height)
    }

    /// Ends the read: failed, abandoned, or lost.
    pub(super) fn end(&mut self) {
        self.ended = true;
    }

    /// Whether the read ended.
    pub(super) fn ended(&self) -> bool {
        self.ended
    }
}

impl App {
    /// Ends the overlay on every path: the fold drops, a landed pass can
    /// no longer fire, and the screen unfreezes, applying one stored
    /// resize.
    pub(super) fn end_overlay(&mut self) {
        self.overlay = Overlay::None;
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
            Overlay::None
            | Overlay::Shown { .. }
            | Overlay::Folding { .. }
            | Overlay::Reading(..) => false,
        };
        kept && self.session().is_some_and(|attached| attached == session)
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
