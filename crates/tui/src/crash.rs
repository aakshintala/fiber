//! Lines that can come outside any turn, and what the conversation shows
//! after a crash (`docs/tui.md`, "Errors and retries", "After a crash").
//!
//! An aside goes into the open turn's card where it came; with no turn
//! open it stands after the turns there were when it came. A child of
//! `turn`, so it places asides in a card.

use contract::Envelope;
use contract::events::McpServerFailed;
use ratatui::text::Line;

use super::{Entry, Fold, Turn, open};
use crate::app::{Target, read};
use crate::turn::Row;

/// A line outside a turn's own items.
#[derive(Debug)]
pub(crate) enum Aside {
    /// One line, as drawn.
    Line(Line<'static>),
}

impl Aside {
    /// Its lines.
    pub(crate) fn rows(&self, out: &mut Vec<Row>) {
        match self {
            Self::Line(line) => out.push((line.clone(), None)),
        }
    }

    /// Toggles what `target` opens; false when it is not this aside's.
    pub(crate) fn toggle(&mut self, target: Target) -> bool {
        match self {
            Self::Line(_) => target == Target::Login && false,
        }
    }
}

/// Places `aside` in the open turn, or after the turns there are.
pub(super) fn place(turns: &mut [Turn], fold: &mut Fold, aside: Aside) {
    match open(turns) {
        Some(turn) => turn.entries.push(Entry::Aside(aside)),
        None => fold.asides.push((turns.len(), aside)),
    }
}

/// Folds `mcp_server_failed`; false when it changed nothing.
pub(crate) fn fold(turns: &mut [Turn], fold: &mut Fold, envelope: &Envelope) -> bool {
    match envelope.kind.as_str() {
        "mcp_server_failed" => read!(envelope, McpServerFailed).is_some_and(|failed| {
            let line = Line::raw(format!("⚠ {}", failed.error.message));
            place(turns, fold, Aside::Line(line));
            true
        }),
        _ => false,
    }
}
