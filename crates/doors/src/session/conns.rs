//! Owns the live connection registry and the waits used to shut it down.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

use super::lock;
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use contract::SessionId;
use contract::clock::Clock;

// debt: 2 s grace is picked, not measured; a slow client's measured drain time would set it.
/// How long [`super::Session::close`] waits for a connection's writer to finish
/// before it shuts the socket.
pub(super) const GRACE: Duration = Duration::from_secs(2);

// debt: 250 ms is picked, not measured; the measured time a cut writer takes to flush its `indeterminate` lines would set it.
/// How long [`super::Session::close`] waits, after the grace, for the writers
/// it cut to write their lines before it shuts the sockets.
pub(super) const CUT_FLUSH: Duration = Duration::from_millis(250);

/// A control line that tells a writer the grace passed: never written to
/// the client.
pub(crate) const CUT: &str = "doors.cut";

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
    /// The reader's thread, to tell `reap` from the reader itself.
    reader_thread: ThreadId,
    /// The tracked reader thread whose answers this connection's writer
    /// writes, if it has a writer: [`Conns::cut_owed`] cuts the
    /// connections that still owe answers.
    owed_by: Option<ThreadId>,
    /// The cut's answers are flushed: `write_cut` wrote what the cut
    /// decided, or the writer ended. Set under the lock before the wake,
    /// so [`Conns::wait_cut`] never misses it between the wake and the
    /// thread's exit. The writer stays up past the cut to stream what is
    /// written next.
    cut_flushed: bool,
    writer: Option<JoinHandle<()>>,
    /// Stops the reader and shuts the socket. [`Conns::quiet_readers`]
    /// drops it to stop the reader and leave the socket open.
    ender: Option<Box<dyn Fn() + Send + Sync>>,
    /// Shuts the socket and stops nothing: what shuts it once the ender is
    /// gone.
    shut: Box<dyn Fn() + Send + Sync>,
    /// Pushes into the queue the connection's writer is reading, once its
    /// writer spawned and registered.
    wake: Option<log::Injector>,
    /// The close grace passed: the writer answers what it holds
    /// `indeterminate` and ends.
    cut: bool,
    /// The reader returned from its work, while the gate was stopped.
    finished: bool,
}

impl Live {
    /// Shuts the socket, through the ender while it is still there.
    fn shut_now(&self) {
        match self.ender.as_ref() {
            Some(ender) => ender(),
            None => (self.shut)(),
        }
    }
}

