//! Takes one batch of ready inputs for one frame (`docs/tui.md`,
//! "History and paging"): hub lines and ticks share the frame in arrival
//! order, queued terminal reads batch in arrival order so a run of wheel
//! steps shares one frame, and anything else keeps its own, so keys,
//! clicks and resizes draw at once and click targets never age.

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use crate::Input;
use crate::link::Credits;

/// The most inputs one frame folds, so a stream that never empties still
/// draws.
// debt: 4,096 is picked, not measured; a measured fold rate per line on the benchmark runner would set it.
pub(crate) const HUB_BATCH: usize = 4096;

/// The most inputs one hub chunk holds: the batch's share of K, so a
/// hub batch folds chunk by chunk inside one logical batch of up to
/// [`HUB_BATCH`].
pub(super) const HOLD: usize = 64;

/// Whether the batch takes `input` with the ones waiting, and whether a
/// batch starting with `input` holds: hub lines and ticks only. Hub
/// lines and ticks never swap screens, so only they hold; a key, click
/// or resize may open a delegate, which swaps the shown screen.
pub(super) fn batchable(input: &Input) -> bool {
    matches!(input, Input::Hub(_) | Input::Tick)
}

/// Whether `next` joins a batch starting with `first`: two terminal
/// reads, or two batchable inputs. A hub line never joins a Bytes batch,
/// so it waits at the stash's front for the next batch; a Bytes input
/// never joins a hub batch, so hub batches hold exactly as before. Any
/// other input joins nothing and keeps its own batch.
pub(super) fn joins(first: &Input, next: &Input) -> bool {
    (matches!(first, Input::Bytes(_)) && matches!(next, Input::Bytes(_)))
        || (batchable(first) && batchable(next))
}

/// `first` and the inputs already ready behind it, in arrival order, up
/// to `left`, at most [`HOLD`] for a hub chunk: the stash's front while
/// it joins, then the channel's while something waits. A ready input
/// that joins nothing goes back to the stash's front for the next batch,
/// so it runs before anything behind it; an empty or closed channel ends
/// this one. The cap is checked before a pop, so no input is taken and
/// put back for being over the limit. Never blocks, and drops and
/// duplicates nothing.
pub(super) fn batch(
    first: Input,
    stash: &mut VecDeque<Input>,
    rx: &Receiver<Input>,
    credits: &Credits,
    left: usize,
) -> Vec<Input> {
    let mut out = Vec::new();
    if !batchable(&first) && !matches!(&first, Input::Bytes(_)) {
        return vec![first];
    }
    // A hub chunk holds at most `HOLD`; a terminal-read batch takes up
    // to `left`.
    let limit = if batchable(&first) {
        left.min(HOLD)
    } else {
        left
    };
    out.push(first);
    for _ in 0..limit.saturating_sub(1) {
        let Some(input) = ready(stash, rx, credits) else {
            break;
        };
        if out.first().is_some_and(|first| joins(first, &input)) {
            out.push(input);
        } else {
            stash.push_front(input);
            break;
        }
    }
    out
}

/// The next ready input: the stash's front, else the channel's,
/// releasing the unit the loop's receive gives back. Stash pops release
/// nothing: they were released when received. Never blocks.
pub(super) fn ready(
    stash: &mut VecDeque<Input>,
    rx: &Receiver<Input>,
    credits: &Credits,
) -> Option<Input> {
    if let Some(input) = stash.pop_front() {
        return Some(input);
    }
    rx.try_recv().ok().inspect(|input| credits.received(input))
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
