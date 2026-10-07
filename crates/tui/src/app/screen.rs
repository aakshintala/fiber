//! The conversation's geometry: the screen's size, the scroll position, and
//! the pages resident in the window over the session's history
//! (`docs/tui.md`, "History and paging").
//!
//! The conversation's height depends on what is drawn below it, which the
//! geometry does not own, so each method that needs it takes it as
//! `height`.

use std::ops::RangeInclusive;

use contract::Seq;

use super::Target;
use crate::view::Scroll;
use crate::window::Pages;

/// The screen's size, the scroll position and the resident pages. The top
/// row is never past the bottom once settled, and the size is at least
/// 1x1.
pub(crate) struct Screen {
    width: u16,
    height: u16,
    pages: Pages,
    scroll: Scroll,
}

impl Screen {
    /// An 80x24 screen with no pages, following.
    pub(super) fn new() -> Self {
        Self {
            width: 80,
            height: 24,
            pages: Pages::new(80),
            scroll: Scroll::default(),
        }
    }

    /// Sets the size, each side at least 1; a new width re-counts every
    /// page.
    pub(super) fn set_size(&mut self, width: u16, height: u16) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.pages.set_width(self.width);
    }

    /// The screen's columns.
    pub(super) fn width(&self) -> u16 {
        self.width
    }

    /// The screen's rows.
    pub(super) fn height(&self) -> u16 {
        self.height
    }

    /// The pages.
    pub(super) fn pages(&self) -> &Pages {
        &self.pages
    }

    /// The pages, for folding content into them.
    pub(super) fn pages_mut(&mut self) -> &mut Pages {
        &mut self.pages
    }

    /// Whether new output arrived while scrolled up.
    pub(super) fn has_new(&self) -> bool {
        self.scroll.has_new
    }

    /// The top wrapped row while scrolled up; `None` follows.
    pub(super) fn top(&self) -> Option<usize> {
        self.scroll.top
    }

    /// Jumps to the bottom and follows new output.
    pub(super) fn follow(&mut self) {
        self.scroll.follow();
    }

    /// New output arrived: while scrolled up, the overlay shows.
    pub(super) fn changed(&mut self) {
        self.scroll.changed();
    }

    /// Scrolls so `row` is the top row; the next settle clamps it.
    pub(super) fn jump(&mut self, row: usize) {
        self.scroll.top = Some(row);
    }

    /// Opens or closes what `target` names; whether anything changed.
    pub(super) fn open(&mut self, target: Target) -> bool {
        let changed = self.pages.open(&target);
        if changed {
            self.scroll.changed();
        }
        changed
    }

    /// Drops every page and follows.
    pub(super) fn clear(&mut self) {
        self.pages.clear();
        self.scroll.follow();
    }

    /// PageUp and PageDown move by `height` less one, at least one row.
    pub(super) fn page(&mut self, up: bool, height: usize) {
        let step = height.saturating_sub(1).max(1);
        let bottom = self.bottom_top(height);
        if up {
            self.scroll.up(step, bottom);
        } else {
            self.scroll.down(step, bottom);
        }
    }

    /// Scrolls so focus row `row` shows, keeping a wrapped target together
    /// when it fits and following again at the bottom. With no
    /// conversation rows there is nothing to show, and the scroll stays as
    /// it was.
    pub(super) fn reveal(&mut self, row: usize, height: usize) {
        if height == 0 {
            return;
        }
        let Some((_, rows, _)) = self
            .pages
            .focus_items()
            .into_iter()
            .find(|(at, _, _)| *at == row)
        else {
            return;
        };
        let height = height.max(1);
        let bottom = self.bottom_top(height);
        let top = self.scroll.top.map_or(bottom, |top| top.min(bottom));
        let end = row.saturating_add(rows.min(height));
        let next = top.clamp(end.saturating_sub(height), row);
        if next >= bottom {
            self.scroll.follow();
        } else {
            self.scroll.top = Some(next);
        }
    }

    /// The top row shown and every row: what a scroll bar draws.
    pub(super) fn scroll_bar(&self, height: usize) -> (usize, usize) {
        (self.view_top(height), self.pages.index().total())
    }

    /// The seq ranges of pages the window needs and does not hold.
    pub(super) fn needs(&self, height: usize) -> Vec<RangeInclusive<Seq>> {
        self.pages.needs(self.view_top(height), height)
    }

    /// Clamps the top to the bottom and drops the pages outside the window.
    pub(super) fn settle(&mut self, height: usize) {
        if self.scroll.top.is_some() {
            self.scroll.top = Some(self.view_top(height));
        }
        self.pages.trim(self.view_top(height), height);
    }

    /// The top row when following: the last screenful.
    fn bottom_top(&self, height: usize) -> usize {
        let total = self.pages.index().total();
        total.saturating_sub(height)
    }

    /// The top row shown: the bottom while following, and never past it.
    fn view_top(&self, height: usize) -> usize {
        let bottom = self.bottom_top(height);
        self.scroll.top.map_or(bottom, |top| top.min(bottom))
    }
}
