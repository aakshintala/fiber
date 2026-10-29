//! Reads validated across a concurrent write (`docs/system-prompt.md`,
//! "When something changes"): a stat taken before the read and one taken
//! after it, so a read landing mid-write never pins a stale header under
//! the finished file's size and time.

use crate::changes::{Stat, stat_of};

#[cfg(test)]
thread_local! {
    /// The held pause the test arms at the helper's hook edge, bound to
    /// one path: the race then happens on every run. Consumed only when
    /// the site path equals its target, so neighbouring reads pass
    /// through. Thread-local: the worker thread running the hooked
    /// operation arms it, so the test thread's waits never see it.
    /// Inert when unarmed and confined to test builds.
    #[allow(
        clippy::type_complexity,
        reason = "one test-only slot holding a target plus a held pause; a named type would only rename it"
    )]
    static HOOK: std::cell::RefCell<Option<(String, Box<dyn FnOnce()>)>> =
        std::cell::RefCell::new(None);
}

/// Arms the one-shot held pause for `target`: the next helper call on that
/// path signals `entered` at its hook edge and blocks on `release` until
/// the test releases it, then clears the slot. Call inside the worker
/// thread that runs the hooked operation: the slot is thread-local, so an
/// arm on the test thread never reaches the worker. The test thread waits
/// on `entered` with a `Deadline`, rewrites the file, then releases;
/// dropping the release sender unblocks the worker too, so a test that
/// fails still ends the wait.
#[cfg(test)]
pub(crate) fn arm(
    target: &str,
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) {
    HOOK.with(|slot| {
        *slot.borrow_mut() = Some((
            target.to_owned(),
            Box::new(move || {
                let _sent = entered.send(());
                let _released = release.recv();
            }),
        ));
    });
}

/// Clears an armed hook without running it.
#[cfg(test)]
pub(crate) fn disarm() {
    HOOK.with(|slot| {
        slot.borrow_mut().take();
    });
}

/// Runs the armed held pause when `path` equals its target, then clears
/// it: signals the test thread and blocks until the test releases it.
/// Dropping the release sender unblocks the wait, so a failing test still
/// ends it.
#[cfg(test)]
fn run_hook(path: &str) {
    let matched =
        HOOK.with(|slot| matches!(slot.borrow().as_ref(), Some((target, _)) if target == path));
    if matched {
        let (_, rewrite) = HOOK.with(|slot| slot.borrow_mut().take().unwrap());
        rewrite();
    }
}

/// Does nothing outside test builds: the helpers never divert.
#[cfg(not(test))]
fn run_hook(_path: &str) {}

/// The bytes at `path` with the stat of what was read: the stat before
/// the read and the stat after it, returning the after-stat only when the
/// two match, else no stat. The caller stores a stat only on `Some`, and
/// serves the bytes for this turn either way, so the next turn start
/// reads the file again.
pub(crate) fn read_validated(path: &str) -> (std::io::Result<Vec<u8>>, Option<Stat>) {
    let before = stat_of(path);
    let bytes = std::fs::read(path);
    run_hook(path);
    let after = stat_of(path);
    // A stat taken after a read is stored as the stat of what was read
    // only when nothing moved across the read.
    if before == after {
        (bytes, after)
    } else {
        (bytes, None)
    }
}

/// The bytes at `path` with the stat taken immediately before the read:
/// the baseline the opening build stores, so a change between the stat
/// and the read misses the size-and-time shortcut at the next check and
/// content decides.
pub(crate) fn read_pre(path: &str) -> (std::io::Result<Vec<u8>>, Option<Stat>) {
    let before = stat_of(path);
    run_hook(path);
    let bytes = std::fs::read(path);
    (bytes, before)
}
