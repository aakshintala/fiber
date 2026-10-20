//! The conversation's geometry: the screen's size, the scroll position, and
//! the pages resident in the window over the session's history
//! (`docs/tui.md`, "History and paging").
//!
//! The conversation's height depends on what is drawn below it, which the
//! geometry does not own, so each method that needs it takes it as
//! `height`.

use std::ops::RangeInclusive;

use contract::{Seq, SessionId};

use super::Target;
use crate::view::Scroll;
use crate::window::Pages;

/// Whether the scroll position moves: live, or frozen while an open
/// holds the preview, keeping one deferred resize for the unfreeze.
/// One enum, never two fields: a frozen screen with no width stored
/// is `Frozen { pending_width: None }`, never a contradiction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Freeze {
    /// The scroll position moves.
    Live,
    /// The scroll keys wait; clearing applies one stored resize.
    Frozen { pending_width: Option<u16> },
}

/// The screen's size, the scroll position and the resident pages. The top
/// row is never past the bottom once settled, and the size is at least
/// 1x1.
pub(crate) struct Screen {
    width: u16,
    height: u16,
    pages: Pages,
    scroll: Scroll,
    freeze: Freeze,
}

impl Screen {
    /// An 80x24 screen with no pages, following.
    pub(super) fn new() -> Self {
        Self {
            width: 80,
            height: 24,
            pages: Pages::new(80),
            scroll: Scroll::default(),
            freeze: Freeze::Live,
        }
    }

    /// Freezes the scroll position while an open holds the preview: the
    /// scroll keys wait, and clearing applies one stored resize.
    pub(super) fn set_frozen(&mut self, frozen: bool) {
        if frozen {
            if matches!(self.freeze, Freeze::Live) {
                self.freeze = Freeze::Frozen {
                    pending_width: None,
                };
            }
        } else if let Freeze::Frozen { pending_width } =
            std::mem::replace(&mut self.freeze, Freeze::Live)
            && let Some(width) = pending_width
        {
            self.pages.set_width(width);
        }
    }

    /// Whether the scroll position is frozen.
    pub(super) fn frozen(&self) -> bool {
        matches!(self.freeze, Freeze::Frozen { .. })
    }

    /// Sets the size, each side at least 1. The pages wrap at what
    /// [`Screen::wrap_at`] names, which the app sets from the layout.
    pub(super) fn set_size(&mut self, width: u16, height: u16) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    /// Wraps the pages at `width` columns; a new width re-counts every
    /// page. While frozen the width is stored for the unfreeze and
    /// nothing moves. Returns whether the width changed.
    pub(super) fn wrap_at(&mut self, width: u16) -> bool {
        let width = width.max(1);
        if let Freeze::Frozen { pending_width } = &mut self.freeze {
            *pending_width = Some(width);
            return false;
        }
        let changed = width != self.pages.wrap_width();
        self.pages.set_width(width);
        changed
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

    /// Jumps to the bottom and follows new output. A follow ends the
    /// held landing's anchor, wherever the follow comes from: a key,
    /// a click on "new below", or the landing's own fallback.
    pub(super) fn follow(&mut self) {
        self.pages.clear_anchor();
        self.scroll.follow();
    }

    /// New output arrived: while scrolled up, the overlay shows.
    pub(super) fn changed(&mut self) {
        self.scroll.changed();
    }

    /// Scrolls so `row` is the top row; the next settle clamps it.
    /// While frozen the top does not move. A jump ends the held
    /// landing's anchor, wherever the jump comes from: a key, a
    /// search reveal, or a scroll-bar drag.
    pub(super) fn jump(&mut self, row: usize) {
        self.pages.clear_anchor();
        self.jump_kept(row);
    }

    /// Scrolls so `row` is the top row, keeping the held landing's
    /// anchor: the landing's own restore and a later load's re-anchor,
    /// never a scroll, follow or navigation. While frozen the top does
    /// not move.
    pub(super) fn jump_kept(&mut self, row: usize) {
        if self.frozen() {
            return;
        }
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

    /// Opens what `target` names without ever closing, as a search
    /// expanding the sections around its current match does; whether
    /// anything changed (`docs/tui.md`, "Search").
    pub(super) fn force_open(&mut self, target: &Target) -> bool {
        let changed = self.pages.force_open(target);
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
    /// While frozen the top does not move. A page ends the held
    /// landing's anchor.
    pub(super) fn page(&mut self, up: bool, height: usize) {
        self.pages.clear_anchor();
        if self.frozen() {
            return;
        }
        self.scroll_rows_kept(up, height.saturating_sub(1).max(1), height);
    }

    /// Scrolls by `step` rows: up from the top row, or from the bottom
    /// while following, saturating at the top; down follows again on
    /// reaching the bottom (`docs/tui.md`, "History and paging").
    /// While frozen the top does not move. A scroll ends the held
    /// landing's anchor, wherever the scroll comes from: a key or the
    /// mouse wheel.
    pub(super) fn scroll_rows(&mut self, up: bool, step: usize, height: usize) {
        self.pages.clear_anchor();
        if self.frozen() {
            return;
        }
        self.scroll_rows_kept(up, step, height);
    }

    /// Scrolls by `step` rows, keeping the held landing's anchor: a
    /// page's own step, never a scroll of its own. While frozen the
    /// top does not move.
    fn scroll_rows_kept(&mut self, up: bool, step: usize, height: usize) {
        if self.frozen() {
            return;
        }
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
    /// While frozen the top does not move. A reveal ends the held
    /// landing's anchor: focus navigation moves the top.
    pub(super) fn reveal(&mut self, row: usize, height: usize) {
        self.pages.clear_anchor();
        if height == 0 || self.frozen() {
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

    /// The seq ranges of pages the window needs and does not hold, each
    /// with its segment's session.
    pub(super) fn needs(&self, height: usize) -> Vec<(SessionId, RangeInclusive<Seq>)> {
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

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
