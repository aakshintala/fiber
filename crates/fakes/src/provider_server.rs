//! The fake provider server (`docs/testing.md`, "Model calls"): a local
//! HTTP/1.1 server that answers each request with the next response of its
//! script and records every request it receives. Accepting, head reading
//! and every reply run on the shared HTTP core; only the script, the
//! credential-safe projection and the hold below are this fake's own.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use contract::clock::Clock;

use crate::clock::SystemClock;
use crate::deadline::Deadline;
#[cfg(test)]
use crate::http_server::WaitAck;
use crate::http_server::server::DropProbe;
use crate::http_server::{
    Connection, HttpServer, Limits, StallEnd,
    request::{ParsedRequest, RequestOutcome, read_request},
};

/// Header names whose values are credentials, lowercase. Their values are
/// replaced by a fingerprint before a request is recorded, as is a `key`
/// query parameter, which Google's API also accepts (`docs/model-routing.md`). Shared with the request reader's redacted `Debug`.
pub(crate) const CREDENTIAL_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
    "api-key",
    "cookie",
];

/// A credential's fingerprint: `sha256:` and the first 8 lowercase hex digits
/// of SHA-256 over `value`, so a recording can be asserted without holding
/// the credential.
pub fn fingerprint(value: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, value.as_bytes());
    let hex: String = digest
        .as_ref()
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// One scripted response: a status, headers and the body bytes, sent as they
/// are. A recorded stream is served by passing its bytes as the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status code.
    pub status: u16,
    /// Response headers in the order they are sent. The server adds
    /// `content-length` and `connection: close` itself.
    pub headers: Vec<(String, String)>,
    /// The body bytes, byte for byte.
    pub body: Vec<u8>,
    /// When true, the server records the request, then drops the
    /// connection without answering: the client sees a dropped
    /// connection. `status`, `headers` and `body` are ignored.
    pub drop_connection: bool,
    /// When true, the server sends the head with the headers as scripted
    /// (the script gives `content-length`) and `body` as the only body
    /// bytes, then holds the connection open until the client closes it.
    pub stall: bool,
}

impl Response {
    /// A 200 server-sent-events stream with these bytes as its body.
    pub fn stream(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
            body: body.into(),
            drop_connection: false,
            stall: false,
        }
    }

    /// A JSON response with this status, such as a 429 or a 500 error body.
    pub fn status(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.into(),
            drop_connection: false,
            stall: false,
        }
    }

    /// Drops the connection without answering, once the request is
    /// recorded: what a client that retries a dropped connection sees.
    pub fn drop_connection() -> Self {
        Self {
            status: 0,
            headers: Vec::new(),
            body: Vec::new(),
            drop_connection: true,
            stall: false,
        }
    }

    /// Sends the head with `total` as its `content-length` and `prefix` as
    /// the only body bytes, then holds the connection open until the client
    /// closes it: what a client blocked mid-body sees. `total` must exceed
    /// `prefix.len()`; the server records the partial send and the client's
    /// close for [`ProviderServer::await_partial`] and
    /// [`ProviderServer::await_closed`]. Chain `.header()` for the other
    /// headers, such as `content-type`.
    pub fn stall(status: u16, prefix: impl Into<Vec<u8>>, total: usize) -> Self {
        Self {
            status,
            headers: vec![("content-length".to_owned(), total.to_string())],
            body: prefix.into(),
            drop_connection: false,
            stall: true,
        }
    }

    /// The same response with one more header, such as `retry-after`.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// One request the server received, with credentials replaced by their
/// fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The method, such as `POST`.
    pub method: String,
    /// The request target: the path and any query, with a `key` query
    /// parameter's value replaced by its fingerprint.
    pub path: String,
    /// Headers in the order received, names lowercased, credential values
    /// replaced by their fingerprints.
    pub headers: Vec<(String, String)>,
    /// The body bytes, as received.
    pub body: Vec<u8>,
    /// The size of the body as received, in bytes.
    pub body_len: usize,
}

