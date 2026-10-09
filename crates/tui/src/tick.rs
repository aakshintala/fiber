//! The working line's timer (`docs/tui.md`, "The working line"): a gate the
//! loop arms with the next frame's deadline, and a thread that sends one
//! [`Input::Tick`] when it passes on the injected clock. With nothing
//! armed the thread waits on its condvar, so a still screen runs no
//! timer. At most one tick is ever in the channel: each send waits for
//! the loop's ack.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use contract::clock::{Clock, Wake};

use crate::Input;

/// The frames' step: spinners, the glimmer and the pulse all move on it
/// (`docs/tui.md`, "The working line").
pub(crate) const TICK: Duration = Duration::from_millis(120);

/// What the gate says to do, decided pure in [`Gate::step`].
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// The loop is gone: the thread ends.
    Quit,
    /// Nothing armed, or a tick unacked: wait for a notify, on no clock.
    Idle,
    /// The deadline passed: send one tick.
    Send,
    /// Wait for the deadline on the clock.
    Until(std::time::Instant),
}

/// What the loop armed, under one lock.
struct Gate {
    /// The next frame's deadline, until the thread takes it.
    at: Option<std::time::Instant>,
    /// A tick is in the channel, waiting for the loop's ack.
    in_flight: bool,
    /// The loop is gone: the thread ends. Never cleared.
    quit: bool,
}

impl Gate {
    /// What the thread does next. Calls `now` only when armed and with no
    /// tick out: disarmed and in-flight passes ask for no time.
    fn step(&mut self, now: impl FnOnce() -> std::time::Instant) -> Step {
        if self.quit {
            return Step::Quit;
        }
        let Some(at) = self.at else {
            return Step::Idle;
        };
        if self.in_flight {
            return Step::Idle;
        }
        if now() >= at {
            self.at = None;
            self.in_flight = true;
            Step::Send
        } else {
            Step::Until(at)
        }
    }
}

/// The loop's side of the timer: armed with the next frame's deadline,
/// acked once its tick is handled.
pub(crate) struct Ticker {
    gate: Mutex<Gate>,
    changed: Condvar,
}

impl Wake for Ticker {
    /// The clock moved. Taking the lock first means a thread between its
    /// clock check and its condvar wait holds it, so the notify reaches
    /// that thread once it waits.
    fn wake(&self) {
        drop(self.lock());
        self.changed.notify_all();
    }
}

impl Ticker {
    /// A ticker with no deadline armed, woken whenever `clock` moves.
    /// With no clock nothing wakes it: the idle loop's arm only records.
    pub(crate) fn new(clock: Option<&Arc<dyn Clock>>) -> Arc<Self> {
        let ticker = Arc::new(Self {
            gate: Mutex::new(Gate {
                at: None,
                in_flight: false,
                quit: false,
            }),
            changed: Condvar::new(),
        });
        if let Some(clock) = clock {
            let waker: Weak<Self> = Arc::downgrade(&ticker);
            clock.subscribe(waker);
        }
        ticker
    }

    /// Arms the next frame's deadline, replacing the last; `None`
    /// disarms. Never touches `in_flight`: a tick in the channel still
    /// waits for its ack.
    pub(crate) fn arm(&self, at: Option<std::time::Instant>) {
        self.lock().at = at;
        self.changed.notify_all();
    }

    /// The loop handled the tick in the channel: the next deadline armed
    /// sends again.
    pub(crate) fn ack(&self) {
        self.lock().in_flight = false;
        self.changed.notify_all();
    }

    /// Ends the thread's wait, now and for every later one. Never
    /// cleared: a dropped loop still ends its thread.
    pub(crate) fn quit(&self) {
        self.lock().quit = true;
        self.changed.notify_all();
    }

    /// Sends one [`Input::Tick`] per armed deadline passed on `clock`,
    /// until [`Ticker::quit`] or the channel's end. A deadline already
    /// past sends at once; with nothing armed, or a tick unacked, no
    /// clock is read.
    fn run(&self, clock: &dyn Clock, tx: &Sender<Input>) {
        let mut gate = self.lock();
        loop {
            match gate.step(|| clock.now()) {
                Step::Quit => return,
                Step::Idle => {
                    gate = self
                        .changed
                        .wait(gate)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                Step::Send => {
                    // The ack the send waits for needs the lock, so it
                    // is released while sending.
                    drop(gate);
                    if tx.send(Input::Tick).is_err() {
                        // The loop is gone: nothing will ack, so end.
                        return;
                    }
                    gate = self.lock();
                }
                Step::Until(at) => {
                    // The lock is held into `wait_until`, so a wake
                    // blocks on it until the condvar wait releases it,
                    // and is never missed.
                    let mut slot = Some(gate);
                    clock.wait_until(Some(at), &mut |bound| {
                        let Some(held) = slot.take() else {
                            return;
                        };
                        slot = Some(match bound {
                            Some(bound) => {
                                self.changed
                                    .wait_timeout(held, bound)
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .0
                            }
                            None => self
                                .changed
                                .wait(held)
                                .unwrap_or_else(PoisonError::into_inner),
                        });
                    });
                    gate = match slot {
                        Some(held) => held,
                        None => self.lock(),
                    };
                }
            }
        }
    }

    /// The armed deadline, if any.
    #[cfg(test)]
    fn armed(&self) -> Option<std::time::Instant> {
        self.lock().at
    }

    /// The gate, even after a thread panicked holding it.
    fn lock(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The loop's timer: a ticker and the thread sending its ticks.
pub(crate) struct TickThread {
    ticker: Arc<Ticker>,
    thread: Option<JoinHandle<()>>,
}

impl TickThread {
    /// A ticker with no clock and no thread: an arm only records its
    /// deadline, and nothing sends. Test `Loop` literals use it.
    pub(crate) fn idle() -> Self {
        Self {
            ticker: Ticker::new(None),
            thread: None,
        }
    }

    /// Starts the thread sending this ticker's ticks to `tx`, on `clock`.
    /// Subscribes first, so an idle ticker wakes on the clock only once
    /// started. A spawn that fails leaves no thread: arms only record.
    pub(crate) fn start(&mut self, clock: Arc<dyn Clock>, tx: Sender<Input>) {
        let waker: Weak<Ticker> = Arc::downgrade(&self.ticker);
        clock.subscribe(waker);
        let ticker = Arc::clone(&self.ticker);
        if let Ok(thread) =
            crate::sources::builder("tui-tick").spawn(move || ticker.run(clock.as_ref(), &tx))
        {
            self.thread = Some(thread);
        }
    }

    /// Arms the next frame's deadline; `None` disarms.
    pub(crate) fn arm(&self, at: Option<std::time::Instant>) {
        self.ticker.arm(at);
    }

    /// The loop handled the tick in the channel.
    pub(crate) fn ack(&self) {
        self.ticker.ack();
    }

    /// Ends the thread and waits for it. Releases nothing else first:
    /// the thread waits on the clock and the condvar only.
    pub(crate) fn stop(&mut self) {
        self.ticker.quit();
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }

    /// The armed deadline, or the last arm while idle: what the loop
    /// armed without a thread.
    #[cfg(test)]
    pub(crate) fn armed(&self) -> Option<std::time::Instant> {
        self.ticker.armed()
    }
}

/// Quitting only: a `Loop` dropped early never blocks on its thread. The
/// loop's own `Drop` is unchanged.
impl Drop for TickThread {
    fn drop(&mut self) {
        self.ticker.quit();
    }
}

#[cfg(test)]
#[path = "tick_tests.rs"]
mod tests;
