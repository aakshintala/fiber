//! Owns the live connection registry and the waits used to shut it down.

use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::clock::Clock;

// debt: 2 s grace is picked, not measured; a slow client's measured drain time would set it.
/// How long [`super::Session::close`] waits for a connection's writer to finish
/// before it shuts the socket.
pub(super) const GRACE: Duration = Duration::from_secs(2);

/// The live connections, how many writers are open, and the next connection id.
pub(crate) struct Conns {
    state: Mutex<State>,
    writers: Condvar,
    stop: AtomicBool,
}

pub(crate) struct State {
    live: Vec<(u64, Live)>,
    writers_open: u32,
    /// The next connection id. Starts at 1, so a missed store cannot look
    /// like the first connection.
    next: u64,
}

struct Live {
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    /// The stored closure: it stops the reader and shuts the socket.
    shutdown: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Conns {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                live: Vec::new(),
                writers_open: 0,
                next: 1,
            }),
            writers: Condvar::new(),
            stop: AtomicBool::new(false),
        }
    }

    pub(super) fn wait_writers(&self, clock: &dyn Clock) {
        let until = clock.now() + GRACE;
        let mut state = lock(&self.state);
        while should_wait(state.writers_open, clock.now(), until) {
            // The guard is held into the condvar wait; `None` relocks.
            state = support::clock::park(clock, Some(until), None, &self.writers, state, |_| false)
                .unwrap_or_else(|| lock(&self.state));
        }
    }

    pub(super) fn join_clients(&self) {
        let live = {
            let mut state = lock(&self.state);
            std::mem::take(&mut state.live)
        };
        for (_, live) in live {
            reap(live);
        }
    }

    /// Records `handle` and `shutdown` together, and returns the id `serve`
    /// finishes the connection with. `close` joins the reader; the stored
    /// closure stops the reader and shuts the socket. A published connection never lacks one. The
    /// stopped check and the publication share the connection lock with
    /// [`Conns::mark_stopped`] and [`Conns::join_clients`], so a reader
    /// admitted after the stop is rejected: its stream is shut down and
    /// its handle is returned for the caller to end, never published.
    pub(crate) fn push_reader(
        &self,
        handle: JoinHandle<()>,
        shutdown: Box<dyn Fn() + Send + Sync>,
    ) -> Result<u64, JoinHandle<()>> {
        let mut state = lock(&self.state);
        if self.stop.load(Ordering::Relaxed) {
            drop(state);
            shutdown();
            return Err(handle);
        }
        let id = state.next;
        state.next = state.next.wrapping_add(1);
        state.live.push((
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
        let mut state = lock(&self.state);
        if let Some((_, live)) = state.live.iter_mut().find(|(slot, _)| *slot == id) {
            live.writer = Some(handle);
        }
        drop(state);
        self.writers.notify_all();
    }

    /// Drops this connection's socket and joins its writer. The reader calls
    /// it as it exits.
    pub(crate) fn finish(&self, id: u64) {
        let taken = {
            let mut state = lock(&self.state);
            let pos = state.live.iter().position(|(slot, _)| *slot == id);
            pos.map(|pos| state.live.swap_remove(pos).1)
        };
        if let Some(live) = taken {
            reap(live);
        }
        // The socket closed outside the lock. Taking it before the notify
        // means a waiter that judged the descriptors still open has parked.
        let _held = lock(&self.state);
        self.writers.notify_all();
    }

    /// Runs the live connection's stored closure, if any,
    /// without removing its entry: it stops the reader and shuts the
    /// socket, so the reader runs its normal cleanup, and the client sees
    /// EOF. Runs under the connection lock
    /// and never joins (`docs/code-quality.md`, "Threads"): a writer
    /// whose watcher failed calls it from the writer thread, while the
    /// reader reaps that thread.
    pub(crate) fn shut(&self, id: u64) {
        let state = lock(&self.state);
        if let Some((_, live)) = state.live.iter().find(|(slot, _)| *slot == id)
            && let Some(shutdown) = live.shutdown.as_ref()
        {
            shutdown();
        }
    }

    pub(super) fn mark_stopped(&self) {
        // Held while storing, so a `push_reader` either publishes before
        // the stop and is reaped by `join_clients`, or sees the stop and
        // is rejected: the check and the publication are atomic.
        let _state = lock(&self.state);
        self.stop.store(true, Ordering::Relaxed);
        self.writers.notify_all();
    }

    pub(super) fn wait_for_room(&self) {
        let state = lock(&self.state);
        if self.stopped() {
            return;
        }
        #[cfg(test)]
        super::tests::note_accept_wait();
        drop(
            self.writers
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    pub(crate) fn begin_writer(&self) {
        lock(&self.state).writers_open += 1;
    }

    pub(crate) fn end_writer(&self) {
        let mut state = lock(&self.state);
        state.writers_open = state.writers_open.saturating_sub(1);
        drop(state);
        self.writers.notify_all();
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Wakes whoever waits on the registry: a waiter that judged and has not
    /// yet parked cannot miss it, since the lock is taken before the notify.
    pub(crate) fn notify(&self) {
        let _held = lock(&self.state);
        self.writers.notify_all();
    }

    /// Waits up to `within` while `keep` holds, and says whether it stopped
    /// holding in time.
    #[cfg(test)]
    pub(super) fn wait_while(
        &self,
        within: Duration,
        mut keep: impl FnMut(&State) -> bool,
    ) -> bool {
        let guard = lock(&self.state);
        let (_guard, waited) = self
            .writers
            .wait_timeout_while(guard, within, |state| keep(state))
            .unwrap_or_else(PoisonError::into_inner);
        !waited.timed_out()
    }
}

#[cfg(test)]
impl State {
    pub(crate) fn live_len(&self) -> usize {
        self.live.len()
    }

    pub(crate) fn writers_open(&self) -> u32 {
        self.writers_open
    }

    pub(crate) fn published(&self, id: u64) -> bool {
        self.live.iter().any(|(slot, _)| *slot == id)
    }
}

/// True while `close` must keep waiting for writers: one is still open
/// and the grace has not been reached. A `match` on the count, not a
/// comparison, so no operator here widens into a wait that parks forever:
/// the `0` arm returns at once without reading the clock, and the open arm
/// defers to [`grace_remains`]. The boundary table in `session_tests` pins
/// every corner.
pub(super) fn should_wait(
    writers_open: u32,
    now: std::time::Instant,
    until: std::time::Instant,
) -> bool {
    match writers_open {
        0 => false,
        _ => grace_remains(now, until),
    }
}

/// True while the grace has not been reached. An equal instant is the
/// deadline itself. Waiting on through it would spin: the clock does not
/// park for a time that has already arrived.
pub(super) fn grace_remains(now: std::time::Instant, until: std::time::Instant) -> bool {
    now < until
}

/// Stops the reader and shuts the socket through the stored closure, then
/// joins the threads still running on it.
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
