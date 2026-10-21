//! A fake HTTP CONNECT proxy (`docs/dependencies.md`, "Proxies"): it answers
//! `CONNECT host:port` by dialling the target and copying bytes both ways, so
//! a test can prove a client tunnelled through it. Anything else gets a 405.
//! Accepting, head reading and the tunnel run on the shared HTTP core.

use std::io;
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use contract::clock::Clock;

use crate::clock::SystemClock;
use crate::deadline::Deadline;
#[cfg(test)]
use crate::http_server::WaitAck;
use crate::http_server::{
    Connection, HttpServer, Limits, TunnelEnd,
    request::{self, RequestOutcome},
};

#[derive(Default)]
struct State {
    /// Each `CONNECT` target in arrival order, recorded before the 200 is
    /// sent, so a client holding the tunnel sees it recorded.
    connects: Vec<String>,
    /// Connections whose client side closed first, in arrival order.
    closed: usize,
}

/// A fake CONNECT proxy listening on a local port. Dropping it closes the
/// port and every open tunnel: the core field drops first, and its own
/// drop shuts the server down, breaking tunnels and reaping the serving
/// threads, so no explicit `Drop` duplicates it.
pub struct ConnectProxy {
    core: HttpServer,
    state: Arc<Mutex<State>>,
    arrived: Arc<Condvar>,
}

impl ConnectProxy {
    /// Listens on a free port on 127.0.0.1. The port accepts connections
    /// when this returns.
    pub fn start() -> io::Result<Self> {
        Self::start_on(Arc::new(SystemClock))
    }

    /// [`ConnectProxy::start`], on `clock`: the test drives silent
    /// handshakes without waiting out the real bound.
    pub(crate) fn start_on(clock: Arc<dyn Clock>) -> io::Result<Self> {
        let proxy = Self {
            core: HttpServer::bind(clock, Limits::default())?,
            state: Arc::new(Mutex::new(State::default())),
            arrived: Arc::new(Condvar::new()),
        };
        let state = Arc::clone(&proxy.state);
        let arrived = Arc::clone(&proxy.arrived);
        proxy
            .core
            .serve_persistent(move |conn| serve(conn, &state, &arrived));
        Ok(proxy)
    }

    /// The proxy URL to hand a client, such as `http://127.0.0.1:49152`.
    pub fn url(&self) -> String {
        format!("http://{}", self.core.addr())
    }

    /// The local port the proxy listens on.
    pub fn port(&self) -> u16 {
        self.core.addr().port()
    }

    /// Every `CONNECT` target received so far, as `host:port`, in arrival
    /// order.
    pub fn connects(&self) -> Vec<String> {
        lock(&self.state).connects.clone()
    }

    /// Receives every clock-wait acknowledgment from here on, test-only:
    /// the test synchronises on the handshake reader's park instead of
    /// on elapsed time.
    #[cfg(test)]
    pub(crate) fn test_acknowledgments(&self) -> mpsc::Receiver<WaitAck> {
        self.core.test_acknowledgments()
    }

    /// Waits, at most what remains of `deadline`, until at least `count`
    /// `CONNECT` targets are recorded. True once they are; false at the
    /// deadline.
    pub fn await_connects(&self, count: usize, deadline: Deadline) -> bool {
        let within = deadline.left();
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.connects.len() < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.connects.len() >= count
    }

    /// Waits, at most what remains of `deadline`, until at least `count`
    /// connections saw their client side close. True once they did; false
    /// at the deadline.
    pub fn await_closed(&self, count: usize, deadline: Deadline) -> bool {
        let within = deadline.left();
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.closed < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.closed >= count
    }
}

/// A lock that outlives a panicked holder: the state is plain data, and a
/// test thread that panicked has already failed its test.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Serves one connection: anything but `CONNECT` gets a 405, and a
/// `CONNECT` hands its stream to the core's tunnel past the recorded
/// target. A close or shutdown ends the read silently, as the drop
/// wake-up does.
fn serve(mut conn: Connection, state: &Mutex<State>, arrived: &Condvar) {
    let request = match request::read_request(&mut conn) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof | RequestOutcome::Invalid(_) | RequestOutcome::Stopped => return,
    };
    if request.method != "CONNECT" {
        let _ = conn.reply_full(
            b"HTTP/1.1 405 Method Not Allowed\r\nconnection: close\r\ncontent-length: 0\r\n\r\n",
            b"",
            None,
        );
        return;
    }
    let target = request.target.clone();
    let origin = match conn.dial(&target) {
        Ok(origin) => origin,
        Err(_) => return,
    };
    {
        lock(state).connects.push(target.clone());
        arrived.notify_all();
    }
    if conn.tunnel(&target, origin) == TunnelEnd::ClientClosed {
        lock(state).closed += 1;
        arrived.notify_all();
    }
}

#[cfg(test)]
#[path = "connect_proxy_tests.rs"]
mod tests;
