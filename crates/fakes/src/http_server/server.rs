//! The core's listener lifecycle (`docs/testing.md`, "Fakes"): bounded
//! accept windows, the shared wait, shutdown and the CONNECT tunnel
//! registry. It owns no request parsing or adapter script state.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[cfg(test)]
use std::sync::{atomic::AtomicU64, mpsc};
use std::thread::{self, JoinHandle};

use contract::clock::{Clock, Wake};

use super::{
    AcceptMode, Connection, Limits, RequestBudget, SHUTDOWN_WITHIN, WaitEnd, guard_expired,
    join_within, lock,
};
#[cfg(test)]
use super::{CheckGate, WaitAck};

/// Bumps the server's wake generation whenever the injected clock moves.
struct ClockWaker {
    shared: std::sync::Weak<Shared>,
}

impl Wake for ClockWaker {
    fn wake(&self) {
        if let Some(shared) = self.shared.upgrade() {
            *lock(&shared.wake) += 1;
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
    _waker: Arc<ClockWaker>,
    #[cfg(test)]
    acks: Mutex<Option<mpsc::Sender<WaitAck>>>,
    /// How many times waiters entered [`Shared::wait`], test-only: a
    /// waiter that parks enters once per park, one that spins re-enters
    /// without a wake.
    #[cfg(test)]
    waits: AtomicU64,
    #[cfg(test)]
    gate: Mutex<Option<Arc<CheckGate>>>,
    pub(crate) idle: Mutex<usize>,
    pub(crate) idle_changed: Condvar,
    /// Started windows, test-only: the test advances only past a window's
    /// start, so no advance lands before its budget exists.
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
        acked: &mut Option<u64>,
    ) -> WaitEnd {
        // One count per entry: a waiter that parks enters once per
        // park, one that spins re-enters without a wake.
        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::SeqCst);
        // Read under the mutex the wake bumps, before every check, so a
        // wake between here and the park still ends the park at once.
        let seen = *lock(&self.wake);
        if self.stopping.load(Ordering::SeqCst) {
            return WaitEnd::Stopped;
        }
        if budget.expired() {
            return WaitEnd::Expired;
        }
        let guard_left = budget.guard_left();
        if guard_left.is_zero() {
            guard_expired(budget, name, phase);
        }
        if *acked != Some(seen) {
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
            *acked = Some(seen);
        }
        #[cfg(test)]
        if let Some(gate) = lock(&self.gate).as_ref().map(Arc::clone) {
            gate.hold();
        }
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
        WaitEnd::Waited
    }
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
        let shared = Arc::new_cyclic(|shared| Shared {
            listener: Mutex::new(Some(listener)),
            clock: Arc::clone(&clock),
            limits,
            stopping: AtomicBool::new(false),
            accept: Mutex::new(None),
            conns: Mutex::new(Vec::new()),
            wake: Mutex::new(0),
            wake_changed: Condvar::new(),
            _waker: Arc::new(ClockWaker {
                shared: shared.clone(),
            }),
            #[cfg(test)]
            acks: Mutex::new(None),
            #[cfg(test)]
            waits: AtomicU64::new(0),
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
        let mut seq = 0;
        let mut acked = None;
        match self.accept_once(mode, &budget, &mut seq, &mut acked) {
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
        #[cfg(test)]
        self.note_window();
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
        acked: &mut Option<u64>,
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
            match self
                .shared
                .wait(budget, mode.name(), "to accept the connection", seq, acked)
            {
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
