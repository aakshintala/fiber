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
    accept: Mutex<Option<JoinHandle<()>>>,
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
            |wake| woken(seen, *wake),
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

    /// Spends the wake pipe's bytes, so the next park sleeps instead of
    /// ending at once on bytes an earlier wake already consumed. Any
    /// unreadable end breaks the drain; a byte left behind only wakes the
    /// next park once, which spends it then.
    fn drain_wake(&self) {
        let mut one = [0u8; 64];
        while rustix::io::read(self.wake_recv.as_fd(), &mut one).is_ok() {}
    }
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
            accept: Mutex::new(None),
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
        });
        // Subscribed before any waiter checks: no advance slips between a
        // check and its park.
        clock.subscribe(Arc::downgrade(&shared._waker) as std::sync::Weak<dyn Wake>);
        Ok(Self { shared })
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
    /// again.
    pub(crate) fn serve_persistent(&self, handler: impl Fn(Connection) + Send + Sync + 'static) {
        let handler = Arc::new(handler);
        let serving = self.clone();
        let accept = thread::Builder::new()
            .name("fakes-http-accept".to_owned())
            .spawn(move || {
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
            *lock(&self.shared.accept) = Some(accept);
        }
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
        // No wake-generation bump beside the notify: every parked wait
        // ends on the notify, and every other waiter re-checks stopping
        // past its millisecond poll, so no waiter sleeps past shutdown.
        self.shared.wake_changed.notify_all();
        self.shared.idle_changed.notify_all();
        // Take and close the listener: the port refuses connections
        // even while stuck connections outlive the server.
        let _listener = lock(&self.shared.listener).take();
        // Break every blocked tunnel copy before joining its thread.
        for (_, client, origin) in std::mem::take(&mut lock(&self.shared.tunnels).live) {
            match client.shutdown(std::net::Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
            match origin.shutdown(std::net::Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
        // Bound before matching: a match scrutinee temporary would hold
        // the mutex through the whole join below, blocking every drop.
        let accept = lock(&self.shared.accept).take();
        match accept {
            None => true,
            Some(accept) => join_within(accept, SHUTDOWN_WITHIN),
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
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
