//! A model call that blocks until cancelled, for cancel tests: the loop
//! cancels it through the waker its helper subscribes, and the test fires
//! the cancel once the call signals it started to block.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use contract::provider::{CallError, Delta, ModelCall, ModelRequest, Provider, Reply};

use crate::reply;

/// How long a blocked call waits for its cancel before giving up and
/// reporting `cancelled` anyway. Every test cancels promptly; the loop's
/// own deadline reports first when one does not, and this bound keeps the
/// call's thread from outliving the test binary by much.
const LIMIT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Inner {
    started: bool,
    cancelled: bool,
}

/// A provider whose first call signals when it starts to block and ends
/// `cancelled` once cancelled. Later calls answer "After." at once, so a
/// turn the cancel starts runs to completion.
#[derive(Clone, Default)]
pub struct BlockingProvider {
    inner: Arc<(Mutex<Inner>, Condvar)>,
    calls: Arc<AtomicUsize>,
}

impl BlockingProvider {
    /// Blocks until the first call starts to block, failing the test at
    /// `timeout` naming the missing call. The test fires its cancel after
    /// this, so the cancel lands mid-stream.
    pub fn wait_started(&self, timeout: Duration) {
        let (lock, changed) = &*self.inner;
        let started = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (started, _) = changed
            .wait_timeout_while(started, timeout, |started| !started.started)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            started.started,
            "timed out waiting for the model call to start"
        );
    }
}

impl Provider for BlockingProvider {
    fn call(&self, _request: &ModelRequest) -> Box<dyn ModelCall> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Box::new(BlockingCall {
                inner: Arc::clone(&self.inner),
            })
        } else {
            Box::new(Answer)
        }
    }
}

/// The first call: signals, then waits for its cancel.
struct BlockingCall {
    inner: Arc<(Mutex<Inner>, Condvar)>,
}

impl ModelCall for BlockingCall {
    fn run(&self, _sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let (lock, changed) = &*self.inner;
        {
            let mut inner = lock.lock().unwrap_or_else(PoisonError::into_inner);
            inner.started = true;
            // A cancel before `run` still ends the call at once below.
            changed.notify_all();
        }
        let (guard, _) = changed
            .wait_timeout_while(
                lock.lock().unwrap_or_else(PoisonError::into_inner),
                LIMIT,
                |inner| !inner.cancelled,
            )
            .unwrap_or_else(PoisonError::into_inner);
        // A call the cancel never reached is a missed signal, not a
        // cancellation: it must not report `Cancelled` after its timeout.
        assert!(guard.cancelled, "timed out waiting for the call's cancel");
        Err(CallError::Cancelled)
    }

    fn cancel(&self) {
        let (lock, changed) = &*self.inner;
        lock.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled = true;
        changed.notify_all();
    }
}

/// Every later call: answers "After." at once.
struct Answer;

impl ModelCall for Answer {
    fn run(&self, _sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        Ok(reply("After."))
    }

    fn cancel(&self) {}
}

#[cfg(test)]
#[path = "blocking_tests.rs"]
mod tests;
