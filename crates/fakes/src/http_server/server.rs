//! The core's listener lifecycle (`docs/testing.md`, "Fakes"): bounded
//! accept windows, the shared wait, shutdown and the CONNECT tunnel
//! registry. It owns no request parsing or adapter script state.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::clock::{Clock, Wake};

use super::{
    AcceptMode, Connection, Limits, RequestBudget, SHUTDOWN_WITHIN, WaitEnd, guard_expired,
    join_within, lock,
};
#[cfg(test)]
use super::{CheckGate, WaitAck};

/// Bumps the server's wake generation whenever the injected clock moves,
/// and fires a byte down the wake pipe so a readiness park ends at once
/// past it, even mid-poll.
struct ClockWaker {
    shared: std::sync::Weak<Shared>,
}

impl Wake for ClockWaker {
    fn wake(&self) {
        if let Some(shared) = self.shared.upgrade() {
            *lock(&shared.wake) += 1;
            // After the bump, so a waiter that saw the old generation
            // still finds the byte when it polls; a full pipe drops it,
            // and the poll bound ends that park instead. The pipe holds
            // tens of thousands of advances, far past any test's.
            match rustix::io::write(shared.wake_send.as_fd(), &[1]) {
                Ok(_) | Err(_) => {}
            }
            shared.wake_changed.notify_all();
        }
    }
}

/// Whether a wake generation moved past the one a waiter saw: the park
/// ends at once past it, and only past it.
pub(crate) fn woken(seen: u64, current: u64) -> bool {
    current != seen
}

/// One accept attempt's end.
enum AcceptOutcome {
    Accepted(TcpStream),
    /// The window's budget ran out with no connection.
    Idle,
    /// Shutdown ended the wait.
    Stopped,
}

/// One persistent window's end.
pub(crate) enum WindowOutcome {
    Accepted(Connection),
    /// The window's budget ran out with no connection.
    Idle,
    /// Shutdown ended the wait.
    Stopped,
}

/// The core's open CONNECT tunnels: shutdown breaks every one.
pub(crate) struct TunnelState {
    /// The next tunnel's id.
    pub(crate) next: u64,
    /// Each open tunnel's sockets, broken on shutdown.
    pub(crate) live: Vec<(u64, TcpStream, TcpStream)>,
}

pub(crate) struct Shared {
    /// The loopback listener, polled nonblocking: shutdown takes and
    /// closes it, so the port refuses connections even while stuck
    /// connections keep the server alive.
    listener: Mutex<Option<TcpListener>>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) limits: Limits,
    /// Set by shutdown: every wait ends stopped.
    pub(crate) stopping: AtomicBool,
    accept: Mutex<AcceptSlot>,
    /// True once the shutdown that took the accept handle joined it
    /// within the bound, or a shutdown ran on a server that never
    /// served: the only way a test sees a failed join after the drop.
    /// Only moves false to true; a timed-out join leaves it false for
    /// good, including past later shutdown calls.
    joined: AtomicBool,
    conns: Mutex<Vec<JoinHandle<()>>>,
    /// The wake generation every waiter parks on.
    wake: Mutex<u64>,
    wake_changed: Condvar,
    /// The wake pipe's write end: every clock advance and shutdown fires
    /// a byte down it, so a readiness park ends at once past either,
    /// even mid-poll. Nonblocking; a full pipe drops the byte, and the
    /// poll bound ends that park instead.
    wake_send: UnixStream,
    /// The wake pipe's read end, polled beside every waited fd.
    wake_recv: UnixStream,
    _waker: Arc<ClockWaker>,
    #[cfg(test)]
    acks: Mutex<Option<mpsc::Sender<WaitAck>>>,
    /// How many times waiters entered [`Shared::wait`], test-only: a
    /// waiter that parks enters once per park, one that spins re-enters
    /// without a wake.
    #[cfg(test)]
    waits: AtomicU64,
    /// How many waits slept their whole bound with no wake: a wait the
    /// clock never advanced past and shutdown never ended sleeps its full
    /// poll bound, so each such sleep costs a busy turn about a
    /// millisecond. Only the busy-shape regression test reads it.
    #[cfg(test)]
    slept: AtomicU64,
    /// How many data readiness parks found their waited fd readable: a
    /// wait whose bytes arrive mid-park ends at once instead of sleeping
    /// past them. Accepts never count. Only the busy-shape regression
    /// test reads it.
    #[cfg(test)]
    readied: AtomicU64,
    /// How many socket bytes the lookahead held so far, test-only: every
    /// fill appends its read here, so a request whose body bypasses the
    /// lookahead adds only its head and one fill's over-read. Only the
    /// fixed-body regression test reads it.
    #[cfg(test)]
    lookahead: AtomicU64,
    #[cfg(test)]
    gate: Mutex<Option<Arc<CheckGate>>>,
    pub(crate) idle: Mutex<usize>,
    pub(crate) idle_changed: Condvar,
    /// Started windows, test-only: announced after the window's budget
    /// exists, so awaiting a window proves its start precedes the test's
    /// next advance.
    #[cfg(test)]
    pub(crate) windows: Mutex<usize>,
    #[cfg(test)]
    pub(crate) windows_changed: Condvar,
    /// Open tunnels, broken by shutdown: each entry's sockets end every
    /// blocked copy when the server stops.
    pub(crate) tunnels: Mutex<TunnelState>,
    /// The next serve that passes its stopping check waits here while
    /// the test holds it, test-only: the pause sits between the spawn
    /// and the slot store, inside the accept lock, so no outside seam
    /// reaches that window.
    #[cfg(test)]
    pause: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    /// The next condvar park waits here while the test holds it,
    /// test-only: the hold sits between the park becoming visible to
    /// the clock and the condvar wait, holding the wake mutex, so no
    /// outside seam reaches that window.
    #[cfg(test)]
    gap: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    /// The next shutdown signals immediately before it takes the accept
    /// lock, test-only: the spawn-race test waits for the actual contest.
    #[cfg(test)]
    shutdown_accept_lock: Mutex<Option<mpsc::Sender<()>>>,
}

