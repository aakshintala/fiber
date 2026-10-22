//! [`Recorder`], an [`Emit`] that records every ephemeral event a test
//! tool is given (`docs/tools.md`, "Progress"): a durable event is
//! ignored, as [`Emit`] says. [`EventLog`] instead records every event,
//! ephemeral and durable alike, in arrival order.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use crate::deadline::Deadline;

use contract::emit::Emit;
use contract::events::{Class, Event};

/// An [`Emit`] a test hands a tool's `run`: it records every ephemeral
/// event, ignoring durable ones.
#[derive(Default)]
pub struct Recorder {
    inner: Mutex<Vec<Event>>,
    changed: Condvar,
}

impl Recorder {
    /// Every ephemeral event recorded so far, in arrival order.
    pub fn events(&self) -> Vec<Event> {
        lock(&self.inner).clone()
    }

    /// The concatenated `text` of every `tool_call_delta` recorded so far.
    pub fn text(&self) -> String {
        text_of(&lock(&self.inner))
    }

    /// Waits, at most what remains of `deadline`, until the recorded delta text
    /// contains `needle`. True once it does; false at the deadline. The
    /// bound is the condvar's timeout, so this reads no process clock.
    pub fn wait_for_text(&self, needle: &str, deadline: Deadline) -> bool {
        let within = deadline.left();
        let inner = lock(&self.inner);
        let (inner, _) = self
            .changed
            .wait_timeout_while(inner, within, |inner| !text_of(inner).contains(needle))
            .unwrap_or_else(PoisonError::into_inner);
        text_of(&inner).contains(needle)
    }
}

impl Emit for Recorder {
    fn emit(&self, event: &Event) {
        if event.class() != Class::Ephemeral {
            return;
        }
        lock(&self.inner).push(event.clone());
        self.changed.notify_all();
    }
}

/// An [`Emit`] a test hands an emitter: it records every event without
/// looking at its class, so durable events are kept as well as ephemeral
/// ones. Unlike [`Recorder`] it cannot wait for text; [`EventLog::channel`]
/// also hands each event to a receiver as it arrives.
#[derive(Default)]
pub struct EventLog {
    inner: Mutex<Vec<Event>>,
    tx: Option<mpsc::Sender<Event>>,
}

impl EventLog {
    /// A log that also sends each event to the returned receiver, as it
    /// arrives. A dropped receiver does not stop recording.
    pub fn channel() -> (Self, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let log = Self {
            inner: Mutex::new(Vec::new()),
            tx: Some(tx),
        };
        (log, rx)
    }

    /// Every event recorded so far, in arrival order.
    pub fn events(&self) -> Vec<Event> {
        lock(&self.inner).clone()
    }
}

impl Emit for EventLog {
    fn emit(&self, event: &Event) {
        let mut inner = lock(&self.inner);
        inner.push(event.clone());
        if let Some(tx) = &self.tx {
            let _sent = tx.send(event.clone());
        }
    }
}

/// The concatenated `text` of every `tool_call_delta` in `events`.
fn text_of(events: &[Event]) -> String {
    let mut out = String::new();
    for event in events {
        if let Event::ToolCallDelta(progress) = event
            && let Some(text) = &progress.text
        {
            out.push_str(text);
        }
    }
    out
}

fn lock(inner: &Mutex<Vec<Event>>) -> MutexGuard<'_, Vec<Event>> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "emit_tests.rs"]
mod tests;
