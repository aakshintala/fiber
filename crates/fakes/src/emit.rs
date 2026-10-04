//! An [`Emit`] that records every ephemeral event a test tool is given
//! (`docs/tools.md`, "Progress"). A durable event is ignored, as [`Emit`]
//! says, and nothing is written for it.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use contract::emit::Emit;
use contract::events::{Class, Event};

#[derive(Default)]
struct Inner {
    events: Vec<Event>,
}

/// An [`Emit`] a test hands a tool's `run`: it records every ephemeral
/// event, ignoring durable ones.
#[derive(Default)]
pub struct Recorder {
    inner: Mutex<Inner>,
    changed: Condvar,
}

impl Recorder {
    /// Every ephemeral event recorded so far, in arrival order.
    pub fn events(&self) -> Vec<Event> {
        lock(&self.inner).events.clone()
    }

    /// The concatenated `text` of every `tool_call_delta` recorded so far.
    pub fn text(&self) -> String {
        let inner = lock(&self.inner);
        let mut out = String::new();
        for event in &inner.events {
            if let Event::ToolCallDelta(progress) = event
                && let Some(text) = &progress.text
            {
                out.push_str(text);
            }
        }
        out
    }

    /// Waits, at most `within` of real time, until the recorded delta text
    /// contains `needle`. True once it does; false at the deadline. The
    /// bound is the condvar's timeout, so this reads no process clock.
    pub fn wait_for_text(&self, needle: &str, within: Duration) -> bool {
        let inner = lock(&self.inner);
        let (inner, _) = self
            .changed
            .wait_timeout_while(inner, within, |inner| !has_text(inner, needle))
            .unwrap_or_else(PoisonError::into_inner);
        has_text(&inner, needle)
    }
}

impl Emit for Recorder {
    fn emit(&self, event: &Event) {
        if event.class() != Class::Ephemeral {
            return;
        }
        lock(&self.inner).events.push(event.clone());
        self.changed.notify_all();
    }
}

fn has_text(inner: &Inner, needle: &str) -> bool {
    inner.events.iter().any(|event| {
        if let Event::ToolCallDelta(progress) = event {
            progress
                .text
                .as_deref()
                .is_some_and(|text| text.contains(needle))
        } else {
            false
        }
    })
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "emit_tests.rs"]
mod tests;
