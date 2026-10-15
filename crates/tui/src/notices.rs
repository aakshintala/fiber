//! Notices as rows above the working line (`docs/tui.md`, "Notices").
//!
//! A notice is this terminal's own memory: the terminal's own messages and
//! the attached session's `notice` lines, never written anywhere. Oldest
//! first, at most three show, then "+N more". A child of `app`, so the calls
//! the click layer makes sit here beside the stack.

use super::App;

/// How many notices show before "+N more".
const SHOWN: usize = 3;

/// One notice.
#[derive(Debug)]
struct Notice {
    id: usize,
    code: String,
    text: String,
}

/// What the overlay shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    /// One notice's whole text.
    One(usize),
    /// Every notice.
    All,
}

/// The notices, oldest first, and the overlay.
#[derive(Debug, Default)]
pub(crate) struct Notices {
    next: usize,
    stack: Vec<Notice>,
    overlay: Option<Overlay>,
    /// While a drag selects, the first id pushed since it began: that
    /// notice and later ones wait for the release ("Notices": a notice that
    /// arrives during a drag waits for the release).
    held_from: Option<usize>,
}

/// One drawn row: a notice, or the "+N more" row for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoticeLine {
    /// A notice, oldest shown first.
    Notice {
        /// The notice it shows.
        id: usize,
        /// Its registry code (`docs/errors.md`, "Registry").
        code: String,
        /// Its message.
        text: String,
    },
    /// How many shown notices have no row.
    More(usize),
}

impl Notices {
    /// Adds `text` under `code` on top.
    pub(crate) fn push(&mut self, code: &str, text: String) {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        self.stack.push(Notice {
            id,
            code: code.to_owned(),
            text,
        });
    }

    /// The newest notice's text.
    #[cfg(test)]
    pub(crate) fn newest(&self) -> Option<&str> {
        self.stack.last().map(|notice| notice.text.as_str())
    }

    /// A drag began: notices pushed from now on wait, kept but not drawn,
    /// listed or clickable, until [`Notices::release`].
    pub(crate) fn hold(&mut self) {
        self.held_from.get_or_insert(self.next);
    }

    /// The drag ended: the notices that waited show, oldest first.
    pub(crate) fn release(&mut self) {
        self.held_from = None;
    }

    /// The notices that show, oldest first: all but those a drag holds.
    fn shown(&self) -> impl DoubleEndedIterator<Item = &Notice> {
        let held = self.held_from;
        self.stack
            .iter()
            .filter(move |notice| held.is_none_or(|from| notice.id < from))
    }

    /// How many rows [`Notices::lines`] draws, without building them.
    pub(crate) fn drawn(&self) -> usize {
        let shown = self.shown().count();
        shown.min(SHOWN) + usize::from(shown > SHOWN)
    }

    /// The rows the view draws, oldest first: at most [`SHOWN`] notices,
    /// then one `More` for the rest.
    pub(crate) fn lines(&self) -> Vec<NoticeLine> {
        let mut lines: Vec<NoticeLine> = self
            .shown()
            .take(SHOWN)
            .map(|notice| NoticeLine::Notice {
                id: notice.id,
                code: notice.code.clone(),
                text: notice.text.clone(),
            })
            .collect();
        let rest = self.shown().count().saturating_sub(SHOWN);
        if rest > 0 {
            lines.push(NoticeLine::More(rest));
        }
        lines
    }

    /// Removes notice `id`, closing an overlay that showed it alone.
    pub(crate) fn dismiss(&mut self, id: usize) {
        self.stack.retain(|notice| notice.id != id);
        if self.overlay == Some(Overlay::One(id)) {
            self.overlay = None;
        }
    }

    /// Opens notice `id` whole in the overlay.
    pub(crate) fn open(&mut self, id: usize) {
        if self.stack.iter().any(|notice| notice.id == id) {
            self.overlay = Some(Overlay::One(id));
        }
    }

    /// Opens every notice in the overlay.
    pub(crate) fn open_all(&mut self) {
        if !self.stack.is_empty() {
            self.overlay = Some(Overlay::All);
        }
    }

    /// Closes the overlay; false when none was open.
    pub(crate) fn close(&mut self) -> bool {
        self.overlay.take().is_some()
    }

    /// The overlay's text, newest first, while it is open.
    pub(crate) fn overlay(&self) -> Option<Vec<String>> {
        let overlay = self.overlay?;
        Some(
            self.shown()
                .rev()
                .filter(|notice| overlay == Overlay::All || overlay == Overlay::One(notice.id))
                .map(|notice| notice.text.clone())
                .collect(),
        )
    }

    /// Notice `id`'s whole text, while it is in the stack.
    pub(crate) fn text(&self, id: usize) -> Option<&str> {
        self.stack
            .iter()
            .find(|notice| notice.id == id)
            .map(|notice| notice.text.as_str())
    }
}

impl App {
    /// The notice rows the view draws above the working line.
    pub(crate) fn notices(&self) -> Vec<NoticeLine> {
        self.notices.lines()
    }

    /// The notice overlay's text while it is open.
    pub(crate) fn notice_overlay(&self) -> Option<Vec<String>> {
        self.notices.overlay()
    }

    /// The newest notice's text.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&str> {
        self.notices.newest()
    }

    /// ✕ on notice `id`.
    pub(crate) fn dismiss_notice(&mut self, id: usize) {
        self.notices.dismiss(id);
    }

    /// A click on notice `id`: its whole text in the overlay.
    pub(crate) fn open_notice(&mut self, id: usize) {
        self.notices.open(id);
    }

    /// A click on "+N more": every notice in the overlay.
    pub(crate) fn open_more_notices(&mut self) {
        self.notices.open_all();
    }
}

#[cfg(test)]
#[path = "notices_tests.rs"]
mod tests;
