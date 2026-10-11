//! Owns the count of `full` connections and whether `clients` lines are
//! sealed, so an emission in flight finishes before `seal` returns.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// The `full` connections, and whether `clients` lines are sealed.
pub(crate) struct Clients {
    state: Mutex<(u32, bool)>,
}

impl Clients {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new((0, false)),
        }
    }

    /// One more `full` connection, emitting the count unless sealed. The
    /// lock is held across `emit`, so an emission in flight finishes before
    /// `seal` returns.
    pub(super) fn attach(&self, emit: impl FnOnce(u32)) {
        let mut state = lock(&self.state);
        state.0 += 1;
        if !state.1 {
            emit(state.0);
        }
    }

    /// One fewer `full` connection, emitting the count unless sealed. The
    /// lock is held across `emit`, as `attach` does.
    pub(super) fn detach(&self, emit: impl FnOnce(u32)) {
        let mut state = lock(&self.state);
        state.0 = state.0.saturating_sub(1);
        if !state.1 {
            emit(state.0);
        }
    }

    /// Seals `clients` lines: no later attach or detach emits one.
    pub(super) fn seal(&self) {
        lock(&self.state).1 = true;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
