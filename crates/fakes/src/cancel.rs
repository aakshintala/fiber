//! A cancel signal a test or a jig can fire (`docs/tools.md`, "Cancellation").

use std::sync::PoisonError;
use std::sync::{Arc, Mutex, Weak};

use contract::clock::Wake;
use contract::tool::Cancel;

#[derive(Default)]
struct Inner {
    cancelled: bool,
    wakers: Vec<Weak<dyn Wake>>,
}

/// A [`Cancel`] a test fires with [`CancelToken::cancel`]. Clones share one
/// signal. `cancel` is idempotent: a second call changes nothing.
#[derive(Clone, Default)]
pub struct CancelToken {
    inner: Arc<Mutex<Inner>>,
}

impl CancelToken {
    /// A signal that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every clone. Wakes subscribers registered before this call.
    /// A subscriber registered after it is not woken: the caller checks
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

impl Cancel for CancelToken {
    fn is_cancelled(&self) -> bool {
        lock(&self.inner).cancelled
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        lock(&self.inner).wakers.push(waker);
    }
}

fn lock(inner: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod tests;