impl Shared {
    /// Claims a tunnel id with its break sockets, unless shutdown
    /// already landed: the check and the registration hold one lock, so
    /// a shutdown racing either way still reaps both directions.
    /// `None` past shutdown, or when a break socket cannot be duped:
    /// the tunnel ends before any copy, as past shutdown.
    pub(crate) fn register_tunnel(&self, client: &TcpStream, origin: &TcpStream) -> Option<u64> {
        let mut tunnels = lock(&self.tunnels);
        if self.stopping.load(Ordering::SeqCst) {
            return None;
        }
        let id = tunnels.next;
        tunnels.next = tunnels.next.wrapping_add(1);
        match (client.try_clone(), origin.try_clone()) {
            (Ok(break_client), Ok(break_origin)) => {
                tunnels.live.push((id, break_client, break_origin));
                Some(id)
            }
            _ => None,
        }
    }

    /// Parks once until the clock wakes it, a poll ends, or shutdown
    /// lands. The caller maps expiry: idle for a window, a named failure
    /// for a request. Only the real guard panics here.
    #[allow(
        clippy::panic,
        reason = "a fake's wait; the panic names the request whose real guard ran out"
    )]
    pub(crate) fn wait(
        &self,
        budget: &RequestBudget,
        name: &str,
        phase: &'static str,
        seq: &mut u64,
        acked: &mut Option<(u64, &'static str)>,
    ) -> WaitEnd {
        // One count per entry: a waiter that parks enters once per
        // park, one that spins re-enters without a wake.
        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::SeqCst);
        let park = match self.pre_wait(budget, name, phase, seq, acked) {
            Ok(park) => park,
            Err(end) => return end,
        };
        // One park, bounded by the shorter of the poll and the guard. The
        // predicate holds the generation read above: a wake past that read
        // returns at once.
        if self.condvar_park(budget, park.seen, park.guard_left) {
            self.slept_count();
        }
        WaitEnd::Waited
    }

    /// Parks once until `io` is readable, the clock wakes it, a poll ends,
    /// or shutdown lands: the readiness wait behind socket reads and
    /// accepts. A wait with no wake still sleeps its whole bound, but one
    /// whose bytes arrive mid-park ends at once instead of sleeping past
    /// them. The caller maps expiry and shutdown exactly as with
    /// [`Shared::wait`]. `count_readiness` marks a data wait, whose
    /// readable park the test counter counts; accepts pass false.
    #[allow(
        clippy::too_many_arguments,
        reason = "the wait's own state threads through as in `wait`, plus the fd and its kind"
    )]
    pub(crate) fn wait_ready(
        &self,
        budget: &RequestBudget,
        name: &str,
        phase: &'static str,
        seq: &mut u64,
        acked: &mut Option<(u64, &'static str)>,
        io: Option<BorrowedFd<'_>>,
        count_readiness: bool,
    ) -> WaitEnd {
        // One count per entry, as in `wait`.
        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::SeqCst);
        let park = match self.pre_wait(budget, name, phase, seq, acked) {
            Ok(park) => park,
            Err(end) => return end,
        };
        let Some(io) = io else {
            // No fd to watch: the condvar way, as in `wait`.
            if self.condvar_park(budget, park.seen, park.guard_left) {
                self.slept_count();
            }
            return WaitEnd::Waited;
        };
        // One readiness park, bounded as the condvar park is: readable
        // ends it at once through the fd, an advance or shutdown through
        // the pipe, anything else at the bound.
        let mut slept = false;
        budget.clock.wait_until(budget.until(), &mut |bound| {
            let timeout = [bound, Some(self.limits.poll), Some(park.guard_left)]
                .into_iter()
                .flatten()
                .min()
                .map(|bound| rustix::event::Timespec::try_from(bound).unwrap_or_default());
            let mut fds = [
                rustix::event::PollFd::from_borrowed_fd(io, rustix::event::PollFlags::IN),
                rustix::event::PollFd::from_borrowed_fd(
                    self.wake_recv.as_fd(),
                    rustix::event::PollFlags::IN,
                ),
            ];
            match rustix::event::poll(&mut fds, timeout.as_ref()) {
                // Nothing ready within the bound: the whole bound slept.
                Ok(0) => slept = true,
                // The fd or the pipe is readable: a data wait's readable
                // fd counts a readiness outcome, and the pipe's bytes are
                // spent, so the next park sleeps again instead of
                // spinning on them.
                Ok(_) => {
                    if count_readiness && fds.first().is_some_and(|fd| !fd.revents().is_empty()) {
                        self.readied_count();
                    }
                    self.drain_wake();
                }
                // Interrupted or failed: the caller re-checks outside.
                Err(_) => {}
            }
        });
        if slept {
            self.slept_count();
        }
        WaitEnd::Waited
    }

    /// The checks before any park, shared by [`Shared::wait`] and
    /// [`Shared::wait_ready`]: shutdown and expiry end at once, the real
    /// guard fails, and a new generation-and-phase announces itself. `Ok`
    /// carries what the park bounds itself by.
    #[allow(
        clippy::panic,
        reason = "a fake's wait; the panic names the request whose real guard ran out"
    )]
    fn pre_wait(
        &self,
        budget: &RequestBudget,
        name: &str,
        phase: &'static str,
        seq: &mut u64,
        acked: &mut Option<(u64, &'static str)>,
    ) -> Result<Park, WaitEnd> {
        // Read under the mutex the wake bumps, before every check, so a
        // wake between here and the park still ends the park at once.
        let seen = *lock(&self.wake);
        if self.stopping.load(Ordering::SeqCst) {
            return Err(WaitEnd::Stopped);
        }
        if budget.expired() {
            return Err(WaitEnd::Expired);
        }
        let guard_left = budget.guard_left();
        if guard_left.is_zero() {
            guard_expired(budget, name, phase);
        }
        if *acked != Some((seen, phase)) {
            *seq += 1;
            #[cfg(test)]
            if let Some(acks) = lock(&self.acks).as_ref() {
                let ack = WaitAck {
                    phase,
                    thread: thread::current().id(),
                    seq: *seq,
                    until: budget.until(),
                };
                match acks.send(ack) {
                    Ok(()) | Err(_) => {}
                }
            }
            *acked = Some((seen, phase));
        }
        // Bound before the hold: a scrutinee temporary would keep the gate
        // mutex through the hold, blocking every other waiter's.
        #[cfg(test)]
        let gate = lock(&self.gate).as_ref().map(Arc::clone);
        #[cfg(test)]
        if let Some(gate) = gate {
            gate.hold(phase, *seq);
        }
        Ok(Park { seen, guard_left })
    }

    /// One condvar park for `seen`, bounded by the shorter of the poll and
    /// `guard_left`: true when no wake landed while parked, so the whole
    /// bound slept.
    /// One condvar park for `seen`, bounded by the shorter of the poll and
    /// `guard_left`: true when no wake landed while parked, so the whole
    /// bound slept.
    fn condvar_park(&self, budget: &RequestBudget, seen: u64, guard_left: Duration) -> bool {
        // One park, bounded by the shorter of the poll and the guard. The
        // predicate holds the generation read above: a wake past that read
        // returns at once.
        let guard = lock(&self.wake);
        let parked = support::clock::park(
            budget.clock.as_ref(),
            budget.until(),
            Some(self.limits.poll.min(guard_left)),
            &self.wake_changed,
            guard,
            |wake| {
                // Test-only hold between the park becoming visible and
                // the condvar wait, under the wake mutex: the test keeps
                // the waiter here across shutdown, so a notify sent
                // without the mutex is missed and the wait sleeps its
                // bound. Consumed once.
                #[cfg(test)]
                if let Some((reached, release)) = lock(&self.gap).take() {
                    match reached.send(()) {
                        Ok(()) | Err(_) => {}
                    }
                    match release.recv() {
                        Ok(()) | Err(_) => {}
                    }
                }
                woken(seen, *wake)
            },
        );
        drop(parked);
        // No generation moved past the read above and shutdown never
        // landed: nothing ended the park early, so the whole bound slept.
        !woken(seen, *lock(&self.wake)) && !self.stopping.load(Ordering::SeqCst)
    }

    /// Counts one whole-bound sleep, test-only.
    #[cfg(test)]
    fn slept_count(&self) {
        self.slept.fetch_add(1, Ordering::SeqCst);
    }

    /// Counts one whole-bound sleep: nothing in non-test builds, where no
    /// accessor reads the counter.
    #[cfg(not(test))]
    fn slept_count(&self) {}

    /// Counts one readiness park that found its waited fd readable,
    /// test-only.
    #[cfg(test)]
    fn readied_count(&self) {
        self.readied.fetch_add(1, Ordering::SeqCst);
    }

    /// Counts one readable readiness park: nothing in non-test builds,
    /// where no accessor reads the counter.
    #[cfg(not(test))]
    fn readied_count(&self) {}

    /// Counts socket bytes appended to the lookahead, test-only.
    #[cfg(test)]
    pub(crate) fn lookahead_count(&self, bytes: usize) {
        self.lookahead
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::SeqCst);
    }

    /// Counts an append to the lookahead: nothing in non-test builds,
    /// where no accessor reads the counter.
    #[cfg(not(test))]
    pub(crate) fn lookahead_count(&self, _bytes: usize) {}

    /// Spends the wake pipe's bytes, so the next park sleeps instead of
    /// ending at once on bytes an earlier wake already consumed. Any
    /// unreadable end breaks the drain; a byte left behind only wakes the
    /// next park once, which spends it then.
    fn drain_wake(&self) {
        let mut one = [0u8; 64];
        while rustix::io::read(self.wake_recv.as_fd(), &mut one).is_ok() {}
    }
}