impl Request {
    /// The value of the first header with this lowercase name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// How long [`ProviderServer::hold`] keeps a response for its release: the
/// whole test budget, so a test's own deadline (`WAITS` inside `BUDGET`)
/// fails it before the fake answers a 500 the test never asked for.
const HELD_LIMIT: Duration = crate::deadline::BUDGET;

/// How long one poll inside a hold wait lasts.
const HOLD_POLL: Duration = Duration::from_millis(1);

/// Answers one request from its content: the recorded request, so an answer
/// can depend on the request's body or on an earlier request's.
type Responder = std::sync::Arc<dyn Fn(&Request) -> Response + Send + Sync>;

struct State {
    script: VecDeque<Response>,
    /// Responses claimed by request path: the first request to a path takes
    /// its route, ahead of the script.
    routes: Vec<(String, Response)>,
    /// Answers every request from its content, ahead of the routes, when set.
    responder: Option<Responder>,
    /// The response of every request past the script.
    fallback: Option<Response>,
    requests: Vec<Request>,
    /// When set, a recorded request keeps its headers and `body_len` but
    /// its body is emptied before it is stored.
    drop_bodies: bool,
    /// When set, a recorded request is not answered until [`ProviderServer::release`].
    hold: bool,
    /// The number (1-based) of the first request a hold applies to.
    hold_from: usize,
    /// How long a held response waits for its release.
    hold_within: Duration,
    /// Held responses [`ProviderServer::release_one`] has let go.
    permits: usize,
    /// Stalled responses that sent their partial body.
    partial: usize,
    /// Stalled connections whose client side closed.
    closed: usize,
    /// The clock holds and core budgets run on.
    clock: Arc<dyn Clock>,
    /// Where held-response waits announce themselves while a test listens.
    #[cfg(test)]
    acks: Option<mpsc::Sender<WaitAck>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            script: VecDeque::new(),
            routes: Vec::new(),
            responder: None,
            fallback: None,
            requests: Vec::new(),
            drop_bodies: false,
            hold: false,
            hold_from: 1,
            hold_within: HELD_LIMIT,
            permits: 0,
            partial: 0,
            closed: 0,
            clock: Arc::new(SystemClock),
            #[cfg(test)]
            acks: None,
        }
    }
}

/// How often [`ProviderServer::drop`] retries its hold release: one
/// `try_lock` can land while a serving thread holds the state lock
/// briefly on a hold poll or while entering its hold wait, so a single
/// attempt can miss and leave held responses unanswered. Spinning this
/// often still ends at once past a responder stuck under the lock.
const RELEASE_RETRIES: usize = 1_000;

/// A fake provider listening on a local port. Each request gets the next
/// response of the script, and a request past its end gets a 500 saying so.
/// Dropping the server closes its port.
pub struct ProviderServer {
    addr: SocketAddr,
    core: HttpServer,
    state: Arc<Mutex<State>>,
    arrived: Arc<Condvar>,
}

impl ProviderServer {
    /// Listens on a free port on 127.0.0.1 and serves `script` in order. The
    /// port accepts connections when this returns.
    pub fn start(script: impl IntoIterator<Item = Response>) -> io::Result<Self> {
        Self::start_with_fallback(script, no_scripted_response())
    }

    /// Like [`ProviderServer::start`], but every request past the script
    /// gets `fallback` instead of the 500 saying the script ran out.
    pub fn start_with_fallback(
        script: impl IntoIterator<Item = Response>,
        fallback: Response,
    ) -> io::Result<Self> {
        Self::start_with(
            State {
                script: script.into_iter().collect(),
                fallback: Some(fallback),
                ..State::default()
            },
            Arc::new(SystemClock),
        )
    }

    /// Like [`ProviderServer::start_with_fallback`], but each of `routes` is
    /// answered to the first request for its path (the target without its
    /// query), whenever that request arrives. A request no route claims gets
    /// `fallback`. For clients whose concurrent requests have no fixed order.
    pub fn start_routed<'a>(
        routes: impl IntoIterator<Item = (&'a str, Response)>,
        fallback: Response,
    ) -> io::Result<Self> {
        let server = Self::start_with_fallback([], fallback)?;
        lock(&server.state).routes = routes
            .into_iter()
            .map(|(path, response)| (path.to_owned(), response))
            .collect();
        Ok(server)
    }

    /// Like [`ProviderServer::start_routed`], but every request is answered
    /// by `responder`, called with the recorded request: an answer can
    /// depend on the request's body or on an earlier request's. `hold`,
    /// `release` and `release_one` gate a responded answer as they do a
    /// scripted one. The responder runs while the server holds its lock, so
    /// it must not call back into the server; it reads all it needs from
    /// the request.
    pub fn start_responding(
        responder: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::start_with(
            State {
                responder: Some(Arc::new(responder)),
                ..State::default()
            },
            Arc::new(SystemClock),
        )
    }

