//! Takes one batch of ready inputs for one frame (`docs/tui.md`,
//! "History and paging"): hub lines and ticks share the frame in arrival
//! order, and anything else keeps its own, so keys, clicks and resizes
//! draw at once and click targets never age.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::Input;

/// The most inputs one frame folds, so a stream that never empties still
/// draws.
// debt: 4,096 is picked, not measured; a measured fold rate per line on the benchmark runner would set it.
pub(super) const HUB_BATCH: usize = 4096;

/// Whether the batch takes `input` with the ones waiting: hub lines and
/// ticks only.
fn batchable(input: &Input) -> bool {
    matches!(input, Input::Hub(_) | Input::Tick)
}

/// `first` and the inputs already ready behind it, in arrival order, up
/// to [`HUB_BATCH`]: the stash's front while batchable, then the
/// channel's while something waits. A ready non-batchable input goes back
/// to the stash's front for the next batch, so it runs before anything
/// behind it; an empty or closed channel ends this one. Never blocks, and
/// drops and duplicates nothing.
pub(super) fn batch(first: Input, stash: &mut VecDeque<Input>, rx: &Receiver<Input>) -> Vec<Input> {
    let mut out = Vec::new();
    if !batchable(&first) {
        return vec![first];
    }
    out.push(first);
    while out.len() < HUB_BATCH {
        match stash.pop_front() {
            Some(input) if batchable(&input) => out.push(input),
            Some(input) => {
                stash.push_front(input);
                break;
            }
            None => match rx.try_recv() {
                Ok(input) if batchable(&input) => out.push(input),
                Ok(input) => {
                    stash.push_front(input);
                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            },
        }
    }
    out
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
