//! What an open holds over the attached screen while the session's
//! lines stream in: the preview the screen shows, then the second `Pages`
//! the pass folds into, swapped in at its end (`docs/tui.md`, "History
//! and paging").

use contract::{Envelope, Seq, SessionId};
use serde_json::{Value, json};

use super::{App, history_args};
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
/// far, oldest first and contiguous within each segment, ancestors
/// before the session (`docs/tui.md`, "History and paging").
#[derive(Debug, Default)]
pub(super) struct Tail {
    /// The lines read so far, oldest first.
    lines: Vec<Envelope>,
    /// Whether an answer held no line, or the held run reached its
    /// segment's first line: the current segment ended.
    start: bool,
    /// Whether the read ended: failed, abandoned, or lost.
    ended: bool,
    /// Whether the held run already followed its oldest segment's
    /// parent and the answer came back empty: the parent is unknown,
    /// so the walk stops instead of asking again.
    crossed: bool,
}

impl Tail {
    /// An empty tail: nothing read, the start not reached, the read open.
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Puts `older` before the held lines. Refuses a line carrying no
    /// seq, an answer whose own lines run out of order within itself,
    /// an answer that leaves a gap within the segment (its last seq is
    /// not one below the first held line), or a new segment across the
    /// seam that the held run does not name, that names another
    /// session, or that reaches above the named point.
    pub(super) fn prepend(&mut self, older: Vec<Envelope>) -> Result<(), ()> {
        if older.is_empty() {
            self.start = true;
            if self.forked_from().is_some() {
                self.crossed = true;
            }
            return Ok(());
        }
        if older.iter().any(|line| line.seq.is_none()) {
            return Err(());
        }
        // An answer's own lines run oldest first with no gap: a
        // descending, duplicate or gapped pair inside one window is as
        // out of order as a gap across windows.
        let ascending = older.windows(2).all(|pair| match pair {
            [previous, next] => match (previous.seq, next.seq) {
                (Some(previous), Some(next)) => previous.0.checked_add(1) == Some(next.0),
                _ => false,
            },
            _ => false,
        });
        if !ascending {
            return Err(());
        }
        let crossed_seam = match (self.lines.first(), older.last()) {
            (Some(first), Some(newest)) => first.session_id != newest.session_id,
            _ => false,
        };
        if crossed_seam {
            let Some(forked) = self.forked_from() else {
                return Err(());
            };
            let newest = older.last().and_then(|line| line.seq);
            if newest.is_some_and(|seq| seq > forked.seq)
                || older
                    .iter()
                    .any(|line| line.session_id != forked.session_id)
            {
                return Err(());
            }
            self.start = false;
            self.crossed = false;
        } else if let (Some(first), Some(last)) = (
            self.lines.first().and_then(|line| line.seq),
            older.last().and_then(|line| line.seq),
        ) && last.0.checked_add(1) != Some(first.0)
        {
            return Err(());
        }
        self.lines.splice(..0, older);
        if self.lines.first().and_then(|line| line.seq) == Some(Seq(0)) {
            self.start = true;
        }
        Ok(())
    }

    /// The parent the held run continues from, when it reached its
    /// segment's first line: the `forked_from` of the held
    /// `session_started`; `None` at the chain root or without one.
    /// Only the point parses, so a fuller line still crosses.
    fn forked_from(&self) -> Option<contract::shapes::Point> {
        let first = self.lines.first()?;
        if first.kind != "session_started" || first.seq != Some(Seq(0)) {
            return None;
        }
        first
            .payload
            .get("forked_from")
            .and_then(|forked| serde_json::from_value(forked.clone()).ok())
    }

    /// Whether the held lines span the seam: lines from more than one
    /// segment are held, so the read already crossed into an ancestor.
    /// An own-segment fill stops pre-frame reads when this is false,
    /// leaving older windows and ancestors to the post-frame count.
    pub(super) fn crossed_the_seam(&self) -> bool {
        let mut sessions = self.lines.iter().map(|line| &line.session_id);
        let Some(first) = sessions.next() else {
            return false;
        };
        sessions.any(|session| session != first)
    }

