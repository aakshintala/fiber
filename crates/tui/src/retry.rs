//! The hub thread's wait between connection attempts (`docs/tui.md`, "A
//! dropped connection"): the loop gives one delay per failure, and the
//! thread waits it out on the injected clock before it connects again. No
//! delay given means no attempt: a refused schema never retries, and with
//! no timer the idle terminal does nothing ("Performance").

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use contract::clock::{Clock, Wake};

/// The permit between the loop and the hub thread.
pub(crate) struct Retry {
    gate: Mutex<Gate>,
    changed: Condvar,
}

/// What the loop gave, under one lock.
struct Gate {
    /// The delay before the next attempt, until the thread takes it.
    delay: Option<Duration>,
    /// The loop is gone: the thread ends. Never cleared.
    quit: bool,
    /// The hub thread's current stream, to shut down on quit: quitting
    /// before the loop adopts the connection still ends its read
    /// (`docs/tui.md`, "A dropped connection").
    watched: Option<UnixStream>,
}

impl Wake for Retry {
    /// The clock moved. Taking the lock first means a thread between its
    /// clock check and its condvar wait holds it, so the notify reaches
    /// that thread once it waits.
    fn wake(&self) {
        drop(self.lock());
        self.changed.notify_all();
    }
}

impl Retry {
    /// A permit with no delay given, woken whenever `clock` moves.
    pub(crate) fn new(clock: &Arc<dyn Clock>) -> Arc<Self> {
        let retry = Arc::new(Self {
            gate: Mutex::new(Gate {
                delay: None,
                quit: false,
                watched: None,
            }),
            changed: Condvar::new(),
        });
        let waker: Weak<Self> = Arc::downgrade(&retry);
        clock.subscribe(waker);
        retry
    }

    /// Lets the thread try again once `delay` has passed.
    pub(crate) fn give(&self, delay: Duration) {
        self.lock().delay = Some(delay);
        self.changed.notify_all();
    }

    /// Ends the thread's wait, now and for every later one, and shuts
    /// down the stream it watches, if any, so a read started before the
    /// loop adopted the connection still ends.
    pub(crate) fn quit(&self) {
        let watched = {
            let mut gate = self.lock();
            gate.quit = true;
            std::mem::take(&mut gate.watched)
        };
        if let Some(stream) = watched {
            stream.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
        self.changed.notify_all();
    }

    /// Watches `stream` for [`Retry::quit`]: `Ok(false)` when the loop
    /// already quit, so the caller ends without reading. `Err` when the
    /// watch's clone fails, so the caller reports instead of starting a
    /// read no quit can end. The watch ends at [`Retry::untrack`].
    pub(crate) fn track(&self, stream: &UnixStream) -> std::io::Result<bool> {
        let mut gate = self.lock();
        if gate.quit {
            return Ok(false);
        }
        gate.watched = Some(stream.try_clone()?);
        Ok(true)
    }

    /// Forgets the stream [`Retry::track`] watches.
    pub(crate) fn untrack(&self) {
        self.lock().watched = None;
    }

    /// Blocks until a delay is given and has passed on `clock`: true then,
    /// false once the loop quits. Waiting for the delay holds no timer.
    pub(crate) fn wait(&self, clock: &dyn Clock) -> bool {
        let gate = self.lock();
        let mut gate = self
            .changed
            .wait_while(gate, |gate| gate.delay.is_none() && !gate.quit)
            .unwrap_or_else(PoisonError::into_inner);
        let Some(delay) = gate.delay.take() else {
            return false;
        };
        let start = clock.now();
        let until = start.checked_add(delay).unwrap_or(start);
        loop {
            if gate.quit {
                return false;
            }
            if clock.now() >= until {
                return true;
            }
            // The lock is held into `wait_until`, so a wake blocks on it
            // until the condvar wait releases it, and is never missed.
            let mut slot = Some(gate);
            clock.wait_until(Some(until), &mut |bound| {
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

    /// The delay given and not yet taken.
    #[cfg(test)]
    pub(crate) fn held(&self) -> Option<Duration> {
        self.lock().delay
    }

    /// The gate, even after a thread panicked holding it.
    fn lock(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
