//! The rows a card draws, each with how its text joins the row before
//! (`docs/tui.md`, "Selection and copy": a selection copies the text
//! unwrapped). Producers push into [`Rows`] as they would into a
//! `Vec<Row>`; a plain push is a row that starts a line of its own.

use std::ops::{Deref, Range};

use crate::app::Target;
use crate::surface::{Edges, edge_row};
use crate::theme::Role;
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
    /// The trailing cells that are decoration, not text: the bubble's
    /// right pad and stripe.
    pub(crate) tail: u16,
    /// The whole row is decoration and adds no text: a code block's header.
    pub(crate) decoration: bool,
    /// The links drawn on the row: each one's cells in the line and its
    /// destination (`docs/tui.md`, "Links": links are handled on click).
    pub(crate) links: Vec<(Range<u16>, String)>,
    /// The collapsible sections the row is inside, outermost first: what
    /// a search draw opens, and what a match names (`docs/tui.md`,
    /// "Search").
    pub(crate) scopes: Vec<Target>,
    /// The column of the one cell that spins while its line runs, on the
    /// working line's tick (`docs/tui.md`, "The working line"); none in
    /// [`RowText::plain`].
    pub(crate) spin: Option<u16>,
}

impl RowText {
    /// A row that starts a line of its own, all of it text.
    pub(crate) fn plain() -> Self {
        Self {
            join: Join::Break,
            skip: 0,
            tail: 0,
            decoration: false,
            links: Vec::new(),
            scopes: Vec::new(),
            spin: None,
        }
    }
}

/// The rows drawn so far and, one each, their texts.
#[derive(Debug, Default)]
pub(crate) struct Rows {
    rows: Vec<Row>,
    texts: Vec<RowText>,
    /// The collapsible sections the next row lands inside, outermost
    /// first.
    scopes: Vec<Target>,
    /// A search draw: every collapsible body draws, open or not
    /// (`docs/tui.md`, "Search": every match is counted at once).
    all_open: bool,
}

impl Rows {
    /// Rows for a search draw: every collapsible body draws.
    pub(crate) fn all_open() -> Self {
        Self {
            all_open: true,
            ..Self::default()
        }
    }

    /// Enters what `target` opens: a collapsible body draws when `open`
    /// on a normal draw, and always on a search draw. Every row pushed
    /// until [`Rows::end_scope`] names `target` among its scopes,
    /// outermost first. A body with no target draws as it would closed:
    /// nothing could reveal a match there.
    pub(crate) fn open_scope(&mut self, target: Target, open: bool) -> bool {
        self.scopes.push(target);
        open || self.all_open
    }

    /// Leaves the innermost open section; nothing without one.
    pub(crate) fn end_scope(&mut self) {
        self.scopes.pop();
    }

    /// Adds a row that starts a line of its own.
    pub(crate) fn push(&mut self, row: Row) {
        self.push_text(row, RowText::plain());
    }

    /// Adds a row with what it adds to its logical line.
    pub(crate) fn push_text(&mut self, row: Row, mut text: RowText) {
        text.scopes = self.scopes.clone();
        self.rows.push(row);
        self.texts.push(text);
    }

    /// The rows and their texts, one each.
    pub(crate) fn into_parts(self) -> (Vec<Row>, Vec<RowText>) {
        (self.rows, self.texts)
    }

    /// The rows from `from` on sit on `tint`: each takes it as its line's
    /// background; with `edges`, an `edge_row` (`width` cells,
    /// decoration) is inserted at `from` and pushed after the last. No
    /// rows from `from`: nothing (`docs/tui.md`, "Look").
    pub(crate) fn on_surface(&mut self, from: usize, width: u16, tint: Role, edges: Edges) {
        let Some(body) = self.rows.get_mut(from..) else {
            return;
        };
        if body.is_empty() {
            return;
        }
        for (line, _) in body {
            line.style.bg = Some(tint.color());
        }
        let width = usize::from(width);
        if edges.bottom {
            self.rows.push((edge_row(width, tint, false), None));
            self.texts.push(RowText {
                decoration: true,
                scopes: self.scopes.clone(),
                ..RowText::plain()
            });
        }
        if edges.top {
            self.rows.insert(from, (edge_row(width, tint, true), None));
            self.texts.insert(
                from,
                RowText {
                    decoration: true,
                    scopes: self.scopes.clone(),
                    ..RowText::plain()
                },
            );
        }
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