    /// The held lines the count phase folds: the leading run outside
    /// the opening session that stays strictly older than the installed
    /// candidate's oldest folded line, oldest first, with the line
    /// following it as the fold's trailer when that line is also older
    /// than the floor. Older own-segment lines stay for the pass; the
    /// trailer closes the fold below the ancestors, so the ancestor
    /// run carries above the preview. Lines at or after the floor
    /// already show, so folding them again would double-count the
    /// preview the install moved into the screen.
    pub(super) fn count_parts(
        &self,
        own: &SessionId,
        floor: Option<&(SessionId, Seq)>,
    ) -> (&[Envelope], Option<&Envelope>) {
        let len = self
            .lines
            .iter()
            .take_while(|line| line.session_id != *own)
            .count();
        let (prefix, trailer) = (self.lines.get(..len).unwrap_or(&[]), self.lines.get(len));
        let Some((floor_session, floor_seq)) = floor else {
            return (prefix, trailer);
        };
        let Some(at) = self
            .lines
            .iter()
            .position(|line| line.session_id == *floor_session && line.seq == Some(*floor_seq))
        else {
            return (prefix, trailer);
        };
        if at < len {
            // The floor sits inside the ancestor run: only lines strictly
            // before it still need folding, and the trailer sits at or
            // after the floor, so it already shows.
            return (self.lines.get(..at).unwrap_or(&[]), None);
        }
        // The floor sits at or after the trailer: the whole ancestor run
        // is older than the preview, and the trailer closes the count
        // fold even when it is the preview's oldest line.
        (prefix, trailer)
    }

    /// Whether the held run reached its segment's first line with a
    /// parent still to read: past a segment start the read keeps going
    /// for the joint total.
    pub(super) fn seam_pending(&self) -> bool {
        self.lines.first().and_then(|line| line.seq) == Some(Seq(0))
            && self.forked_from().filter(|_| !self.crossed).is_some()
    }

    /// Whether the walk reached the chain root: the read ended, or a
    /// segment ended with no onward parent.
    pub(super) fn chain_done(&self) -> bool {
        self.ended || (self.start && (self.crossed || self.forked_from().is_none()))
    }

    /// The args of the next read: `{}` first, `to_seq` just below the
    /// first held line, or the parent the held run continues from when
    /// it reached its segment's first line. Past the seam every window
    /// names its segment: the held run spans the crossing.
    pub(super) fn next_args(&self) -> Value {
        match self.lines.first().and_then(|line| line.seq) {
            Some(first) => {
                // The tip stays on the attached segment: the walk only
                // ever prepends the named parent, so the newest line's
                // session is the serving one.
                let Some(newest) = self.lines.last() else {
                    return json!({});
                };
                if first == Seq(0)
                    && let Some(forked) = self.forked_from().filter(|_| !self.crossed)
                {
                    return history_args(
                        &newest.session_id,
                        &forked.session_id,
                        None,
                        Some(forked.seq.0),
                    );
                }
                let Some(oldest) = self.lines.first() else {
                    return json!({});
                };
                history_args(
                    &newest.session_id,
                    &oldest.session_id,
                    None,
                    Some(first.0.saturating_sub(1)),
                )
            }
            None => json!({}),
        }
    }

    /// Whether the read reached its segment's start: the first held
    /// seq is zero, or an answer held no line. Tests only: production
    /// walks past the segment's start into each ancestor.
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
    /// The open stops the read here on its own segment; past a seam it
    /// keeps walking for the joint total (`seam_pending`).
    /// Whether the held run fills the display: the read reached its
    /// segment's start or end, or the run's rows reach the height.
    /// Tests only: production stops pre-frame reads on rows alone, so a
    /// run that reaches `seq 0` short of the display still crosses.
    #[cfg(test)]
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

    /// Whether the read ended. Tests only: production reads it through
    /// `chain_done`.
    #[cfg(test)]
    pub(super) fn ended(&self) -> bool {
        self.ended
    }
}

/// The visible run folded once: the pages the install moves into the
/// screen, with the last busy the fold reported. The install moves
/// this object instead of refolding the run, and the count phase never
/// touches it again.
pub(super) struct Preview {
    /// The folded pages.
    pub(super) pages: Pages,
    /// The last busy the fold reported.
    pub(super) busy: Option<bool>,
}

impl App {
    /// Ends the overlay on every path: the fold drops, a landed pass can
    /// no longer fire, and the screen unfreezes, applying one stored
    /// resize. The cached preview and any retained count go with it, so
    /// no later open installs or counts the old run.
    pub(super) fn end_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.preview = None;
        self.count = None;
        self.installed_floor = None;
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