    /// Listens on a free port on 127.0.0.1 with `state` on `clock` and
    /// serves it: the one start path behind every constructor. The port
    /// accepts connections when this returns.
    fn start_with(state: State, clock: Arc<dyn Clock>) -> io::Result<Self> {
        let core = HttpServer::bind(Arc::clone(&clock), Limits::default())?;
        Ok(Self::serve_on(state, clock, core))
    }

    /// Serves `state` on `core`, bound on `clock`.
    fn serve_on(state: State, clock: Arc<dyn Clock>, core: HttpServer) -> Self {
        let addr = core.addr();
        let mut state = state;
        state.clock = Arc::clone(&clock);
        let server = Self {
            addr,
            core,
            state: Arc::new(Mutex::new(state)),
            arrived: Arc::new(Condvar::new()),
        };
        let shared = Arc::clone(&server.state);
        let wake = Arc::clone(&server.arrived);
        let ticking = clock;
        server
            .core
            .serve_persistent(move |conn| serve(conn, &shared, &wake, &ticking));
        server
    }

    /// [`ProviderServer::start`], on `clock`: the test drives timed holds
    /// without waiting them out. Test-only: production constructors run on
    /// the process clock.
    #[cfg(test)]
    pub(crate) fn start_on(clock: Arc<dyn Clock>, script: Vec<Response>) -> io::Result<Self> {
        Self::start_with(scripted(script), clock)
    }

    /// [`ProviderServer::start_on`], on a `core` the test bound on `clock`
    /// and set up before any accept: its limits and gate hold from the
    /// first window. Test-only.
    #[cfg(test)]
    pub(crate) fn start_on_core(
        clock: Arc<dyn Clock>,
        core: HttpServer,
        script: Vec<Response>,
    ) -> Self {
        Self::serve_on(scripted(script), clock, core)
    }

    /// Receives every held-response wait announcement from here on.
    #[cfg(test)]
    pub(crate) fn test_acknowledgments(&self) -> mpsc::Receiver<WaitAck> {
        let (tx, rx) = mpsc::channel();
        lock(&self.state).acks = Some(tx);
        rx
    }

    /// The base URL, such as `http://127.0.0.1:49152`, for a provider
    /// definition's base URL.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request received so far, in arrival order. A request is
    /// recorded before its response is sent, so once a client has its
    /// response the request is here.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.state).requests.clone()
    }

    /// Empties each request's body as it is recorded: the record keeps
    /// every request's method, path, headers and `body_len`, so its
    /// length stays exact, but the stored bodies hold no bytes. A
    /// responder still answers from the full body of the request it
    /// sees. For a run whose requests carry an ever-growing conversation
    /// and which asserts only the count, so the record never holds them
    /// all.
    pub fn drop_bodies(&self) {
        lock(&self.state).drop_bodies = true;
    }

    /// Waits, at most what remains of `deadline`, until at least `count`
    /// requests are recorded. True once they are; false at the deadline.
    /// Dropping the server ends no wait early.
    pub fn await_requests(&self, count: usize, deadline: Deadline) -> bool {
        let within = deadline.left();
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.requests.len() < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.requests.len() >= count
    }

    /// Holds every response until [`ProviderServer::release`]. A request is
    /// still recorded first, so [`ProviderServer::await_requests`] sees it
    /// while the client waits for the body. A held response never counts
    /// toward [`ProviderServer::await_closed`], which counts only stalled
    /// responses whose client closed.
    ///
    /// The default limit is `BUDGET`, longer than any wait (`WAITS`) a
    /// test makes, so the test's own deadline fails a hold nobody releases
    /// and the limit never fires first on a loaded machine. A test that
    /// relies on the 500 passes its own limit to [`ProviderServer::hold_from`].
    pub fn hold(&self) {
        self.hold_from(1, HELD_LIMIT);
    }

    /// As [`ProviderServer::hold`], for the `number`th request and every
    /// later one (1-based): earlier requests are answered at once. A held
    /// response nobody releases within `within` is answered with a 500
    /// naming the hold, so a test that never releases fails instead of
    /// hanging.
    pub fn hold_from(&self, number: usize, within: Duration) {
        let mut state = lock(&self.state);
        state.hold = true;
        state.hold_from = number;
        state.hold_within = within;
    }

    /// Sends the responses [`ProviderServer::hold`] is holding.
    pub fn release(&self) {
        lock(&self.state).hold = false;
        self.arrived.notify_all();
    }

    /// Lets one held response go, while the hold stays on for the rest: the
    /// next response sent is the one of the oldest request waiting, or of
    /// the next request to arrive.
    pub fn release_one(&self) {
        lock(&self.state).permits += 1;
        self.arrived.notify_all();
    }

    /// Waits, at most what remains of `deadline`, until at least `count`
    /// stalled responses have sent their partial body. True once they have;
    /// false at the deadline.
    pub fn await_partial(&self, count: usize, deadline: Deadline) -> bool {
        let within = deadline.left();
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.partial < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.partial >= count
    }

    /// Waits, at most what remains of `deadline`, until at least `count`
    /// stalled connections saw their client side close. True once they did;
    /// false at the deadline.
    pub fn await_closed(&self, count: usize, deadline: Deadline) -> bool {
        let within = deadline.left();
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.closed < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.closed >= count
    }

    /// The dropped server's own state, read after the drop: take it
    /// before the drop, since the server moves into the drop. The probe
    /// holds the shared core, so it stays valid past the drop.
    #[doc(hidden)]
    pub fn drop_probe(&self) -> DropProbe {
        self.core.drop_probe()
    }
}

