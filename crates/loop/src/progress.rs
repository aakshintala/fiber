//! A running call's streamed output (`docs/tools.md`, "Progress"): the
//! pacer holds a call's changes and says when the held change is due, one
//! stream wires each running call to the loop thread, which writes every
//! `tool_call_delta`, and the shared wake is what that wait sleeps on.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{Event, Progress};
use contract::tool::Output;
use serde_json::Value;

/// At most one delta every 100 ms (`docs/tools.md`, "Progress").
const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// The byte rate the interval grows past the minimum with: 100 KiB/s
/// (`docs/tools.md`, "Progress").
const BYTES_PER_SECOND: u64 = 102_400;

/// What one running call holds: its changes since the last written delta.
#[derive(Debug, Default)]
struct Held {
    /// Output added since the last delta, in arrival order.
    text: String,
    /// The latest progress a delta carried, shaped as the tool chooses.
    details: Option<Value>,
}

/// Paces one running call's `tool_call_delta` lines (`docs/tools.md`,
/// "Progress"). Pure: every instant comes from the caller, which reads the
/// loop's clock, so a test builds them from one origin and the pacer never
/// reads a clock.
#[derive(Debug, Default)]
pub(crate) struct Pacer {
    held: Option<Held>,
    /// When the held change may next be written. `None` is idle: nothing
    /// was written yet, so the first change goes out at once.
    next_due: Option<Instant>,
}

impl Pacer {
    /// Merges `delta` into the held change: texts concatenate in order, so
    /// no output is lost, and `details` is the latest one a delta carried.
    /// A delta with neither is ignored: it carries nothing.
    pub(crate) fn hold(&mut self, delta: &Progress) {
        if delta.text.is_none() && delta.details.is_none() {
            return;
        }
        let held = self.held.get_or_insert_with(Held::default);
        if let Some(text) = &delta.text {
            held.text.push_str(text);
        }
        if delta.details.is_some() {
            held.details = delta.details.clone();
        }
    }

    /// The held change when it is due at `now`: the first change after
    /// idle, or once `now` has reached what the last write cost. Takes it,
    /// so a second call without a new `hold` finds nothing.
    pub(crate) fn take_due(&mut self, now: Instant) -> Option<Progress> {
        if self.held.is_some() && self.next_due.is_none_or(|due| now >= due) {
            self.take_final()
        } else {
            None
        }
    }

    /// The held change whatever the interval: the final flush a call's end
    /// writes before its `tool_call_completed` (`docs/tools.md`,
    /// "Progress"). `None` when nothing is held.
    pub(crate) fn take_final(&mut self) -> Option<Progress> {
        self.held.take().map(|held| Progress {
            text: (!held.text.is_empty()).then_some(held.text),
            details: held.details,
        })
    }

    /// Records a write of `bytes` encoded bytes at `at`: the next delta is
    /// due at `at + max(100 ms, bytes ÷ 100 KiB/s)` (`docs/tools.md`,
    /// "Progress"). `bytes` is the written delta's payload serialised as
    /// JSON, measured after the write.
    pub(crate) fn wrote(&mut self, bytes: u64, at: Instant) {
        let paced = Duration::from_nanos(bytes.saturating_mul(1_000_000_000) / BYTES_PER_SECOND);
        let interval = paced.max(MIN_INTERVAL);
        self.next_due = at.checked_add(interval).or(Some(at));
    }

    /// When the held change is next due: what the loop waits until. `None`
    /// when nothing is held, or when the held change is due at once after
    /// idle, so the loop parks until woken instead of waiting on the clock.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.held.as_ref().and(self.next_due)
    }
}

/// Wakes the loop thread waiting in `run_calls`: one flag under one mutex,
/// set by every emit, every call returning and every clock move, and
/// checked across the wait, so a bump between the check and the wait is
/// seen (`docs/tools.md`, "Progress"; Ruling 17). The flag only ever asks
/// for another pass, so clearing it on waking loses nothing.
#[derive(Debug, Default)]
pub(crate) struct SharedWake {
    inner: Mutex<bool>,
    cv: Condvar,
}

