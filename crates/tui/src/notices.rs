//! Notices floating in the conversation's top-right corner (`docs/tui.md`,
//! "Notices").
//!
//! A notice is this terminal's own memory: the terminal's own messages and
//! the attached session's `notice` lines, never written anywhere. Newest on
//! top, at most three show, then "+N more". A child of `app`, so the calls
//! the click layer makes sit here beside the stack.

use super::App;
use crate::format;

/// How many notices show before "+N more".
const SHOWN: usize = 3;

/// The lines a notice box wraps to before it cuts.
const LINES: usize = 3;

/// The widest a notice box gets, in columns.
const WIDEST: usize = 60;

/// What a box cut short ends with.
const MORE: &str = "… more";

/// One notice.
#[derive(Debug)]
struct Notice {
    id: usize,
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
}

/// One box as the view draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoticeBox {
    /// The notice it shows; `None` for the "+N more" row.
    pub(crate) id: Option<usize>,
    /// Its rows, each the box's width; a notice's first ends in "✕".
    pub(crate) rows: Vec<String>,
}

impl Notices {
    /// Adds `text` on top.
    pub(crate) fn push(&mut self, text: String) {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        self.stack.push(Notice { id, text });
    }

    /// The newest notice's text.
    #[cfg(test)]
    pub(crate) fn newest(&self) -> Option<&str> {
        self.stack.last().map(|notice| notice.text.as_str())
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
            self.stack
                .iter()
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

    /// The boxes for a conversation `columns` wide, newest first: at most
    /// [`SHOWN`], then "+N more" for the rest. Each is 40% of the width, at
    /// most [`WIDEST`] columns, and wraps to [`LINES`] lines, the last cut
    /// to end "… more".
    pub(crate) fn boxes(&self, columns: u16) -> Vec<NoticeBox> {
        let wide = (usize::from(columns).saturating_mul(2) / 5).min(WIDEST);
        // The text leaves a space and the ✕ at the right.
        let text = wide.saturating_sub(2);
        if text == 0 {
            return Vec::new();
        }
        let mut boxes: Vec<NoticeBox> = self
            .stack
            .iter()
            .rev()
            .take(SHOWN)
            .map(|notice| NoticeBox {
                id: Some(notice.id),
                rows: rows(&notice.text, text),
            })
            .collect();
        let rest = self.stack.len().saturating_sub(SHOWN);
        if rest > 0 {
            boxes.push(NoticeBox {
                id: None,
                rows: vec![pad(&format::cut(&format!("+{rest} more"), wide), wide)],
            });
        }
        boxes
    }
}

/// `text` wrapped to `width` columns in at most [`LINES`] rows, the last
/// cut to end "… more" when there is more; each row padded, the first
/// ending in "✕".
fn rows(text: &str, width: usize) -> Vec<String> {
    let mut lines = format::wrap(text, width);
    if lines.len() > LINES {
        lines.truncate(LINES);
        if let Some(last) = lines.last_mut() {
            let room = width.saturating_sub(format::width(MORE).saturating_add(1));
            let kept = format::cut(last, room);
            *last = format::cut(format!("{kept} {MORE}").trim_start(), width);
        }
    }
    lines
        .iter()
        .enumerate()
        .map(|(at, line)| {
            let mark = if at == 0 { "✕" } else { " " };
            format!("{} {mark}", pad(line, width))
        })
        .collect()
}

/// `text` padded with spaces to `width` columns.
fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(format::width(text));
    format!("{text}{}", " ".repeat(fill))
}

impl App {
    /// The notice boxes the view floats over the conversation.
    pub(crate) fn notices(&self) -> Vec<NoticeBox> {
        self.notices.boxes(self.width)
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
