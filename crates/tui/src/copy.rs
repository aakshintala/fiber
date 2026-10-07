//! What a click on the conversation reaches: each line's target, and a
//! code block's `copy` cells, which copy its code and show "Copied" until
//! the next key or click (`docs/tui.md`, "Look", "Selection and copy").

use super::{App, Effect, Target};

impl App {
    /// For each line of [`Self::lines`] that opens something, its index and
    /// what it opens.
    pub(crate) fn targets(&self) -> Vec<(usize, Target)> {
        self.rows()
            .into_iter()
            .enumerate()
            .filter_map(|(at, (_, target))| target.map(|target| (at, target)))
            .collect()
    }

    /// A left click at a 0-based cell: on a code block's `copy` it copies
    /// the code and shows "Copied", which any click first clears.
    #[cfg_attr(not(test), expect(dead_code, reason = "#995's mouse events call it"))]
    pub(crate) fn on_click(&mut self, col: u16, row: u16) -> Effect {
        let code = crate::view::target_at(self, self.width, usize::from(row))
            .and_then(|target| crate::turn::copy_target(&self.turns, target, self.width))
            .filter(|copy| copy.cols.contains(&col));
        self.copied = code.is_some();
        code.map_or(Effect::None, |copy| Effect::Copy(copy.code))
    }

    /// Whether "Copied" shows.
    pub(crate) fn copied(&self) -> bool {
        self.copied
    }
}

#[cfg(test)]
#[path = "copy_tests.rs"]
mod tests;