/// Which accept thread the server owns: spawned and its handle change
/// only together under the one accept lock, so a shutdown racing the
/// spawn never sees a spawned thread as unspawned.
struct AcceptSlot {
    spawned: bool,
    handle: Option<JoinHandle<()>>,
}

/// What the pre-park checks decided: park bounded by `guard_left`, past
/// the generation `seen`.
struct Park {
    seen: u64,
    guard_left: Duration,
}

/// One bounded HTTP/1.1 core on a loopback port. Clone shares the server;
/// dropping any clone stops it, so a thread needing the server past its
/// own end borrows it under a scope instead of holding a clone.
#[derive(Clone)]
pub(crate) struct HttpServer {
    pub(crate) shared: Arc<Shared>,
    /// False for the handle the accept thread owns: the serving
    /// thread's end never stops the server, so only a caller's
    /// explicit shutdown or a caller-owned drop reaches shutdown.
    stops_on_drop: bool,
}

impl HttpServer {
    /// Binds a loopback port on `clock` with `limits`: it accepts when
    /// this returns.
    pub(crate) fn bind(clock: Arc<dyn Clock>, limits: Limits) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        // The wake pipe carries clock advances and shutdown to readiness
        // parks: nonblocking both ends, so neither a wake nor a drain
        // ever blocks.
        let (wake_recv, wake_send) = UnixStream::pair()?;
        wake_recv.set_nonblocking(true)?;
        wake_send.set_nonblocking(true)?;
        let shared = Arc::new_cyclic(|shared| Shared {
            listener: Mutex::new(Some(listener)),
            clock: Arc::clone(&clock),
            limits,
            stopping: AtomicBool::new(false),
            accept: Mutex::new(AcceptSlot {
                spawned: false,
                handle: None,
            }),
            joined: AtomicBool::new(false),
            conns: Mutex::new(Vec::new()),
            wake: Mutex::new(0),
            wake_changed: Condvar::new(),
            wake_send,
            wake_recv,
            _waker: Arc::new(ClockWaker {
                shared: shared.clone(),
            }),
            #[cfg(test)]
            acks: Mutex::new(None),
            #[cfg(test)]
            waits: AtomicU64::new(0),
            #[cfg(test)]
            slept: AtomicU64::new(0),
            #[cfg(test)]
            readied: AtomicU64::new(0),
            #[cfg(test)]
            lookahead: AtomicU64::new(0),
            #[cfg(test)]
            gate: Mutex::new(None),
            idle: Mutex::new(0),
            idle_changed: Condvar::new(),
            #[cfg(test)]
            windows: Mutex::new(0),
            #[cfg(test)]
            windows_changed: Condvar::new(),
            tunnels: Mutex::new(TunnelState {
                next: 0,
                live: Vec::new(),
            }),
            #[cfg(test)]
            pause: Mutex::new(None),
            #[cfg(test)]
            gap: Mutex::new(None),
            #[cfg(test)]
            shutdown_accept_lock: Mutex::new(None),
        });
        // Subscribed before any waiter checks: no advance slips between a
        // check and its park.
        clock.subscribe(Arc::downgrade(&shared._waker) as std::sync::Weak<dyn Wake>);
        Ok(Self {
            shared,
            stops_on_drop: true,
        })
    }

    /// The loopback address the core listens on.
    pub(crate) fn addr(&self) -> SocketAddr {
        lock(&self.shared.listener)
            .as_ref()
            .and_then(|listener| listener.local_addr().ok())
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    /// Waits for one connection under `mode` within a fresh budget, made
    /// before the attempt: `None` on shutdown. A persistent window's
    /// connection charges that window's budget; an empty expected window
    /// fails naming its request, an empty persistent one ends idle.
    #[allow(
        clippy::panic,
        reason = "a fake's wait; the panic names the expected request whose bound ran out"
    )]
    fn accept(&self, mode: &AcceptMode) -> Option<Connection> {
        let budget = RequestBudget::start_on(
            &self.shared.clock,
            self.shared.limits.request,
            self.shared.limits.guard,
        );
        #[cfg(test)]
        if matches!(mode, AcceptMode::Persistent) {
            self.note_window();
        }
        let mut seq = 0;
        let mut acked = None;
        // A duped handle of the listener for the window's readiness waits:
        // shutdown closing the listener never disturbs a poll on it, since
        // the wake pipe still ends the wait at once past shutdown; the
        // window drops it on its way out, so no dup keeps the port open
        // past shutdown. `None` waits the condvar way.
        let probe = lock(&self.shared.listener)
            .as_ref()
            .and_then(|listener| listener.try_clone().ok());
        match self.accept_once(mode, &budget, &mut seq, &mut acked, probe.as_ref()) {
            AcceptOutcome::Accepted(stream) => Some(Connection::new(
                stream,
                budget,
                mode.name().to_owned(),
                Arc::clone(&self.shared),
            )),
            // The wait's verdict, not a second check: an advance
            // between the two would fail an idle window that expired.
            AcceptOutcome::Idle => match mode {
                AcceptMode::Expected { name } => {
                    panic!(
                        "fakes: the HTTP core waited {:?} to accept {name}",
                        self.shared.limits.request
                    );
                }
                AcceptMode::Persistent => None,
            },
            AcceptOutcome::Stopped => None,
        }
    }

    /// Waits for the one connection `name` names: `None` on shutdown.
    pub(crate) fn accept_expected(&self, name: &str) -> Option<Connection> {
        self.accept(&AcceptMode::Expected {
            name: name.to_owned(),
        })
    }

    /// One persistent window: an accepted connection, an idle end, or a
    /// stopped wait.
    pub(crate) fn accept_window(&self) -> WindowOutcome {
        match self.accept(&AcceptMode::Persistent) {
            Some(conn) => WindowOutcome::Accepted(conn),
            // An empty window ends idle; shutdown ends stopped. The
            // accept above tells them apart everywhere except here, where
            // either restarts the loop: stopping is re-checked on the next
            // attempt, so shutdown still ends the window within one poll.
            None => {
                if self.shared.stopping.load(Ordering::SeqCst) {
                    WindowOutcome::Stopped
                } else {
                    WindowOutcome::Idle
                }
            }
        }
    }

    /// One nonblocking accept attempt inside the window's budget.
    fn accept_once(
        &self,
        mode: &AcceptMode,
        budget: &RequestBudget,
        seq: &mut u64,
        acked: &mut Option<(u64, &'static str)>,
        probe: Option<&TcpListener>,
    ) -> AcceptOutcome {
        loop {
            if self.shared.stopping.load(Ordering::SeqCst) {
                return AcceptOutcome::Stopped;
            }
            // Taken by shutdown: no more accepts after it. An empty
            // queue or an aborted handshake is not the listener's end:
            // poll again within the budget, which still bounds the wait
            // and names its expiry. Shutdown is checked first above, so
            // stopping still ends at once.
            if let Some(Ok((stream, _))) = lock(&self.shared.listener)
                .as_ref()
                .map(|listener| listener.accept())
            {
                if stream.set_nonblocking(true).is_err() {
                    continue;
                }
                return AcceptOutcome::Accepted(stream);
            }
            if budget.expired() {
                return AcceptOutcome::Idle;
            }
            // Readiness parks end at once when the connection arrives
            // instead of sleeping a whole poll past it.
            let ready = probe.map(|probe| probe.as_fd());
            match self.shared.wait_ready(
                budget,
                mode.name(),
                "to accept the connection",
                seq,
                acked,
                ready,
                false,
            ) {
                WaitEnd::Waited => {}
                WaitEnd::Expired => return AcceptOutcome::Idle,
                WaitEnd::Stopped => return AcceptOutcome::Stopped,
            }
        }
    }

    /// Serves every connection with `handler` on core-owned threads, all
    /// reaped by shutdown. An idle window renews its budget and accepts
    /// again. Serving a stopped server spawns nothing: the slot keeps
    /// its never-spawned shape, so a later shutdown still reads it as
    /// never served.
    pub(crate) fn serve_persistent(&self, handler: impl Fn(Connection) + Send + Sync + 'static) {
        // Taken before the accept lock, so no lock is ever held while
        // another is taken: a serve past a stop puts it back below.
        #[cfg(test)]
        let paused = lock(&self.shared.pause).take();
        let mut slot = lock(&self.shared.accept);
        if self.shared.stopping.load(Ordering::SeqCst) {
            #[cfg(test)]
            if let Some(paused) = paused {
                *lock(&self.shared.pause) = Some(paused);
            }
            return;
        }
        let handler = Arc::new(handler);
        let shared = Arc::clone(&self.shared);
        let accept = thread::Builder::new()
            .name("fakes-http-accept".to_owned())
            .spawn(move || {
                // Built inside the started thread, never owned by the
                // spawn closure: a failed spawn drops only an `Arc`,
                // and the serving thread's end never stops the server.
                let serving = HttpServer {
                    shared,
                    stops_on_drop: false,
                };
                loop {
                    match serving.accept_window() {
                        WindowOutcome::Accepted(conn) => {
                            let handler = Arc::clone(&handler);
                            match thread::Builder::new()
                                .name("fakes-http-conn".to_owned())
                                .spawn(move || {
                                    handler(conn);
                                }) {
                                Ok(conn) => lock(&serving.shared.conns).push(conn),
                                Err(_) => continue,
                            }
                        }
                        WindowOutcome::Idle => {
                            *lock(&serving.shared.idle) += 1;
                            serving.shared.idle_changed.notify_all();
                        }
                        WindowOutcome::Stopped => break,
                    }
                }
                // Release every connection thread before joining it: each
                // ends its core wait stopped on its own. Bound before the
                // loop, so the mutex is free while joining.
                let conns = std::mem::take(&mut *lock(&serving.shared.conns));
                for conn in conns {
                    match conn.join() {
                        Ok(()) | Err(_) => {}
                    }
                }
            });
        if let Ok(accept) = accept {
            // Test-only pause between the spawn and the store,
            // inside the accept lock: a shutdown racing this window
            // blocks on the lock instead of seeing a spawned thread
            // as unspawned.
            #[cfg(test)]
            if let Some((reached, release)) = paused {
                match reached.send(()) {
                    Ok(()) | Err(_) => {}
                }
                match release.recv() {
                    Ok(()) | Err(_) => {}
                }
            }
            slot.spawned = true;
            slot.handle = Some(accept);
        } else {
            #[cfg(test)]
            if let Some(paused) = paused {
                *lock(&self.shared.pause) = Some(paused);
            }
        }
    }

    /// Reads the dropped server's own state after the drop: the probe
    /// holds the shared core, so it stays valid past every handle.
    pub(crate) fn drop_probe(&self) -> DropProbe {
        DropProbe {
            shared: Arc::clone(&self.shared),
        }
    }

    /// The next serve that passes its stopping check waits between its
    /// spawn and its slot store until the test releases it, test-only:
    /// no outside seam reaches that window, so forcing the startup race
    /// needs this committed pause point. Returns the reached signal and
    /// the release channel; dropping the release ends the wait too.
    #[cfg(test)]
    pub(crate) fn test_pause_after_spawn(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *lock(&self.shared.pause) = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    /// Holds the next condvar park between its park becoming visible and
    /// its condvar wait until the test releases it, test-only: no outside
    /// seam reaches that window, so forcing the shutdown race needs this
    /// committed hold point. Returns the reached signal and the release
    /// channel; dropping the release ends the wait too.
    #[cfg(test)]
    pub(crate) fn test_park_gap(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *lock(&self.shared.gap) = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    /// Signals immediately before the next shutdown takes the accept
    /// lock, test-only: no outside seam reaches the contested acquisition.
    #[cfg(test)]
    pub(crate) fn test_shutdown_before_accept_lock(&self) -> mpsc::Receiver<()> {
        let (reached_tx, reached_rx) = mpsc::channel();
        *lock(&self.shared.shutdown_accept_lock) = Some(reached_tx);
        reached_rx
    }

    /// Stops the server: waits end stopped, open tunnels break, and
    /// every serving thread is joined. True when all stopped within the
    /// shutdown bound.
    pub(crate) fn shutdown(&self) -> bool {
        self.shared.stopping.store(true, Ordering::SeqCst);
        // Fires the wake pipe beside the notify, so a readiness park
        // ends at once past shutdown even mid-poll.
        match rustix::io::write(self.shared.wake_send.as_fd(), &[1]) {
            Ok(_) | Err(_) => {}
        }
        // A waiter between its park becoming visible and its condvar
        // wait holds this mutex, so taking it holds shutdown until the
        // waiter parks and the notify below lands after it waits.
        drop(lock(&self.shared.wake));
        // No wake-generation bump beside the notify: the mutex above
        // holds shutdown until a waiter between its park and its wait
        // has parked, so the notify ends every parked wait, and every
        // other waiter re-checks stopping past its millisecond poll, so
        // no waiter sleeps past shutdown.
        self.shared.wake_changed.notify_all();
        self.shared.idle_changed.notify_all();
        // Take and close the listener: the port refuses connections
        // even while stuck connections outlive the server.
        let _listener = lock(&self.shared.listener).take();
        // Bound to a local before the loop, so no guard lives through
        // the socket shutdowns below.
        let live = std::mem::take(&mut lock(&self.shared.tunnels).live);
        // Break every blocked tunnel copy before joining its thread.
        for (_, client, origin) in live {
            match client.shutdown(std::net::Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
            match origin.shutdown(std::net::Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
        // Only the spawn-race test installs this signal; with no hook,
        // shutdown follows the same path directly to the accept lock.
        #[cfg(test)]
        if let Some(reached) = lock(&self.shared.shutdown_accept_lock).take() {
            match reached.send(()) {
                Ok(()) | Err(_) => {}
            }
        }
        // Read together under one hold, released before the join: the
        // guard never lives through the join below. Only the call that
        // took the handle and joined it in time, or a call that found
        // no handle on a never-served server, records the join; a later
        // call that finds nothing past a live or failed join records
        // nothing, so it never turns a failed join into a joined one.
        let (spawned, accept) = {
            let mut slot = lock(&self.shared.accept);
            (slot.spawned, slot.handle.take())
        };
        match accept {
            None => {
                if !spawned {
                    self.shared.joined.store(true, Ordering::SeqCst);
                }
                true
            }
            Some(accept) => {
                let joined = join_within(accept, SHUTDOWN_WITHIN);
                if joined {
                    self.shared.joined.store(true, Ordering::SeqCst);
                }
                joined
            }
        }
    }

    /// Receives every clock-wait acknowledgment from here on, test-only.
    #[cfg(test)]
    pub(crate) fn test_acknowledgments(&self) -> mpsc::Receiver<WaitAck> {
        let (tx, rx) = mpsc::channel();
        *lock(&self.shared.acks) = Some(tx);
        rx
    }

    /// How many times waiters entered [`Shared::wait`] so far, test-only.
    #[cfg(test)]
    pub(crate) fn test_wait_entries(&self) -> u64 {
        self.shared.waits.load(Ordering::SeqCst)
    }

    /// How many waits slept their whole bound with no wake so far,
    /// test-only.
    #[cfg(test)]
    pub(crate) fn test_slept_waits(&self) -> u64 {
        self.shared.slept.load(Ordering::SeqCst)
    }

    /// How many readiness parks found their waited fd readable so far,
    /// test-only.
    #[cfg(test)]
    pub(crate) fn test_readied_waits(&self) -> u64 {
        self.shared.readied.load(Ordering::SeqCst)
    }

    /// How many socket bytes the lookahead held so far, test-only.
    #[cfg(test)]
    pub(crate) fn test_lookahead_bytes(&self) -> u64 {
        self.shared.lookahead.load(Ordering::SeqCst)
    }

    /// The clock-wake generation waiters park on, test-only: each move
    /// of the injected clock bumps it by one, so a test pins the bump
    /// without waiting on any waiter.
    #[cfg(test)]
    pub(crate) fn test_generation(&self) -> u64 {
        *lock(&self.shared.wake)
    }

    /// Holds check-to-park pauses from here on, test-only.
    #[cfg(test)]
    pub(crate) fn test_gate(&self) -> Arc<CheckGate> {
        let gate = Arc::new(CheckGate::new());
        *lock(&self.shared.gate) = Some(Arc::clone(&gate));
        gate
    }

    /// Counts one started window, test-only.
    #[cfg(test)]
    fn note_window(&self) {
        *lock(&self.shared.windows) += 1;
        self.shared.windows_changed.notify_all();
    }

    /// Checks a CONNECT target against the fake's trust boundary: a
    /// loopback numeric address or `localhost:<port>`, mapped without a
    /// blocking DNS lookup. Anything else is a named `InvalidInput`
    /// before any connect, so no fake ever initiates public-network I/O.
    pub(crate) fn check_target(target: &str) -> io::Result<SocketAddr> {
        let (host, port) = match target.rsplit_once(':') {
            Some((host, port)) => (host, port),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("fakes: the CONNECT target has no port: {target}"),
                ));
            }
        };
        // Bracketed IPv6 loopback first; localhost maps to loopback;
        // other names never resolve here.
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if host.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("fakes: the CONNECT target has no host: {target}"),
            ));
        }
        let port: u16 = match port.parse() {
            Ok(port) => port,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("fakes: the CONNECT target has no port: {target}"),
                ));
            }
        };
        if host.eq_ignore_ascii_case("localhost") {
            return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
        }
        match host.parse::<std::net::IpAddr>() {
            Ok(addr) if addr.is_loopback() => Ok(SocketAddr::new(addr, port)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("fakes: the CONNECT target is not a loopback fake: {target}"),
            )),
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        if self.stops_on_drop {
            self.shutdown();
        }
    }
}

/// The dropped server's own state, read after the drop: a reused port
/// can no longer flake the verdict, since nothing dials it. Both reads
/// are pure: they hold no lock across a call. A joined accept implies a
/// closed listener, because shutdown takes the listener before it joins.
#[doc(hidden)]
pub struct DropProbe {
    shared: Arc<Shared>,
}

impl DropProbe {
    /// True once shutdown took and closed the listener.
    pub fn listener_closed(&self) -> bool {
        lock(&self.shared.listener).is_none()
    }

    /// True once the shutdown that took the accept handle joined it
    /// within the bound, or a shutdown ran on a server that never
    /// served. False before any shutdown, and for good after a
    /// timed-out join.
    pub fn accept_joined(&self) -> bool {
        self.shared.joined.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