impl Drop for ProviderServer {
    fn drop(&mut self) {
        // Release what the holds are holding before reaping: a held
        // response is answered, and every serving thread ends its core
        // wait stopped on its own. A responder stuck under the state
        // lock is not waited for: the release is tried, never blocking
        // the drop, and the core's bounded shutdown reaps the rest.
        // One attempt can miss while a serving thread holds the lock
        // on a hold poll or entering its wait, so retry briefly: a
        // responder past that still ends the retries, never the drop.
        for _ in 0..RELEASE_RETRIES {
            if let Ok(mut state) = self.state.try_lock() {
                state.hold = false;
                break;
            }
            std::thread::yield_now();
        }
        self.arrived.notify_all();
        self.core.shutdown();
    }
}

/// A lock that outlives a panicked holder: the state is plain data, and a
/// test thread that panicked has already failed its test.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the closing adapter discards already-arrived pipelined bytes
/// past a declared fixed body so the close delivers the reply instead of
/// resetting. A pure function, so a unit test pins each side directly:
/// every scripted request carries several headers and no scripted chunked
/// request carries content-length, so no socket test distinguishes either
/// operator.
fn discards_pipelined(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, _)| name == "content-length")
        && !headers.iter().any(|(name, _)| name == "transfer-encoding")
}

/// Serves one connection: records the request under the state lock, then
/// answers from the script, the routes, the responder or the fallback. A
/// malformed body is the client's bug: it is recorded with its decoded
/// prefix and answered 400, and the script keeps its next response.
fn serve(conn: Connection, state: &Mutex<State>, arrived: &Condvar, clock: &Arc<dyn Clock>) {
    let mut conn = conn;
    let request = match read_request(&mut conn) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof | RequestOutcome::Invalid(_) | RequestOutcome::Stopped => return,
    };
    if discards_pipelined(&request.headers) {
        conn.drain_socket();
    }
    let malformed = request.malformed;
    let response = {
        let mut state = lock(state);
        // Cloned before the record moves it, and only when a responder
        // reads the request it answers: the scripted path never does, and
        // cloning every ever-growing body here copies tens of gigabytes a
        // pass (see #2188).
        let answered = state
            .responder
            .clone()
            .map(|responder| (responder, project(&request)));
        let mut record = project(&request);
        if state.drop_bodies {
            record.body = Vec::new();
        }
        let request_path = record.path.clone();
        state.requests.push(record);
        arrived.notify_all();
        match malformed {
            Some(why) => Response::status(
                400,
                format!(r#"{{"error":"fakes: malformed chunked body: {why}"}}"#),
            ),
            None => match answered {
                Some((responder, answered)) => responder(&answered),
                None => {
                    let target = request_path.split('?').next().unwrap_or_default();
                    let route = state.routes.iter().position(|(path, _)| path == target);
                    route
                        .map(|index| state.routes.remove(index).1)
                        .or_else(|| state.script.pop_front())
                        .or_else(|| state.fallback.clone())
                        .unwrap_or_else(no_scripted_response)
                }
            },
        }
    };
    if response.drop_connection {
        // Recorded above; the open stream drops here, so the client
        // sees a connection closed with no response.
        return;
    }
    if let Some(expired) = hold_wait(state, arrived, clock) {
        let _ = conn.reply_full(head(&expired, true).as_bytes(), &expired.body, None);
        return;
    }
    if response.stall {
        if !conn.stall_send(head(&response, false).as_bytes(), &response.body, None) {
            return;
        }
        {
            lock(state).partial += 1;
            arrived.notify_all();
        }
        if conn.stall_wait() == StallEnd::Closed {
            lock(state).closed += 1;
            arrived.notify_all();
        }
        return;
    }
    let _ = conn.reply_full(head(&response, true).as_bytes(), &response.body, None);
}