/// The closure that shuts `stream` and stops nothing.
pub(crate) fn shutter(stream: UnixStream) -> Box<dyn Fn() + Send + Sync> {
    Box::new(move || {
        #[cfg(test)]
        if crate::session::shutdown_skipped_for_test() {
            return;
        }
        match stream.shutdown(Shutdown::Both) {
            Ok(()) | Err(_) => {}
        }
    })
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

    /// Ends the connections: shuts every socket, waits until `until` for each
    /// reader to return, then reaps them outside the lock. A reader still
    /// inside a command then is detached, not joined, and counted in the
    /// result: its line was answered by the cut, and the thread ends with
    /// the process.
    pub(super) fn join_clients(&self, clock: &dyn Clock, until: Instant) -> usize {
        let live = {
            let mut state = lock(&self.state);
            for (_, live) in &state.live {
                live.shut_now();
            }
            let me = thread::current().id();
            while clock.now() < until
                && state
                    .live
                    .iter()
                    .any(|(_, live)| !live.finished && live.reader_thread != me)
            {
                state =
                    support::clock::park(clock, Some(until), None, &self.writers, state, |_| false)
                        .unwrap_or_else(|| lock(&self.state));
            }
            std::mem::take(&mut state.live)
        };
        let mut detached = 0;
        for (_, live) in live {
            if reap(live) {
                detached += 1;
            }
        }
        detached
    }

    /// Records `handle`, `ender` and `shut` together, and returns the id
    /// `serve` finishes the connection with. `close` joins the reader; the
    /// ender stops the reader and shuts the socket, and `shut` shuts it
    /// once the ender is gone. A published connection never lacks them. The
    /// stopped check and the publication share the connection lock with
    /// [`Conns::mark_stopped`] and [`Conns::join_clients`], so a reader
    /// admitted after the stop is rejected: its stream is shut down and
    /// its handle is returned for the caller to end, never published.
    pub(crate) fn push_reader(
        &self,
        handle: JoinHandle<()>,
        ender: Box<dyn Fn() + Send + Sync>,
        shut: Box<dyn Fn() + Send + Sync>,
    ) -> Result<u64, JoinHandle<()>> {
        let mut state = lock(&self.state);
        if self.stop.load(Ordering::Relaxed) {
            drop(state);
            ender();
            return Err(handle);
        }
        let id = state.next;
        state.next = state.next.wrapping_add(1);
        state.live.push((
            id,
            Live {
                reader_thread: handle.thread().id(),
                owed_by: None,
                cut_flushed: false,
                reader: Some(handle),
                writer: None,
                ender: Some(ender),
                shut,
                wake: None,
                cut: false,
                finished: false,
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

    /// Records the tracked reader thread whose answers connection `conn`'s
    /// writer writes. The writer notes it when it starts, running on the
    /// reader's own thread.
    pub(crate) fn note_owed(&self, conn: u64, reader: ThreadId) {
        let mut state = lock(&self.state);
        if let Some((_, live)) = state.live.iter_mut().find(|(slot, _)| *slot == conn) {
            live.owed_by = Some(reader);
        }
    }

    /// Drops this connection's socket and joins its writer. The reader calls
    /// it as it exits. While the gate is stopped the entry stays, marked
    /// finished: its writer may still be writing answers, and
    /// [`Conns::join_clients`] reaps it after the writers are done.
    pub(crate) fn finish(&self, id: u64) {
        let taken = {
            let mut state = lock(&self.state);
            let pos = state.live.iter().position(|(slot, _)| *slot == id);
            if self.stopped() {
                if let Some(pos) = pos
                    && let Some((_, live)) = state.live.get_mut(pos)
                {
                    live.finished = true;
                }
                None
            } else {
                // The reader is the caller, or returned already (the accept
                // thread finishes a connection whose reader never started).
                pos.map(|pos| {
                    let mut live = state.live.swap_remove(pos).1;
                    live.finished = true;
                    live
                })
            }
        };
        if let Some(live) = taken {
            reap(live);
        }
        // The socket closed outside the lock. Taking it before the notify
        // means a waiter that judged the descriptors still open has parked.
        let _held = lock(&self.state);
        self.writers.notify_all();
    }

    /// Shuts the live connection's socket, if any, without removing its
    /// entry: it stops the reader while the ender is there, so the reader
    /// runs its normal cleanup, and the client sees EOF. Runs under the
    /// connection lock and never joins (`docs/code-quality.md`, "Threads"):
    /// a writer whose watcher failed calls it from the writer thread, while
    /// the reader reaps that thread.
    pub(crate) fn shut(&self, id: u64) {
        let state = lock(&self.state);
        if let Some((_, live)) = state.live.iter().find(|(slot, _)| *slot == id) {
            live.shut_now();
        }
    }

    /// Stops every reader from reading its next line, and leaves the
    /// sockets open: a reader finishes the line it is dispatching, and
    /// answers it, then ends. The ender is dropped without being called,
    /// and dropping the reader's `Stop` stops its read
    /// (`support::stoppable`). Never joins.
    pub(super) fn quiet_readers(&self) {
        let mut state = lock(&self.state);
        for (_, live) in &mut state.live {
            drop(live.ender.take());
        }
    }

    /// Whether any live reader has not finished the line it is
    /// dispatching: one lock, no wait. The caller notes the wait-entry
    /// probe after this returns true and all locks are released, then
    /// parks in [`Conns::wait_readers_finished`].
    pub(super) fn has_unfinished_readers(&self) -> bool {
        lock(&self.state)
            .live
            .iter()
            .any(|(_, live)| !live.finished)
    }

    /// Waits, up to `until`, for every live reader to finish the line it
    /// is dispatching. A reader that is still dispatching when the grace
    /// passes is cut `indeterminate` by the caller, so the bound is the
    /// same `until` the owed wait used: no new duration, no restarted
    /// clock. Parks on the `writers` condvar that [`Conns::finish`]
    /// already notifies, and never joins a reader.
    pub(super) fn wait_readers_finished(&self, clock: &dyn Clock, until: Instant) {
        let mut state = lock(&self.state);
        while clock.now() < until && state.live.iter().any(|(_, live)| !live.finished) {
            // The guard is held into the condvar wait; `None` relocks.
            state = support::clock::park(clock, Some(until), None, &self.writers, state, |_| false)
                .unwrap_or_else(|| lock(&self.state));
        }
    }

    /// Waits until `until` for a connection's writer to finish, for
    /// [`Session::close`]: a writer blocked in `write` holds the close
    /// past the grace, and the cut past the flush margin.
    pub(super) fn wait_writers(&self, clock: &dyn Clock, until: Instant) {
        let mut state = lock(&self.state);
        while should_wait(state.writers_open as usize, clock.now(), until) {
            // The guard is held into the condvar wait; `None` relocks.
            state = support::clock::park(clock, Some(until), None, &self.writers, state, |_| false)
                .unwrap_or_else(|| lock(&self.state));
        }
    }

    /// Marks connection `conn`'s cut answers flushed: `write_cut` wrote
    /// what the cut decided, or the writer ended. Runs on the writer
    /// thread before its end wake, so [`Conns::wait_cut`] sees it under
    /// the same lock that parks it.
    pub(crate) fn note_cut_flushed(&self, conn: u64) {
        let mut state = lock(&self.state);
        if let Some((_, live)) = state.live.iter_mut().find(|(slot, _)| *slot == conn) {
            live.cut_flushed = true;
        }
        drop(state);
        self.writers.notify_all();
    }

    /// Registers `wake`, the injector of the queue connection `conn`'s writer
    /// reads, and says whether the connection was cut. One hold of the
    /// connection lock, which [`Conns::cut_owed`] also takes to set the flag
    /// and push the control line, so a writer never waits on a queue after
    /// a cut it has not seen. A connection no longer here counts as cut.
    pub(crate) fn register_wake(&self, conn: u64, wake: log::Injector) -> bool {
        let mut state = lock(&self.state);
        match state.live.iter_mut().find(|(slot, _)| *slot == conn) {
            Some((_, live)) => {
                live.wake = Some(wake);
                live.cut
            }
            None => true,
        }
    }

    /// The quiesce grace passed: marks every live connection whose reader
    /// still owes an answer cut and wakes its writer, which answers what
    /// it holds `indeterminate` and stays up, to stream `fiber_exited`.
    /// Connections with nothing owed stay up too.
    pub(super) fn cut_owed(&self, session: &SessionId, owed: impl Fn(ThreadId) -> bool) {
        let mut state = lock(&self.state);
        for (_, live) in &mut state.live {
            if !live.owed_by.is_some_and(&owed) {
                continue;
            }
            live.cut = true;
            if let Some(wake) = &live.wake {
                wake.push_kept(crate::client::control_line(session, CUT, 0));
            }
        }
    }

    /// Waits, up to `until`, for every cut writer to flush: each writes
    /// what it holds, then its `indeterminate` lines. The writer stays up
    /// past the cut to stream what is written next — `fiber_exited` in
    /// [`Session::quiesce`](super::Session::quiesce) — and ends at the
    /// log's end. Uncut writers stay up and are not waited for, and a cut
    /// connection with no writer holds nothing to flush.
    pub(super) fn wait_cut(&self, clock: &dyn Clock, until: Instant) {
        let mut state = lock(&self.state);
        while clock.now() < until
            && state
                .live
                .iter()
                .any(|(_, live)| live.cut && live.writer.is_some() && !live.cut_flushed)
        {
            // The guard is held into the condvar wait; `None` relocks.
            state = support::clock::park(clock, Some(until), None, &self.writers, state, |_| false)
                .unwrap_or_else(|| lock(&self.state));
        }
    }

    /// The grace passed for [`Session::close`]: marks every live
    /// connection cut and wakes its writer, which answers what it holds
    /// `indeterminate` and ends.
    pub(super) fn cut_all(&self, session: &SessionId) {
        let mut state = lock(&self.state);
        for (_, live) in &mut state.live {
            live.cut = true;
            if let Some(wake) = &live.wake {
                wake.push_kept(crate::client::control_line(session, CUT, 0));
            }
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

    /// How many connections are live right now: one lock, no wait.
    #[cfg(test)]
    pub(crate) fn live_len(&self) -> usize {
        lock(&self.state).live_len()
    }

    /// How many writers are open right now: one lock, no wait.
    #[cfg(test)]
    pub(crate) fn writers_open(&self) -> u32 {
        lock(&self.state).writers_open()
    }

    /// Whether `id` is published right now: one lock, no wait.
    #[cfg(test)]
    pub(crate) fn published(&self, id: u64) -> bool {
        lock(&self.state).published(id)
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

    /// Some live connection keeps its reader, its writer and its ender.
    pub(crate) fn has_full_entry(&self) -> bool {
        self.live
            .iter()
            .any(|(_, live)| live.reader.is_some() && live.writer.is_some() && live.ender.is_some())
    }

    /// Some live connection keeps its reader and its ender.
    pub(crate) fn has_reader_shutdown(&self) -> bool {
        self.live
            .iter()
            .any(|(_, live)| live.reader.is_some() && live.ender.is_some())
    }
}

/// True while `close` or `quiesce` must keep waiting: something is still
/// owed and the grace has not been reached. A `match` on the count, not a
/// comparison, so no operator here widens into a wait that parks forever:
/// the `0` arm returns at once without reading the clock, and the owed arm
/// holds while now is before the deadline. An equal instant is the deadline
/// itself. Waiting on through it would spin: the clock does not park for a
/// time that has already arrived. The boundary table in `session_tests`
/// pins every corner.
pub(super) fn should_wait(owed: usize, now: std::time::Instant, until: std::time::Instant) -> bool {
    match owed {
        0 => false,
        _ => now < until,
    }
}

/// Shuts the socket, joins the writer, and joins the reader once it
/// returned. A reader reaping itself detaches its own handle; joining it
/// would deadlock. A reader still inside a command is detached too, and
/// the result says so: it is `true` only for that case.
fn reap(live: Live) -> bool {
    live.shut_now();
    if let Some(writer) = live.writer {
        super::join(writer);
    }
    let Some(reader) = live.reader else {
        return false;
    };
    if reader.thread().id() == thread::current().id() {
        drop(reader);
        false
    } else if live.finished {
        super::join(reader);
        false
    } else {
        drop(reader);
        true
    }
}
