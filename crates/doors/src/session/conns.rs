//! Owns the live connection registry and the waits used to shut it down.

use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{Gate, lock};

// debt: 2 s grace is picked, not measured; a slow client's measured drain time would set it.
/// How long [`super::Session::close`] waits for a connection's writer to finish
/// before it shuts the socket.
pub(super) const GRACE: Duration = Duration::from_secs(2);

pub(super) struct Live {
    pub(super) reader: Option<JoinHandle<()>>,
    pub(super) writer: Option<JoinHandle<()>>,
    pub(super) shutdown: Option<Box<dyn Fn() + Send + Sync>>,
}

pub(super) struct Conns {
    pub(super) live: Vec<(u64, Live)>,
    pub(super) writers_open: u32,
    /// The next connection id. Starts at 1, so a missed store cannot look
    /// like the first connection.
    pub(super) next: u64,
}

impl Gate {
    pub(super) fn wait_writers(&self) {
        let until = self.clock.now() + GRACE;
        let mut conns = lock(&self.conns);
        while conns.writers_open > 0 && grace_remains(self.clock.now(), until) {
            let writers = &self.writers;
            let mut slot = Some(conns);
            self.clock.wait_until(Some(until), &mut |bound| {
                let Some(guard) = slot.take() else {
                    return;
                };
                slot = Some(match bound {
                    Some(limit) => {
                        writers
                            .wait_timeout(guard, limit)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                    None => writers.wait(guard).unwrap_or_else(PoisonError::into_inner),
                });
            });
            conns = match slot {
                Some(guard) => guard,
                None => lock(&self.conns),
            };
        }
    }

    pub(super) fn join_clients(&self) {
        let live = {
            let mut conns = lock(&self.conns);
            std::mem::take(&mut conns.live)
        };
        for (_, live) in live {
            reap(live);
        }
    }

    /// Records `handle` and `shutdown` together, and returns the id `serve`
    /// finishes the connection with. `close` joins the reader; the shutdown
    /// is what unblocks it. A published connection never lacks one. The
    /// stopped check and the publication share the connection lock with
    /// [`Gate::mark_stopped`] and [`Gate::join_clients`], so a reader
    /// admitted after the stop is rejected: its stream is shut down and
    /// its handle is returned for the caller to end, never published.
    pub(crate) fn push_reader(
        &self,
        handle: JoinHandle<()>,
        shutdown: Box<dyn Fn() + Send + Sync>,
    ) -> Result<u64, JoinHandle<()>> {
        let mut conns = lock(&self.conns);
        if self.stop.load(Ordering::Relaxed) {
            drop(conns);
            shutdown();
            return Err(handle);
        }
        let id = conns.next;
        conns.next = conns.next.wrapping_add(1);
        conns.live.push((
            id,
            Live {
                reader: Some(handle),
                writer: None,
                shutdown: Some(shutdown),
            },
        ));
        Ok(id)
    }

    pub(crate) fn push_writer(&self, id: u64, handle: JoinHandle<()>) {
        let mut conns = lock(&self.conns);
        if let Some((_, live)) = conns.live.iter_mut().find(|(slot, _)| *slot == id) {
            live.writer = Some(handle);
        }
        drop(conns);
        self.writers.notify_all();
    }

    /// Drops this connection's socket and joins its writer. The reader calls
    /// it as it exits.
    pub(crate) fn finish(&self, id: u64) {
        let taken = {
            let mut conns = lock(&self.conns);
            let pos = conns.live.iter().position(|(slot, _)| *slot == id);
            pos.map(|pos| conns.live.swap_remove(pos).1)
        };
        if let Some(live) = taken {
            reap(live);
        }
        // The socket closed outside the lock. Taking it before the notify
        // means a waiter that judged the descriptors still open has parked.
        let _held = lock(&self.conns);
        self.writers.notify_all();
    }

    /// Runs the live connection's stored shutdown closure, if any,
    /// without removing its entry: the reader gets EOF and runs its normal
    /// cleanup, and the client sees EOF. Runs under the connection lock
    /// and never joins (`docs/code-quality.md`, "Threads"): a writer
    /// whose watcher failed calls it from the writer thread, while the
    /// reader reaps that thread.
    pub(crate) fn shut(&self, id: u64) {
        let conns = lock(&self.conns);
        if let Some((_, live)) = conns.live.iter().find(|(slot, _)| *slot == id)
            && let Some(shutdown) = live.shutdown.as_ref()
        {
            shutdown();
        }
    }

    pub(super) fn mark_stopped(&self) {
        // Held while storing, so a `push_reader` either publishes before
        // the stop and is reaped by `join_clients`, or sees the stop and
        // is rejected: the check and the publication are atomic.
        let _conns = lock(&self.conns);
        self.stop.store(true, Ordering::Relaxed);
        self.writers.notify_all();
    }

    pub(super) fn wait_for_room(&self) {
        let conns = lock(&self.conns);
        if self.stopped() {
            return;
        }
        #[cfg(test)]
        super::tests::note_accept_wait();
        drop(
            self.writers
                .wait(conns)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    pub(crate) fn begin_writer(&self) {
        lock(&self.conns).writers_open += 1;
    }

    pub(crate) fn end_writer(&self) {
        let mut conns = lock(&self.conns);
        conns.writers_open = conns.writers_open.saturating_sub(1);
        drop(conns);
        self.writers.notify_all();
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// True while the grace has not been reached. An equal instant is the
/// deadline itself. Waiting on through it would spin: the clock does not
/// park for a time that has already arrived.
pub(super) fn grace_remains(now: Instant, until: Instant) -> bool {
    now < until
}

/// Shuts the connection's socket and joins the threads still running on it.
/// A reader reaping itself detaches its own handle; joining it would deadlock.
fn reap(live: Live) {
    if let Some(shutdown) = live.shutdown {
        shutdown();
    }
    if let Some(writer) = live.writer {
        super::join(writer);
    }
    if let Some(reader) = live.reader {
        if reader.thread().id() == thread::current().id() {
            drop(reader);
        } else {
            super::join(reader);
        }
    }
}
