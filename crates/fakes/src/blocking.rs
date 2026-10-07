//! A model call that blocks until cancelled, for cancel tests: the loop
//! cancels it through the waker its helper subscribes, and the test fires
//! the cancel once the call signals it started to block.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use contract::provider::{
    CallError, CallUsage, Delta, InputSize, ModelCall, ModelRequest, Provider, Reply,
};

use crate::reply;

/// How long a blocked call waits for its cancel before giving up and
/// reporting `cancelled` anyway. Every test cancels promptly; the loop's
/// own deadline reports first when one does not, and this bound keeps the
/// call's thread from outliving the test binary by much.
const LIMIT: Duration = Duration::from_secs(30);

/// The two one-shot signals, each buffered: a send before the wait still
/// meets it, so neither side races the other.
struct Channels {
    started_tx: mpsc::Sender<()>,
    started_rx: Mutex<mpsc::Receiver<()>>,
    cancel_tx: mpsc::Sender<()>,
    cancel_rx: Mutex<mpsc::Receiver<()>>,
}

/// A provider whose first call signals when it starts to block and ends
/// `cancelled` once cancelled. Later calls answer "After." at once, so a
/// turn the cancel starts runs to completion.
#[derive(Clone)]
pub struct BlockingProvider {
    inner: Arc<Channels>,
    calls: Arc<AtomicUsize>,
}

impl Default for BlockingProvider {
    fn default() -> Self {
        let (started_tx, started_rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        Self {
            inner: Arc::new(Channels {
                started_tx,
                started_rx: Mutex::new(started_rx),
                cancel_tx,
                cancel_rx: Mutex::new(cancel_rx),
            }),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl BlockingProvider {
    /// Blocks until the first call starts to block, failing the test at
    /// `timeout` naming the missing call. The test fires its cancel after
    /// this, so the cancel lands mid-stream.
    pub fn wait_started(&self, timeout: Duration) {
        let started = self
            .inner
            .started_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let got = started.recv_timeout(timeout);
        assert!(got.is_ok(), "timed out waiting for the model call to start");
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
    inner: Arc<Channels>,
}

impl ModelCall for BlockingCall {
    fn run(&self, _sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        // Buffered, so a cancel before `run` still ends the call at once
        // below.
        let _sent = self.inner.started_tx.send(()).is_ok();
        let cancel = self
            .inner
            .cancel_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let got = cancel.recv_timeout(LIMIT);
        // A call the cancel never reached is a missed signal, not a
        // cancellation: it must not report `Cancelled` after its timeout.
        assert!(got.is_ok(), "timed out waiting for the call's cancel");
        Err(CallError::Cancelled {
            usage: Box::new(CallUsage::unnamed(InputSize::default())),
        })
    }

    fn cancel(&self) {
        let _sent = self.inner.cancel_tx.send(()).is_ok();
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
