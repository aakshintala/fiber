//! A door that admits until it is closed, once and for good.

use std::sync::Mutex;

/// A door that admits until it is closed, once and for good. What enters
/// enters under the lock, so a closer that looks a last time under the same
/// lock sees everything admitted before it closed.
#[derive(Debug, Default)]
pub struct Admission(Mutex<bool>);

impl Admission {
    /// Runs `send` under the lock and returns its value; `None`, not calling it, once closed.
    pub fn admit<T>(&self, send: impl FnOnce() -> T) -> Option<T> {
        let closed = crate::lock(&self.0);
        if *closed {
            return None;
        }
        Some(send())
    }

    /// Closes. Idempotent.
    pub fn close(&self) {
        *crate::lock(&self.0) = true;
    }

    /// Runs `last` under the lock, whether or not already closed, and
    /// returns its value; when it returns `None`, closes before the lock is
    /// released.
    pub fn close_unless<T>(&self, last: impl FnOnce() -> Option<T>) -> Option<T> {
        let mut closed = crate::lock(&self.0);
        let found = last();
        if found.is_none() {
            *closed = true;
        }
        found
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
