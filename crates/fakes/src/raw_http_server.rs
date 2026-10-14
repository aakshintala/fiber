//! A raw-reply HTTP fake: it serves one scripted byte string for every
//! complete request and records each request with the shared
//! credential-safe projection, so a test can pin exact wire bytes a
//! shaped fake cannot send, such as a reply with no measured length or a
//! declared length the bytes do not carry. Accepting, head reading and
//! every reply run on the shared HTTP core; only the fixed reply, the
//! records and the hold below are this fake's own.
//!
//! A complete request is recorded before its reply is sent, so once a
//! client holds the whole reply the request is in
//! [`RawHttpServer::requests`]. A truncated chunked body still completes
//! with its decoded prefix: the record keeps that prefix and the reply
//! stays the fixed bytes, with no malformed-body policy of its own. A
//! read that ends in EOF, an invalid head or shutdown is recorded and
//! answered nowhere.

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::clock::SystemClock;
use crate::http_server::{
    Connection, HttpServer, Limits, ReplyEnd,
    request::{RequestOutcome, read_request},
};
use crate::provider_server::{Request, project};

struct State {
    /// Every complete request in arrival order, recorded before its reply.
    requests: Vec<Request>,
    /// Replies fully flushed so far: only a send that ran to its end
    /// counts, so [`RawHttpServer::await_replies`] never claims a reply
    /// the peer cut short.
    completed: usize,
    /// While set, a recorded request waits for
    /// [`RawHttpServer::release_replies`] before its reply is sent, so a
    /// test can reset the peer between the record and the send.
    hold: bool,
}

impl State {
    fn new() -> Self {
        Self {
            requests: Vec::new(),
            completed: 0,
            hold: false,
        }
    }
}

/// A fake HTTP server listening on a local port, serving one fixed reply
/// for every complete request. Dropping the server closes its port.
pub struct RawHttpServer {
    addr: SocketAddr,
    core: HttpServer,
    state: Arc<Mutex<State>>,
    arrived: Arc<Condvar>,
}

impl RawHttpServer {
    /// Listens on a free port on 127.0.0.1 and serves `reply` for every
    /// complete request. The port accepts connections when this returns.
    /// The bytes go as supplied, with no parsing, framing or headers
    /// added; the connection closes behind the send, so a client reading
    /// to EOF sees exactly them. Trusted test fixture input, like every
    /// other fake's script.
    pub fn start(reply: impl Into<Vec<u8>>) -> io::Result<Self> {
        let core = HttpServer::bind(Arc::new(SystemClock), Limits::default())?;
        let addr = core.addr();
        let server = Self {
            addr,
            core,
            state: Arc::new(Mutex::new(State::new())),
            arrived: Arc::new(Condvar::new()),
        };
        let reply = std::sync::Arc::new(reply.into());
        let shared = Arc::clone(&server.state);
        let wake = Arc::clone(&server.arrived);
        server.core.serve_persistent(move |conn| {
            serve(conn, &reply, &shared, &wake);
        });
        Ok(server)
    }

    /// The base URL, such as `http://127.0.0.1:49152`, for a client or a
    /// proxy definition.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The local port the server listens on, for proxy and pinned-address
    /// setup.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Every complete request received so far, in arrival order. A request
    /// is recorded before its reply is sent, so once a client has its
    /// reply the request is here.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.state).requests.clone()
    }

    /// Holds every reply after its request is recorded, until
    /// [`RawHttpServer::release_replies`]: the record is visible while
    /// the peer waits for bytes that never come.
    pub fn hold_replies(&self) {
        lock(&self.state).hold = true;
    }

    /// Sends the replies [`RawHttpServer::hold_replies`] is holding.
    pub fn release_replies(&self) {
        lock(&self.state).hold = false;
        self.arrived.notify_all();
    }

    /// Waits, at most `within` of real time, until at least `count`
    /// replies are fully flushed. True once they are; false at the
    /// deadline. Zero admits at once. Dropping the server ends no wait
    /// early.
    // exempted: waiting while completed <= count instead of < count only waits longer once satisfied; the trailing >= verdict returns the same value, so no test can observe the difference without a timing assert (docs/testing.md "Waits and timeouts"); same wait/verdict shape as the established ProviderServer awaits.
    #[cfg_attr(false, mutants::skip)]
    pub fn await_replies(&self, count: usize, within: Duration) -> bool {
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.completed < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.completed >= count
    }
}

impl Drop for RawHttpServer {
    fn drop(&mut self) {
        // Release held replies before reaping: a held reply is sent, and
        // every serving thread ends past the core's bounded shutdown. The
        // lock is free before the shutdown joins, so no serving thread
        // waits on this drop.
        lock(&self.state).hold = false;
        self.arrived.notify_all();
        self.core.shutdown();
    }
}

/// A lock that outlives a panicked holder: the state is plain data, and a
/// test thread that panicked has already failed its test.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Serves one connection: records a complete request, then sends the
/// fixed reply. An EOF, invalid or stopped read ends silently, with no
/// record and no reply.
fn serve(conn: Connection, reply: &[u8], state: &Mutex<State>, arrived: &Condvar) {
    let mut conn = conn;
    let request = match read_request(&mut conn) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof | RequestOutcome::Invalid(_) | RequestOutcome::Stopped => return,
    };
    {
        let mut state = lock(state);
        state.requests.push(project(&request));
        arrived.notify_all();
        while state.hold {
            state = arrived.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
    // The lock is free while the bytes go out, so a slow peer never
    // holds the records; the handler's return closes the connection.
    if conn.reply_full(reply, b"", None) == ReplyEnd::Done {
        lock(state).completed += 1;
        arrived.notify_all();
    }
}
