//! A per-path lock that holds nothing, and a fake one with real
//! exclusion (`contract::files::PathLock`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use support::lock;

use contract::files::PathLock;

/// A per-path lock that holds nothing: `hold` and `hold_all` run `run`
/// once without locking.
pub struct NoLock;

impl PathLock for NoLock {
    fn hold(&self, _path: &Path, run: &mut dyn FnMut()) {
        run();
    }

    fn hold_all(&self, _paths: &[PathBuf], run: &mut dyn FnMut()) {
        run();
    }
}

/// A fake per-path lock for tests: real exclusion, so a test observes a
/// locked call wait, and the keys it was asked for, in order.
pub struct FakeLock {
    state: Mutex<FakeState>,
    changed: Condvar,
    calls: Mutex<Vec<PathBuf>>,
}

struct FakeState {
    held: BTreeSet<PathBuf>,
    /// Callers blocked in [`FakeLock::hold`], raised before they wait.
    waiting: usize,
}

impl FakeLock {
    /// A lock holding nothing.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FakeState {
                held: BTreeSet::new(),
                waiting: 0,
            }),
            changed: Condvar::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The keys `hold` was asked for, in order.
    pub fn calls(&self) -> Vec<PathBuf> {
        lock(&self.calls).clone()
    }

    /// How many callers are blocked in [`FakeLock::hold`].
    pub fn waiting(&self) -> usize {
        lock(&self.state).waiting
    }
}

impl Default for FakeLock {
    fn default() -> Self {
        Self::new()
    }
}

impl PathLock for FakeLock {
    fn hold(&self, path: &Path, run: &mut dyn FnMut()) {
        lock(&self.calls).push(path.to_path_buf());
        let mut state = lock(&self.state);
        while state.held.contains(path) {
            state.waiting += 1;
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.waiting = state.waiting.saturating_sub(1);
        }
        state.held.insert(path.to_path_buf());
        drop(state);
        run();
        let mut state = lock(&self.state);
        state.held.remove(path);
        self.changed.notify_all();
    }

    fn hold_all(&self, paths: &[PathBuf], run: &mut dyn FnMut()) {
        let mut keys = paths.to_vec();
        keys.sort();
        keys.dedup();
        nest(self, &keys, run);
    }
}

/// Runs `run` while holding the fake's lock on each of `keys`: the first
/// key is held, then the rest inside it, so two renames never deadlock.
fn nest(lock: &FakeLock, keys: &[PathBuf], run: &mut dyn FnMut()) {
    match keys.split_first() {
        None => run(),
        Some((first, rest)) => {
            let mut next = || nest(lock, rest, run);
            lock.hold(first, &mut next);
        }
    }
}

#[cfg(test)]
#[path = "locks_tests.rs"]
mod tests;
