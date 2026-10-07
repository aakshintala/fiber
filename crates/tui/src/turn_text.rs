//! A turn's whole text over paged history (`docs/tui.md`, "History and
//! paging"): which pages hold a turn's fragments, so `y` and Ctrl+G load
//! every dropped one before copying or opening it.

use crate::window::Pages;

#[cfg(test)]
#[path = "turn_text_tests.rs"]
mod tests;

/// A whole-turn copy waiting on dropped pages: its turn and the pages
/// it keeps resident until the copy runs or is abandoned.
pub(crate) struct PendingTurn {
    pub(crate) turn: usize,
    pub(crate) pages: Vec<usize>,
}

/// The pages holding turn `turn`: page `at` holds it from its seed's first
/// turn to the next page's first, inclusive when the page's cut says the
/// next page begins inside the same turn. The open page holds every turn
/// from its first on. `firsts` and `cuts` hold one entry per page.
pub(crate) fn pages_for_turn(
    firsts: &[usize],
    cuts: &[bool],
    count: usize,
    turn: usize,
) -> Vec<usize> {
    let mut out = Vec::new();
    for at in 0..count {
        let Some(first) = firsts.get(at).copied() else {
            continue;
        };
        if turn < first {
            continue;
        }
        let Some(next) = firsts.get(at + 1).copied() else {
            out.push(at);
            continue;
        };
        let end = match cuts.get(at).copied() {
            Some(true) => next,
            _ => next.saturating_sub(1),
        };
        if turn <= end {
            out.push(at);
        }
    }
    out
}

/// The pages holding turn `turn` whose cards are dropped.
pub(crate) fn missing_pages(pages: &Pages, turn: usize) -> Vec<usize> {
    let count = pages.page_count();
    let mut firsts = Vec::with_capacity(count);
    let mut cuts = Vec::with_capacity(count);
    for at in 0..count {
        firsts.push(pages.page_first(at).unwrap_or(usize::MAX));
        cuts.push(pages.page_cut(at));
    }
    pages_for_turn(&firsts, &cuts, count, turn)
        .into_iter()
        .filter(|at| pages.part(*at).is_none())
        .collect()
}

/// Keeps every dropped page holding turn `turn` resident until its copy
/// runs, returning those pages.
pub(crate) fn request_turn(pages: &mut Pages, turn: usize) -> Vec<usize> {
    let missing = missing_pages(pages, turn);
    for at in &missing {
        pages.want(*at);
    }
    missing
}

/// Lets the pages holding turn `turn` drop with the window again.
pub(crate) fn release_turn(pages: &mut Pages, turn: usize) {
    let count = pages.page_count();
    let mut firsts = Vec::with_capacity(count);
    let mut cuts = Vec::with_capacity(count);
    for at in 0..count {
        firsts.push(pages.page_first(at).unwrap_or(usize::MAX));
        cuts.push(pages.page_cut(at));
    }
    for at in pages_for_turn(&firsts, &cuts, count, turn) {
        pages.unwant(at);
    }
}
