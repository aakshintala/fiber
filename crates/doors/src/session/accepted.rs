//! Owns the ids of the commands this session accepted, so a repeated id is
//! rejected `duplicate_command`.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use contract::CommandId;

/// The ids this process accepted or is running, across connections.
pub(crate) struct Accepted {
    ids: Mutex<HashSet<String>>,
}

impl Accepted {
    pub(crate) fn new() -> Self {
        Self {
            ids: Mutex::new(HashSet::new()),
        }
    }

    /// Claims `id` before its command is dispatched. False when an earlier
    /// command holds it, running or accepted.
    pub(crate) fn reserve(&self, id: &CommandId) -> bool {
        lock(&self.ids).insert(id.0.clone())
    }

    /// Frees `id` once its command is rejected, so a client may retry it.
    pub(crate) fn release(&self, id: &CommandId) {
        lock(&self.ids).remove(&id.0);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
