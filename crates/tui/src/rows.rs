//! The rows a card draws, each with how its text joins the row before
//! (`docs/tui.md`, "Selection and copy": a selection copies the text
//! unwrapped). Producers push into [`Rows`] as they would into a
//! `Vec<Row>`; a plain push is a row that starts a line of its own.

use std::ops::Deref;

use crate::turn::Row;

/// How a row's text follows the row before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Join {
    /// A new logical line.
    Break,
    /// The same line, with nothing between: a word broken at the edge.
    Wrap,
    /// The same line, with the one space the wrap dropped between.
    WrapSpace,
}

/// What a drawn row adds to its logical line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowText {
    /// How it joins the row before.
    pub(crate) join: Join,
    /// The leading cells that are decoration, not text: quote bars, a hang
    /// indent, a code gutter, the bubble's pad.
    pub(crate) skip: u16,
    /// The whole row is decoration and adds no text: a code block's header.
    pub(crate) decoration: bool,
}

impl RowText {
    /// A row that starts a line of its own, all of it text.
    pub(crate) fn plain() -> Self {
        Self {
            join: Join::Break,
            skip: 0,
            decoration: false,
        }
    }
}

/// The rows drawn so far and, one each, their texts.
#[derive(Debug, Default)]
pub(crate) struct Rows {
    rows: Vec<Row>,
    texts: Vec<RowText>,
}

impl Rows {
    /// Adds a row that starts a line of its own.
    pub(crate) fn push(&mut self, row: Row) {
        self.push_text(row, RowText::plain());
    }

    /// Adds a row with what it adds to its logical line.
    pub(crate) fn push_text(&mut self, row: Row, text: RowText) {
        self.rows.push(row);
        self.texts.push(text);
    }

    /// The rows and their texts, one each.
    pub(crate) fn into_parts(self) -> (Vec<Row>, Vec<RowText>) {
        (self.rows, self.texts)
    }
}

impl Deref for Rows {
    type Target = [Row];

    fn deref(&self) -> &[Row] {
        &self.rows
    }
}

impl Extend<Row> for Rows {
    fn extend<I: IntoIterator<Item = Row>>(&mut self, rows: I) {
        for row in rows {
            self.push(row);
        }
    }
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
