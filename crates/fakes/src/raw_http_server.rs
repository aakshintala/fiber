//! A raw-reply HTTP fake: it serves one scripted byte string for every
//! complete request and records each request with the shared
//! credential-safe projection, so a test can pin exact wire bytes a
//! shaped fake cannot send, such as a reply with no measured length or a
//! declared length the bytes do not carry. Accepting, head reading and
//! every reply run on the shared HTTP core; only the fixed reply and the
//! records below are this fake's own.
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
use std::sync::{Arc, Condvar, Mutex, PoisonError};
/// A lock that outlives a panicked holder: the state is plain data, and a
/// test thread that panicked has already failed its test.
use support::lock;

use crate::clock::SystemClock;
use crate::deadline::Deadline;
use crate::http_server::server::DropProbe;
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
    /// Replies recorded but not yet finished: incremented when a request
    /// is recorded, decremented after its send attempt ends on every
    /// path, so [`RawHttpServer::await_idle`] observes the attempt's end.
    pending: usize,
}

impl State {
    fn new() -> Self {
        Self {
            requests: Vec::new(),
            completed: 0,
            pending: 0,
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

    /// Waits, at most what remains of `deadline`, until at least `count`
    /// replies are fully flushed. True once they are; false at the
    /// deadline. Zero admits at once. Dropping the server ends no wait
    /// early.
    // exempted: waiting while completed <= count instead of < count only waits longer once satisfied; the trailing >= verdict returns the same value, so no test can observe the difference without a timing assert (docs/testing.md "Waits and timeouts"); same wait/verdict shape as the established ProviderServer awaits.
    #[cfg_attr(false, mutants::skip)]
    pub fn await_replies(&self, count: usize, deadline: Deadline) -> bool {
        let within = deadline.left();
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.completed < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.completed >= count
    }

    /// Waits, at most what remains of `deadline`, until no recorded reply
    /// is still unfinished. True once every send attempt ended; false at
    /// the deadline. Idle admits at once.
    // exempted: waiting while pending >= 0 instead of > 0 only waits longer once satisfied; the trailing == verdict returns the same value, so no test can observe the difference without a timing assert (docs/testing.md "Waits and timeouts"); same wait/verdict shape as await_replies above. Both verdict directions stay pinned by the held zero check and the interrupt idle wait.
    #[cfg_attr(false, mutants::skip)]
    pub fn await_idle(&self, deadline: Deadline) -> bool {
        let within = deadline.left();
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.pending > 0)
            .unwrap_or_else(PoisonError::into_inner);
        guard.pending == 0
    }

    /// The dropped server's own state, read after the drop: take it
    /// before the drop, since the server moves into the drop. The probe
    /// holds the shared core, so it stays valid past the drop.
    #[doc(hidden)]
    pub fn drop_probe(&self) -> DropProbe {
        self.core.drop_probe()
    }
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
        state.pending += 1;
        arrived.notify_all();
    }
    // The lock is free while the bytes go out, so a slow peer never
    // holds the records; the handler's return closes the connection.
    let end = conn.reply_full(reply, b"", None);
    {
        let mut state = lock(state);
        state.pending -= 1;
        if end == ReplyEnd::Done {
            state.completed += 1;
        }
        arrived.notify_all();
    }
}
