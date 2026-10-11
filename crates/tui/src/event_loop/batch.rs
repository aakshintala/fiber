//! Takes one batch of ready inputs for one frame (`docs/tui.md`,
//! "History and paging"): hub lines and ticks share the frame in arrival
//! order, queued terminal reads batch in arrival order so a run of wheel
//! steps shares one frame, and anything else keeps its own, so keys,
//! clicks and resizes draw at once and click targets never age.

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use crate::Input;

/// The most inputs one frame folds, so a stream that never empties still
/// draws.
// debt: 4,096 is picked, not measured; a measured fold rate per line on the benchmark runner would set it.
pub(crate) const HUB_BATCH: usize = 4096;

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
/// to [`HUB_BATCH`]: the stash's front while it joins, then the
/// channel's while something waits. A ready input that joins nothing
/// goes back to the stash's front for the next batch, so it runs before
/// anything behind it; an empty or closed channel ends this one. Never
/// blocks, and drops and duplicates nothing.
pub(super) fn batch(first: Input, stash: &mut VecDeque<Input>, rx: &Receiver<Input>) -> Vec<Input> {
    let mut out = Vec::new();
    if !batchable(&first) && !matches!(&first, Input::Bytes(_)) {
        return vec![first];
    }
    out.push(first);
    loop {
        if out.len() >= HUB_BATCH {
            break;
        }
        let next = match stash.pop_front() {
            Some(input) => Some(input),
            None => rx.try_recv().ok(),
        };
        let Some(input) = next else {
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

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
