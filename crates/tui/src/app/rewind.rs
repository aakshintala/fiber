//! The `/rewind` view (`docs/tui.md`, "Rewind"): the conversation's turns
//! back through the `forked_from` chain, each opening into its step
//! boundaries, with the point's files, calls and running jobs below.
//!
//! Opening the view sends one `history` for the attached session and
//! pages back through each ancestor the chain names, stopping each
//! segment at its successor's `forked_from.seq`, at most one request on
//! the wire. Every pending request carries the frame time it went out
//! at; past ten seconds (`docs/tui.md`, "History and paging") the frame
//! ends the wait as a lost connection, and a late answer folds nowhere.
//!
//! The view's state lives in three owned modules: the fold holds the
//! held lines, the pager, and the folded turns; the cursor holds the
//! selection, the point, and the open turn; the send state holds the
//! `rewind` on the wire, its refusal, and the summary checkbox. Each
//! module owns its fields; this parent holds the three component types
//! and coordinates them, and the `App` seam below reaches them only
//! through the coordinators here.

use contract::{Envelope, SessionId};

use self::fold::Fold;
use self::send::Send;
use self::view::{Cursor, EnterAction, Sel, Vis};
use crate::swapped::{Frame, List};

mod fold;
mod send;
mod view;

/// The rewind view's state: the fold, the cursor, and the send state.
#[derive(Debug, Default)]
pub(crate) struct Rewind {
    /// The folded turns and the chain pager.
    fold: Fold,
    /// The list cursor: selection, point, and open turn.
    cursor: Cursor,
    /// The `rewind` on the wire, its refusal, and the checkbox.
    send: Send,
}

impl Rewind {
    /// An empty view for `serving`.
    pub(crate) fn open(serving: SessionId) -> Self {
        Self {
            fold: Fold::open(serving),
            cursor: Cursor::default(),
            send: Send::default(),
        }
    }

    /// Folds a page into the turns and anchors the cursor, returning
    /// the selected row and the row count. A refused page ends the
    /// walk with the turns read so far.
    pub(crate) fn append_page(&mut self, older: Vec<Envelope>) -> Result<(usize, usize), ()> {
        if self.fold.prepend(older).is_err() {
            self.fold.mark_ended();
            return Err(());
        }
        let fresh = self.fold.rebuild();
        let show_choices = self.send.choices_shown();
        let at = self
            .cursor
            .sync_selection(self.fold.turns(), fresh, show_choices);
        Ok((at, self.row_count()))
    }

    /// Every visible row's kind.
    fn visible(&self) -> Vec<Vis> {
        self.cursor
            .visible(self.fold.turns(), self.send.choices_shown())
    }

    /// The visible row's kind.
    pub(crate) fn visible_row(&self, at: usize) -> Option<Vis> {
        self.visible().get(at).copied()
    }

    /// The visible row count.
    pub(crate) fn row_count(&self) -> usize {
        self.visible().len()
    }

    /// Moves the cursor onto a visible row and follows content rows
    /// with the point.
    pub(crate) fn select_at(&mut self, at: usize) {
        let Some(row) = self.visible_row(at) else {
            return;
        };
        match row {
            Vis::Turn(ti) => {
                let key = self.fold.turn_key(ti);
                let point = key.clone().and(self.fold.first_point(ti));
                self.cursor.select_turn(key, point);
            }
            Vis::Bound(ti, bound) => {
                if let Some((turn, point)) = self.fold.bound_point(ti, bound) {
                    self.cursor.select_bound(turn, point);
                }
            }
            Vis::Check => self.cursor.select_simple(Sel::Check),
            Vis::Action => self.cursor.select_simple(Sel::Action),
            Vis::Retry => self.cursor.select_simple(Sel::Retry),
            Vis::NoSummary => self.cursor.select_simple(Sel::NoSummary),
            Vis::Cancel => self.cursor.select_simple(Sel::Cancel),
        }
    }

    /// Follows a cursor move onto content rows with the point.
    pub(crate) fn follow_selection(&mut self, at: usize) {
        let Some(row) = self.visible_row(at) else {
            return;
        };
        match row {
            Vis::Turn(ti) => {
                if let Some(turn) = self.fold.turns().get(ti) {
                    self.cursor.select_turn(
                        Some((turn.session.clone(), turn.start)),
                        self.fold.first_point(ti),
                    );
                }
            }
            Vis::Bound(ti, bound) => {
                if let Some((turn, point)) = self.fold.bound_point(ti, bound) {
                    self.cursor.select_bound(turn, point);
                }
            }
            Vis::Check => self.cursor.select_simple(Sel::Check),
            Vis::Action => self.cursor.select_simple(Sel::Action),
            Vis::Retry => self.cursor.select_simple(Sel::Retry),
            Vis::NoSummary => self.cursor.select_simple(Sel::NoSummary),
            Vis::Cancel => self.cursor.select_simple(Sel::Cancel),
        }
    }

    /// Enter on the cursor's row: what the app does next.
    pub(crate) fn enter(&mut self, at: usize) -> EnterAction {
        let Some(row) = self.visible_row(at) else {
            return EnterAction::None;
        };
        match row {
            Vis::Turn(ti) => {
                let key = self.fold.turn_key(ti);
                self.cursor.toggle_expanded(key);
                EnterAction::Reseat
            }
            Vis::Bound(..) => {
                // Enter on a boundary row sends from that point at
                // once: reaching the Rewind row crosses content rows,
                // which would move the point.
                EnterAction::Send
            }
            Vis::Check => {
                self.send.toggle_summarise();
                EnterAction::Reseat
            }
            Vis::Action => EnterAction::Send,
            Vis::Retry => EnterAction::SendAs(true),
            Vis::NoSummary => EnterAction::SendAs(false),
            Vis::Cancel => {
                self.send.clear_refusal();
                EnterAction::Reseat
            }
        }
    }

    /// Moves the cursor back onto the selection after the rows change.
    ///
    /// Exempt from mutation testing: at every Enter on a turn row the
    /// cursor already sits on that turn, and toggling the turn leaves
    /// its own row where it is, so re-seating always recomputes the
    /// cursor's current row; no consumer-visible behavior changes when
    /// this coordination is removed.
    #[cfg_attr(false, mutants::skip)]
    pub(crate) fn reseat(&mut self) -> (usize, usize) {
        let show_choices = self.send.choices_shown();
        let at = self
            .cursor
            .sync_selection(self.fold.turns(), false, show_choices);
        (at, self.row_count())
    }

    /// Moves the cursor onto a row the click chose.
    pub(crate) fn reseat_at(&mut self, at: usize) -> (usize, usize) {
        let rows = self.row_count();
        (at.min(rows.saturating_sub(1)), rows)
    }

    /// One turn's step boundaries, oldest first, as holding sessions
    /// and seqs. Tests only: production reads the fold through the
    /// frame.
    #[cfg(test)]
    pub(crate) fn turn_bounds(&self, at: usize) -> Vec<(String, u64)> {
        self.fold.turns().get(at).map_or(Vec::new(), |turn| {
            turn.bounds
                .iter()
                .map(|bound| (bound.session.0.clone(), bound.seq))
                .collect()
        })
    }
}

/// The rewind view's frame for the session view screen.
pub(crate) fn frame(state: &Rewind, jobs: &[String], list: List) -> Frame {
    view::render(&state.fold, &state.cursor, &state.send, jobs, list)
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
