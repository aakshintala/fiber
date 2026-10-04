//! The per-path lock (`docs/architecture.md`, "Tool calls in a step").
//!
//! One set of held paths, not one entry per path: a guard's drop removes its
//! path, so nothing is left behind after a call.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

/// Blocks file-mutating tools that name the same resolved path.
///
/// An extension's tool takes the same lock. Waiting is not cancelled: the
/// holder's write is bounded.
pub struct PathLocks {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    held: BTreeSet<PathBuf>,
    /// Callers blocked in [`PathLocks::lock`], raised before they wait.
    waiting: usize,
}

impl PathLocks {
    /// No path is held.
    #[allow(
        clippy::new_without_default,
        reason = "extensions construct the lock with PathLocks::new"
    )]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                held: BTreeSet::new(),
                waiting: 0,
            }),
            changed: Condvar::new(),
        }
    }

    /// Blocks until `path` is free, then holds it until the guard drops.
    ///
    /// `path` is the resolved path. Two different paths do not block each other.
    pub fn lock(&self, path: &Path) -> PathGuard<'_> {
        let key = path.to_path_buf();
        let mut state = guard(&self.state);
        while state.held.contains(&key) {
            state.waiting += 1;
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            state.waiting = state.waiting.saturating_sub(1);
        }
        state.held.insert(key.clone());
        PathGuard {
            locks: self,
            path: key,
        }
    }

    /// How many callers are blocked in [`PathLocks::lock`].
    ///
    /// Raised before the wait, so a test can observe that a second lock is
    /// blocked rather than sleeping.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        guard(&self.state).waiting
    }

    /// No path is held and nobody is waiting.
    #[cfg(test)]
    pub(crate) fn is_clear(&self) -> bool {
        let state = guard(&self.state);
        state.held.is_empty() && state.waiting == 0
    }
}

/// Holds one path. Drop releases it.
#[must_use = "dropping the guard releases the path"]
pub struct PathGuard<'a> {
    locks: &'a PathLocks,
    path: PathBuf,
}

impl Drop for PathGuard<'_> {
    fn drop(&mut self) {
        let mut state = guard(&self.locks.state);
        state.held.remove(&self.path);
        self.locks.changed.notify_all();
    }
}

fn guard(mutex: &Mutex<State>) -> MutexGuard<'_, State> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "locks_tests.rs"]
mod tests;
