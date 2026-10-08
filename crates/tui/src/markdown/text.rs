//! What each rendered row adds to its logical line (`docs/tui.md`,
//! "Selection and copy": a selection copies text unwrapped). The renderer
//! wraps text itself and drops the whitespace at a break, so it reports
//! here, row by row, how each row joins the one before and how many of its
//! leading cells are decoration.

use crate::rows::{Join, RowText};

/// The row texts of one render, one per line in the order the lines are
/// added.
#[derive(Debug, Default)]
pub(super) struct Track {
    texts: Vec<RowText>,
}

impl Track {
    /// The next row joins the one before by `join`, its first `skip` cells
    /// are not text, and with `decoration` none of it is.
    pub(super) fn row(&mut self, join: Join, skip: u16, decoration: bool) {
        self.texts.push(RowText {
            join,
            skip,
            decoration,
        });
    }

    /// The row texts, one per line.
    pub(super) fn finish(self) -> Vec<RowText> {
        self.texts
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
