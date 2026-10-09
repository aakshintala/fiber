//! What each rendered row adds to its logical line (`docs/tui.md`,
//! "Selection and copy": a selection copies text unwrapped). The renderer
//! wraps text itself and drops the whitespace at a break, so it reports
//! here, row by row, how each row joins the one before and how many of its
//! leading cells are decoration.

use std::ops::Range;

use crate::rows::{Join, RowText};

/// The row texts of one render, one per line in the order the lines are
/// added.
#[derive(Debug, Default)]
pub(super) struct Track {
    texts: Vec<RowText>,
}

impl Track {
    /// The next row joins the one before by `join`, its first `skip` cells
    /// are not text, with `decoration` none of it is, and `links` are the
    /// link destinations drawn on it with their cells in the line
    /// (`docs/tui.md`, "Links": links are handled on click).
    pub(super) fn row(
        &mut self,
        join: Join,
        skip: u16,
        decoration: bool,
        links: Vec<(Range<u16>, String)>,
    ) {
        self.texts.push(RowText {
            join,
            skip,
            decoration,
            links,
            scopes: Vec::new(),
            spin: None,
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
