//! The cancel signal a call watches (`docs/tools.md`, "Cancellation"): one
//! flag and the wakers it wakes on cancel.

use std::sync::{Arc, Mutex, Weak};

use crate::clock::Wake;
use crate::lock;

/// What a tool may ask of the signal that cancels its call
/// (`docs/tools.md`, "Cancellation").
pub trait Cancel: Send + Sync {
    /// Whether the call has been cancelled.
    fn is_cancelled(&self) -> bool;

    /// Wakes `waker` when the call is cancelled from then on. A cancel
    /// before the subscription is not replayed: the caller checks
    /// [`Cancel::is_cancelled`] after subscribing.
    fn subscribe(&self, waker: Weak<dyn Wake>);
}

#[derive(Default)]
struct Inner {
    cancelled: bool,
    wakers: Vec<Weak<dyn Wake>>,
}

/// The shared cancel signal: one flag and the wakers it wakes on cancel.
/// Clones share one signal. `cancel` is idempotent: a second call changes
/// nothing.
#[derive(Clone, Default)]
pub struct Token {
    inner: Arc<Mutex<Inner>>,
}

impl Token {
    /// A signal that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every clone. Wakes subscribers registered before this call,
    /// after releasing the token's lock, so a waker may read the token. A
    /// subscriber registered after it is not woken: the caller checks
    /// [`Cancel::is_cancelled`].
    pub fn cancel(&self) {
        let wakers = {
            let mut inner = lock(&self.inner);
            if inner.cancelled {
                return;
            }
            inner.cancelled = true;
            std::mem::take(&mut inner.wakers)
        };
        for waker in wakers.into_iter().filter_map(|waker| waker.upgrade()) {
            waker.wake();
        }
    }
}

impl Cancel for Token {
    fn is_cancelled(&self) -> bool {
        lock(&self.inner).cancelled
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        let mut inner = lock(&self.inner);
        // A subscriber that dropped its waker leaves a dead entry: pruned
        // here, so the list holds only live wakers. Liveness is the strong
        // count, not an upgrade, so no strong reference is dropped under
        // this lock.
        inner.wakers.retain(|listed| Weak::strong_count(listed) > 0);
        if inner.cancelled {
            return;
        }
        inner.wakers.push(waker);
    }
}

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod tests;
