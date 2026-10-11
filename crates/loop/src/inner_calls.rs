//! Inner calls run inside their anchor on the loop's step machinery.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};

use contract::ActionId;
use contract::clock::Wake;
use contract::events::{CallStatus, ToolCallCompleted};
use contract::tool::{Cancel, InnerCalls};
use serde_json::Value;

use crate::TurnCancel;
use crate::progress::SharedWake;

pub(crate) const MAX_INNER_DEPTH: u8 = 8;

/// One response shared by the requester, the loop and the closing slot.
#[derive(Default)]
struct Answer {
    completed: Mutex<Option<Box<ToolCallCompleted>>>,
    changed: Condvar,
}

impl Answer {
    fn resolve(&self, completed: Box<ToolCallCompleted>) {
        let mut answer = lock(&self.completed);
        if answer.is_none() {
            *answer = Some(completed);
        }
        self.changed.notify_all();
    }

    fn wait(&self) -> Box<ToolCallCompleted> {
        let mut answer = lock(&self.completed);
        loop {
            if let Some(completed) = answer.take() {
                return completed;
            }
            answer = self.changed.wait(answer).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A request not yet taken by the loop thread.
pub(crate) struct Raised {
    pub(crate) extension: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
    answer: Arc<Answer>,
}

impl Raised {
    pub(crate) fn answer(self, completed: Box<ToolCallCompleted>) {
        self.answer.resolve(completed);
    }
}

#[derive(Default)]
struct Requests {
    raised: Vec<Raised>,
    waiting: Vec<Arc<Answer>>,
    closed: bool,
}

/// The runner for a stream's inner calls. Raising and closing share one lock.
pub(crate) struct InnerSlot {
    pub(crate) action: ActionId,
    tool: String,
    pub(crate) depth: u8,
    wake: Arc<SharedWake>,
    requests: Mutex<Requests>,
}

impl InnerSlot {
    pub(crate) fn new(wake: Arc<SharedWake>, action: ActionId, tool: String, depth: u8) -> Self {
        Self { action, tool, depth, wake, requests: Mutex::default() }
    }

    pub(crate) fn take_raised(&self) -> Vec<Raised> {
        std::mem::take(&mut lock(&self.requests).raised)
    }

    pub(crate) fn close(&self) {
        let mut requests = lock(&self.requests);
        requests.closed = true;
        requests.raised.clear();
        for answer in std::mem::take(&mut requests.waiting) {
            answer.resolve(cancelled());
        }
    }
}

impl InnerCalls for InnerSlot {
    fn run(&self, extension: &str, name: &str, arguments: Value) -> Result<Box<ToolCallCompleted>, String> {
        if name == self.tool {
            return Err(format!("host.tool: the tool `{name}` cannot run itself"));
        }
        if self.depth >= MAX_INNER_DEPTH {
            return Err(format!("host.tool: inner-call depth exceeds {MAX_INNER_DEPTH}"));
        }
        let answer = Arc::new(Answer::default());
        {
            let mut requests = lock(&self.requests);
            if requests.closed {
                return Ok(cancelled());
            }
            requests.waiting.push(Arc::clone(&answer));
            requests.raised.push(Raised {
                extension: extension.to_owned(), name: name.to_owned(), arguments,
                answer: Arc::clone(&answer),
            });
        }
        self.wake.wake();
        let completed = answer.wait();
        lock(&self.requests).waiting.retain(|held| !Arc::ptr_eq(held, &answer));
        Ok(completed)
    }
}

fn cancelled() -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted { status: CallStatus::Cancelled, ..crate::completion::completed(String::new(), None) })
}

#[derive(Default)]
struct Signal {
    inner: Mutex<(bool, Vec<Weak<dyn Wake>>)>,
}

impl Signal {
    fn cancel(&self) {
        let wakers = {
            let mut inner = lock(&self.inner);
            inner.0 = true;
            std::mem::take(&mut inner.1)
        };
        for waker in wakers.into_iter().filter_map(|waker| waker.upgrade()) {
            waker.wake();
        }
    }
}

impl Wake for Signal {
    fn wake(&self) { self.cancel(); }
}

/// Cancelled by the turn or by its anchor ending, without cancelling the anchor.
pub(crate) struct InnerCancel {
    turn: Arc<TurnCancel>,
    signal: Arc<Signal>,
}

impl InnerCancel {
    pub(crate) fn new(turn: Arc<TurnCancel>) -> Self {
        let signal = Arc::new(Signal::default());
        let relay: Arc<dyn Wake> = signal.clone();
        turn.subscribe(Arc::downgrade(&relay));
        if turn.is_cancelled() { signal.cancel(); }
        Self { turn, signal }
    }

    pub(crate) fn cancel(&self) { self.signal.cancel(); }
}

impl Cancel for InnerCancel {
    fn is_cancelled(&self) -> bool { lock(&self.signal.inner).0 || self.turn.is_cancelled() }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        let mut inner = lock(&self.signal.inner);
        if !inner.0 { inner.1.push(waker); }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "inner_calls_tests.rs"]
mod tests;
