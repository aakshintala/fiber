//! The conversation's click targets, and a code block's `copy` cells,
//! which copy its code and show "Copied" until the next key or click
//! (`docs/tui.md`, "Look", "Selection and copy").

use std::ops::Range;

use super::{App, Effect, Target};
use crate::markdown::CopyTarget;
use crate::turn::{Entry, Turn};

/// The code block `target` names among `turns`, rendered at the card's
/// inner width with its copy cells shifted past the card's padding, so
/// the hit lands on the drawn label (`docs/tui.md`, "Turns").
pub(crate) fn copy_target(turns: &[Turn], target: Target, width: u16) -> Option<CopyTarget> {
    let Target::Copy { reply: id, block } = target else {
        return None;
    };
    let inner = crate::surface::inset(width);
    let pad = width.saturating_sub(inner) / 2;
    turns
        .iter()
        .flat_map(|turn| &turn.entries)
        .find_map(|entry| match entry {
            Entry::Reply { reply, .. } if reply.id() == id => {
                reply.rendered(inner).target(block).map(|mut target| {
                    target.cols.start = target.cols.start.saturating_add(pad);
                    target.cols.end = target.cols.end.saturating_add(pad);
                    target
                })
            }
            Entry::Reply { .. }
            | Entry::Steer(_)
            | Entry::Group(_)
            | Entry::Aside(_)
            | Entry::Band(_)
            | Entry::Answers(_) => None,
        })
}

impl App {
    /// For each line of [`Self::lines`] that opens something, its index and
    /// what it opens.
    #[cfg(test)]
    pub(crate) fn targets(&self) -> Vec<(usize, Target)> {
        self.screen
            .pages()
            .rows()
            .into_iter()
            .enumerate()
            .filter_map(|(at, (_, target))| target.map(|target| (at, target)))
            .collect()
    }

    /// The columns of the `copy` cells `target` covers on its line, when
    /// it is a code block's copy target.
    pub(crate) fn copy_cells(&self, target: Target) -> Option<Range<u16>> {
        self.screen
            .pages()
            .copy_target(target)
            .map(|copy| copy.cols)
    }

    /// A click on a code block's `copy`: copies its code and shows
    /// "Copied".
    pub(super) fn copy(&mut self, target: Target) -> Effect {
        let code = self.screen.pages().copy_target(target);
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