impl SharedWake {
    /// Parks until `until` on `clock`, an emit, a call returning or a clock
    /// move. When the flag is set, a bump landed since the last pass and
    /// the wait returns at once, clearing it; otherwise the flag is
    /// checked under the same mutex the condvar wait releases, so a bump
    /// that lands before the wait still returns at once, and the flag is
    /// cleared on waking (`crates/tools/src/shell/drive.rs` `park` does
    /// the same).
    pub(crate) fn park(&self, clock: &dyn Clock, until: Option<Instant>) {
        // Taken before `wait_until`, and held until the condvar wait, so a
        // bump blocks on this lock instead of notifying nobody. `FnMut`
        // cannot move the guard out and back; the slot holds it across the
        // one call.
        let mut slot = Some(lock(&self.inner));
        clock.wait_until(until, &mut |bound| {
            let Some(mut guard) = slot.take() else {
                return;
            };
            if *guard {
                *guard = false;
                slot = Some(guard);
                return;
            }
            guard = match bound {
                Some(timeout) => {
                    self.cv
                        .wait_timeout(guard, timeout)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.cv.wait(guard).unwrap_or_else(PoisonError::into_inner),
            };
            *guard = false;
            slot = Some(guard);
        });
    }

    /// Sets the flag and wakes whoever parks on it.
    fn bump(&self) {
        *lock(&self.inner) = true;
        self.cv.notify_all();
    }
}

impl Wake for SharedWake {
    fn wake(&self) {
        self.bump();
    }
}

/// What one running call carries across emits: its pacer and, once its
/// tool returns, its output.
#[derive(Debug, Default)]
struct StreamInner {
    pacer: Pacer,
    done: Option<Output>,
}

/// One running call's stream: the emitter the loop hands the call's own
/// thread (`docs/architecture.md`, "Streaming"). Its [`Emit`] merges only
/// `tool_call_delta` into the call's pacer and wakes the loop thread; every
/// other event is ignored, since it would not belong under the call's
/// action. The loop thread takes what is due, flushes what is held when the
/// call returns, and takes the returned output, each under one lock.
#[derive(Debug)]
pub(crate) struct Stream {
    inner: Mutex<StreamInner>,
    wake: Arc<SharedWake>,
}

impl Stream {
    /// A stream waking `wake`.
    pub(crate) fn new(wake: Arc<SharedWake>) -> Self {
        Self {
            inner: Mutex::new(StreamInner {
                pacer: Pacer::default(),
                done: None,
            }),
            wake,
        }
    }

    /// The delta due at `now`, if any, taking it.
    pub(crate) fn take_due(&self, now: Instant) -> Option<Progress> {
        lock(&self.inner).pacer.take_due(now)
    }

    /// Records a write of `bytes` encoded bytes at `at`.
    pub(crate) fn wrote(&self, bytes: u64, at: Instant) {
        lock(&self.inner).pacer.wrote(bytes, at);
    }

    /// When the held change is next due, if it waits on the clock.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        lock(&self.inner).pacer.deadline()
    }

    /// Takes the held change if (and only if) the call has returned: the
    /// final flush, written the moment its tool returns whatever the
    /// interval (`docs/tools.md`, "Progress"). `None` while the call runs
    /// or when nothing is held.
    pub(crate) fn take_flush(&self) -> Option<Progress> {
        let mut inner = lock(&self.inner);
        inner.done.as_ref()?;
        inner.pacer.take_final()
    }

    /// Takes the returned output together with whatever is held, if the call
    /// has returned: one lock, so the final flush can never slip past the
    /// completion it precedes (`docs/tools.md`, "Progress").
    pub(crate) fn take_finished(&self) -> Option<(Output, Option<Progress>)> {
        let mut inner = lock(&self.inner);
        let output = inner.done.take()?;
        Some((output, inner.pacer.take_final()))
    }

    /// Marks the call returned with `output`, waking the loop thread. The
    /// call's thread runs this once, right after its `run` returns, so
    /// nothing emits after it.
    pub(crate) fn finish(&self, output: Output) {
        lock(&self.inner).done = Some(output);
        self.wake.bump();
    }
}

impl Emit for Stream {
    fn emit(&self, event: &Event) {
        if let Event::ToolCallDelta(delta) = event {
            lock(&self.inner).pacer.hold(delta);
        }
        self.wake.bump();
    }
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
