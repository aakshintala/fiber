//! The screen's geometry: its size, the input box's and the conversation's
//! rows, scrolling, and the pages' window over the session's history
//! (`docs/tui.md`, "History and paging").

use std::ops::RangeInclusive;

use contract::{Envelope, Seq};

use super::{App, Target};
#[cfg(test)]
use crate::turn::Row;
use crate::window::Pages;

impl App {
    /// Sets the screen size for wrapping and paging; a new width re-counts
    /// every page.
    pub(crate) fn set_size(&mut self, width: u16, height: u16) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.pages.set_width(self.width);
        self.settle();
    }

    /// The input box's rows: the draft's wrapped rows, at most a third of
    /// the screen and at least one.
    pub(crate) fn input_height(&self) -> usize {
        let cap = usize::from(self.height / 3).max(1);
        self.draft.rows(self.width).len().min(cap)
    }

    /// Whether new output arrived while scrolled up.
    pub(crate) fn has_new(&self) -> bool {
        self.scroll.has_new
    }

    /// The top wrapped row while scrolled up; `None` follows.
    pub(crate) fn top(&self) -> Option<usize> {
        self.scroll.top
    }

    /// The conversation's rows: the screen less the input box or the
    /// panel in its place, the steering queue, the badge and the hint. None
    /// on a screen too short for them.
    pub(crate) fn conversation_height(&self) -> usize {
        let input = self.panel().map_or(self.input_height(), |panel| {
            panel
                .lines
                .iter()
                .map(|line| crate::view::rows(ratatui::text::Line::raw(line.as_str()), self.width))
                .sum()
        });
        let below = input
            + self.completion_rows()
            + self.steering().len()
            + usize::from(self.badge().is_some())
            + usize::from(self.hint());
        usize::from(self.height).saturating_sub(below)
    }

    /// The resident conversation's lines, before wrapping.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> Vec<ratatui::text::Line<'static>> {
        self.rows().into_iter().map(|(line, _)| line).collect()
    }

    /// Opens or closes what `target` names.
    pub(crate) fn open(&mut self, target: Target) {
        if self.pages.open(&target) {
            self.scroll.changed();
            self.settle();
        }
    }

    /// The resident pages' lines.
    #[cfg(test)]
    pub(super) fn rows(&self) -> Vec<Row> {
        self.pages.rows()
    }

    /// The top row shown and every row: what a scroll bar draws.
    pub(crate) fn scroll(&self) -> (usize, usize) {
        (self.view_top(), self.pages.index().total())
    }

    /// The lines drawing rows `[top, top + height)`.
    pub(crate) fn shown(&self, top: usize, height: usize) -> crate::window::Shown {
        self.pages.shown(top, height)
    }

    /// The seq ranges of pages the next frame needs and does not hold.
    pub(crate) fn needs(&self) -> Vec<RangeInclusive<Seq>> {
        self.pages
            .needs(self.view_top(), self.conversation_height())
    }

    /// Folds a fetched range's durable lines into their pages.
    pub(crate) fn load(&mut self, lines: Vec<Envelope>) {
        self.pages.load(&lines);
        self.settle();
    }

    /// Loading `range` failed: its rows stay blank and the notice says why.
    /// A failed page ends a whole-turn copy waiting on dropped pages.
    pub(crate) fn load_failed(&mut self, range: &RangeInclusive<Seq>, message: &str) {
        self.pages.fail(*range.start());
        self.cancel_pending_turn();
        self.notices
            .push(format!("Could not load history: {message}"));
    }

    /// Scrolls so `row` is the top row, as dragging the scroll bar does.
    pub(crate) fn jump(&mut self, row: usize) {
        self.scroll.top = Some(row);
        self.settle();
    }

    /// The pages.
    pub(crate) fn pages(&self) -> &Pages {
        &self.pages
    }

    /// The top row when following: the last screenful.
    pub(super) fn bottom_top(&self) -> usize {
        let total = self.pages.index().total();
        total.saturating_sub(self.conversation_height())
    }

    /// The top row shown: the bottom while following, and never past it.
    fn view_top(&self) -> usize {
        let bottom = self.bottom_top();
        self.scroll.top.map_or(bottom, |top| top.min(bottom))
    }

    /// Clamps the top to the bottom and drops the pages outside the window.
    pub(super) fn settle_pages(&mut self) {
        if self.scroll.top.is_some() {
            self.scroll.top = Some(self.view_top());
        }
        self.pages.trim(self.view_top(), self.conversation_height());
    }
}
