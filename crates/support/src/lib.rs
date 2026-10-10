//! Small shared mechanisms with no domain knowledge: the process-group guard
//! and the poison-ignoring `lock` (`docs/architecture.md`, "The modules").

use std::sync::{Mutex, MutexGuard};

/// The process-group guard, the process-wide list and the signals
/// (`docs/testing.md`, "Running tests").
pub mod group;

/// Locks `mutex`, returning the guard even when another thread panicked
/// while holding it: a poisoned lock still guards live state.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
