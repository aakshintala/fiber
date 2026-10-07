//! The conversation's click targets, and a code block's `copy` cells,
//! which copy its code and show "Copied" until the next key or click
//! (`docs/tui.md`, "Look", "Selection and copy").

use std::ops::Range;

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

    /// The columns of the `copy` cells `target` covers on its line, when
    /// it is a code block's copy target.
    pub(crate) fn copy_cells(&self, target: Target) -> Option<Range<u16>> {
        crate::turn::copy_target(&self.turns, target, self.width).map(|copy| copy.cols)
    }

    /// A click on a code block's `copy`: copies its code and shows
    /// "Copied".
    pub(super) fn copy(&mut self, target: Target) -> Effect {
        let code = crate::turn::copy_target(&self.turns, target, self.width);
        self.copied = code.is_some();
        code.map_or(Effect::None, |copy| Effect::Copy(copy.code))
    }

    /// Hides "Copied": a left press, on a target or not, starts a click.
    pub(crate) fn clear_copied(&mut self) {
        self.copied = false;
    }

    /// Whether "Copied" shows.
    pub(crate) fn copied(&self) -> bool {
        self.copied
    }
}

#[cfg(test)]
#[path = "copy_tests.rs"]
mod tests;
