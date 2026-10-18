//! The test-only pause seam holding the fold while a test counts
//! live hub events: `Loop::run` waits in [`before_fold`] before it folds
//! a hub chunk, and the test releases it chunk by chunk. [`batch_ended`]
//! reports each held batch's end with its taken count and waits there,
//! before the frame and the next batch, until the test has queued the
//! next part and releases it.

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender};

thread_local! {
    static PAUSE: RefCell<Option<(Sender<()>, Receiver<()>)>> = const { RefCell::new(None) };
    static ENDS: RefCell<Option<(Sender<usize>, Receiver<()>)>> = const { RefCell::new(None) };
}

/// Holds the loop at its next fold on this thread: each [`before_fold`]
/// call sends on `reached`, then waits for one `release` send. The test
/// owns the other ends, so dropping its release sender lets every later
/// call through, and a panicking test releases the loop.
pub(super) fn install(reached: Sender<()>, release: Receiver<()>) {
    PAUSE.with(|pause| *pause.borrow_mut() = Some((reached, release)));
}

/// Waits once as [`install`] says, on the thread that installed it. With
/// nothing installed it returns at once, so every other test runs
/// through; a dropped release sender makes every later call return at
/// once.
pub(super) fn before_fold() {
    PAUSE.with(|pause| {
        let held = pause.borrow();
        let Some((reached, release)) = held.as_ref() else {
            return;
        };
        reached.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    });
}

/// Reports each held batch's end on this thread: every later
/// [`batch_ended`] sends its taken count on `ended`, then waits for one
/// `release` send. The test owns the other ends, so dropping its release
/// sender lets every later call through, and a panicking test releases
/// the loop.
pub(super) fn report_ends(ended: Sender<usize>, release: Receiver<()>) {
    ENDS.with(|ends| *ends.borrow_mut() = Some((ended, release)));
}

/// Sends `taken`, the held batch's input count, as [`report_ends`]
/// says: [`HUB_BATCH`](super::batch::HUB_BATCH) for a full batch, fewer
/// when the channel emptied. Then it waits once for the release, so the
/// loop takes its next batch only from what the test queued before it.
/// With nothing installed it returns at once; a dropped release sender
/// makes every later call return at once.
pub(super) fn batch_ended(taken: usize) {
    ENDS.with(|ends| {
        if let Some((ended, release)) = ends.borrow().as_ref() {
            ended.send(taken).unwrap_or(());
            release.recv().unwrap_or(());
        }
    });
}