/// Whether a held response may go: released, permitted or dropped. A
/// pure function, so a unit test pins each side directly: the hold's
/// millisecond re-poll observes every release without it, so no socket
/// test distinguishes either comparison.
fn hold_released(hold: bool, permits: usize) -> bool {
    !hold || permits > 0
}

/// Holds a recorded response until its release, permit or the server's
/// drop: `None` once it may go, the 500 naming the hold when nobody
/// releases it within its bound. The bound runs on the injected clock,
/// announced once so the test advances past it only when waited for.
fn hold_wait(state: &Mutex<State>, arrived: &Condvar, clock: &Arc<dyn Clock>) -> Option<Response> {
    let until = {
        let state = lock(state);
        if !(state.hold && state.requests.len() >= state.hold_from) {
            return None;
        }
        clock.now().checked_add(state.hold_within)
    };
    // One announcement: the waiter is past its check and parked before
    // the test advances.
    #[cfg(test)]
    if let Some(acks) = lock(state).acks.as_ref() {
        match acks.send(WaitAck {
            phase: "to wait out the held response",
            thread: std::thread::current().id(),
            seq: 1,
            until,
        }) {
            Ok(()) | Err(_) => {}
        }
    }
    let mut guard = lock(state);
    loop {
        if !guard.hold {
            return None;
        }
        if guard.permits > 0 {
            guard.permits -= 1;
            return None;
        }
        // The bound wins over a simultaneous release, as the
        // real-clock wait it replaces did.
        if until.is_some_and(|until| clock.now() >= until) {
            return Some(Response::status(
                500,
                r#"{"error":"fakes: a held response was never released"}"#,
            ));
        }
        let predicate = |state: &mut State| hold_released(state.hold, state.permits);
        guard = support::clock::park(
            clock.as_ref(),
            until,
            Some(HOLD_POLL),
            arrived,
            guard,
            predicate,
        )
        .unwrap_or_else(|| lock(state));
    }
}

/// A test script's state: `script` in order, then the 500 saying it ran
/// out.
#[cfg(test)]
fn scripted(script: Vec<Response>) -> State {
    State {
        script: script.into_iter().collect(),
        fallback: Some(no_scripted_response()),
        ..State::default()
    }
}

fn no_scripted_response() -> Response {
    Response::status(
        500,
        r#"{"error":"fakes: no scripted response left for this request"}"#,
    )
}

/// The recorded request: credentials fingerprinted, exact `body_len`.
pub(crate) fn project(request: &ParsedRequest) -> Request {
    Request {
        method: request.method.clone(),
        path: fingerprint_query(&request.target),
        headers: request
            .headers
            .iter()
            .map(|(name, value)| {
                if CREDENTIAL_HEADERS.contains(&name.as_str()) {
                    (name.clone(), fingerprint(value))
                } else {
                    (name.clone(), value.clone())
                }
            })
            .collect(),
        body_len: request.body.len(),
        body: request.body.clone(),
    }
}

/// The response head: the status line, the scripted headers, the body's
/// measured length when `with_length`, and `connection: close`. A stalled
/// response carries its scripted `content-length` instead, since its body
/// is only a prefix of what it declares.
fn head(response: &Response, with_length: bool) -> String {
    let mut head = format!("HTTP/1.1 {} Fake\r\n", response.status);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if with_length {
        head.push_str(&format!("content-length: {}\r\n", response.body.len()));
    }
    head.push_str("connection: close\r\n\r\n");
    head
}

/// The request target with any `key` query parameter's value replaced by
/// its fingerprint. Shared with the request reader's redacted `Debug`.
pub(crate) fn fingerprint_query(target: &str) -> String {
    let Some((path, query)) = target.split_once('?') else {
        return target.to_owned();
    };
    let query: Vec<String> = query
        .split('&')
        .map(|param| match param.split_once('=') {
            Some(("key", value)) => format!("key={}", fingerprint(value)),
            _ => param.to_owned(),
        })
        .collect();
    format!("{path}?{}", query.join("&"))
}

#[cfg(test)]
#[path = "provider_server_tests.rs"]
mod tests;
