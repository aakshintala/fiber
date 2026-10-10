//! The open completion panel's structured rows: which match list shows,
//! the window on it and today's text form (`docs/tui.md`, "Rules").

use crate::slash;
use crate::slash::SHOWN;

/// Which match list the open completion panel shows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rows {
    /// The `/` window: at most [`SHOWN`] rows from `start` of the matches.
    Slash(Vec<slash::Row>),
    /// The `@` window of paths.
    Files(Vec<String>),
    /// The Ctrl+R panel's lines, query line first, as drawn.
    Search(Vec<String>),
    /// One dim row: "no matches", "no files match", "No files: <error>".
    Message(String),
}

/// The open completion panel's rows as drawn, and which is selected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Completions {
    /// The selected row among [`Completions::lines`], when one is selectable.
    pub(crate) selected: Option<usize>,
    /// The kind behind the panel: the window and its match counts.
    pub(crate) rows: Rows,
    /// The window's first row in the whole match list.
    pub(crate) start: usize,
    /// The whole match list's length; 0 for a message.
    pub(crate) total: usize,
    /// The text after the `/` or `@`.
    pub(crate) query: String,
}

impl Completions {
    /// The rows as drawn, today's text form, derived from the window.
    pub(crate) fn lines(&self) -> Vec<String> {
        match &self.rows {
            Rows::Slash(rows) => rows.iter().map(slash::Row::line).collect(),
            Rows::Files(rows) => rows.clone(),
            Rows::Search(lines) => lines.clone(),
            Rows::Message(text) => vec![text.clone()],
        }
    }

    /// One dim row with no selection and no range line.
    pub(crate) fn message(text: impl Into<String>, query: String) -> Self {
        let text = text.into();
        Self {
            selected: None,
            rows: Rows::Message(text),
            start: 0,
            total: 0,
            query,
        }
    }

    /// The `/` panel for `all`: its window around `selected`, or the
    /// "no matches" row when empty.
    pub(crate) fn slash(all: Vec<slash::Row>, selected: usize, query: String) -> Self {
        if all.is_empty() {
            return Self::message("no matches", query);
        }
        let total = all.len();
        let selected = selected.min(total.saturating_sub(1));
        let start = slash::window_start(selected);
        let rows: Vec<slash::Row> = all.into_iter().skip(start).take(SHOWN).collect();
        Self {
            selected: Some(selected.saturating_sub(start)),
            rows: Rows::Slash(rows),
            start,
            total,
            query,
        }
    }

    /// The `@` panel for `paths`: its window around `selected`, or the
    /// "no files match" row when empty.
    pub(crate) fn files(paths: &[String], selected: usize, query: String) -> Self {
        if paths.is_empty() {
            return Self::message("no files match", query);
        }
        let total = paths.len();
        let selected = selected.min(total.saturating_sub(1));
        let start = slash::window_start(selected);
        let rows: Vec<String> = paths.iter().skip(start).take(SHOWN).cloned().collect();
        Self {
            selected: Some(selected.saturating_sub(start)),
            rows: Rows::Files(rows),
            start,
            total,
            query,
        }
    }
}
