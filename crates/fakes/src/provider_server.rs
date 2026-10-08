//! The fake provider server (`docs/testing.md`, "Model calls"): a local
//! HTTP/1.1 server that answers each request with the next response of its
//! script and records every request it receives.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Header names whose values are credentials, lowercase. Their values are
/// replaced by a fingerprint before a request is recorded, as is a `key`
/// query parameter, which Google's API also accepts (`docs/model-routing.md`).
const CREDENTIAL_HEADERS: [&str; 6] = [
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
    /// The body bytes, as received. Empty once the request is older than the
    /// server's body limit ([`ProviderServer::keep_last_bodies`]); `body_len`
    /// still gives its size.
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

/// How many of the newest requests keep their bodies unless a test asks for
/// more.
const DEFAULT_BODY_LIMIT: usize = 64;

struct State {
    script: VecDeque<Response>,
    /// Responses claimed by request path: the first request to a path takes
    /// its route, ahead of the script.
    routes: Vec<(String, Response)>,
    /// The response of every request past the script.
    fallback: Option<Response>,
    requests: Vec<Request>,
    /// How many of the newest requests keep their bodies; `None` keeps all.
    body_limit: Option<usize>,
    stopping: bool,
    /// When set, a recorded request is not answered until [`ProviderServer::release`].
    hold: bool,
    /// Held responses [`ProviderServer::release_one`] has let go.
    permits: usize,
    /// Stalled responses that sent their partial body.
    partial: usize,
    /// Stalled connections whose client side closed.
    closed: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            script: VecDeque::new(),
            routes: Vec::new(),
            fallback: None,
            requests: Vec::new(),
            body_limit: Some(DEFAULT_BODY_LIMIT),
            stopping: false,
            hold: false,
            permits: 0,
            partial: 0,
            closed: 0,
        }
    }
}

impl State {
    /// Records `request`, then drops the body of the one request that has
    /// just fallen out of the body limit.
    fn record(&mut self, request: Request) {
        self.requests.push(request);
        if let Some(limit) = self.body_limit
            && let Some(old) = self
                .requests
                .len()
                .checked_sub(limit)
                .and_then(|n| n.checked_sub(1))
            && let Some(request) = self.requests.get_mut(old)
        {
            request.body = Vec::new();
        }
    }
}

/// A fake provider listening on a local port. Each request gets the next
/// response of the script, and a request past its end gets a 500 saying so.
/// Dropping the server closes its port.
pub struct ProviderServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    arrived: Arc<Condvar>,
    accept: Option<JoinHandle<()>>,
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
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State {
            script: script.into_iter().collect(),
            fallback: Some(fallback),
            ..State::default()
        }));
        let arrived = Arc::new(Condvar::new());
        let shared = Arc::clone(&state);
        let wake = Arc::clone(&arrived);
        let accept = thread::Builder::new()
            .name("fake-provider".to_owned())
            .spawn(move || accept_loop(&listener, &shared, &wake))?;
        Ok(Self {
            addr,
            state,
            arrived,
            accept: Some(accept),
        })
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

    /// The base URL, such as `http://127.0.0.1:49152`, for a provider
    /// definition's base URL.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Keeps full bodies for only the newest `limit` requests (64 by
    /// default); older requests keep their metadata and `body_len`. Call it
    /// before the first request arrives.
    #[must_use]
    pub fn keep_last_bodies(self, limit: usize) -> Self {
        lock(&self.state).body_limit = Some(limit);
        self
    }

    /// Keeps every request's full body, for a test that reads back more than
    /// the default limit.
    #[must_use]
    pub fn keep_all_bodies(self) -> Self {
        lock(&self.state).body_limit = None;
        self
    }

    /// Every request received so far, in arrival order. A request is
    /// recorded before its response is sent, so once a client has its
    /// response the request is here.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.state).requests.clone()
    }

    /// Waits, at most `within` of real time, until at least `count` requests
    /// are recorded. True once they are; false at the deadline. Dropping the
    /// server ends no wait early.
    pub fn await_requests(&self, count: usize, within: Duration) -> bool {
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| state.requests.len() < count)
            .unwrap_or_else(PoisonError::into_inner);
        guard.requests.len() >= count
    }

    /// Holds every response until [`ProviderServer::release`]. A request is
    /// still recorded first, so [`ProviderServer::await_requests`] sees it
    /// while the client waits for the body.
    pub fn hold(&self) {
        lock(&self.state).hold = true;
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

    /// Waits, at most `within` of real time, until at least `count` stalled
    /// responses have sent their partial body. True once they have; false
    /// at the deadline.
    pub fn await_partial(&self, count: usize, within: Duration) -> bool {
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| fewer_than(state.partial, count))
            .unwrap_or_else(PoisonError::into_inner);
        guard.partial >= count
    }

    /// Waits, at most `within` of real time, until at least `count` stalled
    /// connections saw their client side close. True once they did; false
    /// at the deadline.
    pub fn await_closed(&self, count: usize, within: Duration) -> bool {
        let guard = lock(&self.state);
        let (guard, _) = self
            .arrived
            .wait_timeout_while(guard, within, |state| fewer_than(state.closed, count))
            .unwrap_or_else(PoisonError::into_inner);
        guard.closed >= count
    }
}

impl Drop for ProviderServer {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.state);
            state.stopping = true;
            state.hold = false;
        }
        self.arrived.notify_all();
        if let Some(accept) = self.accept.take() {
            stop(self.addr, accept, STOP_DEADLINE);
        }
    }
}

/// How long dropping a [`ProviderServer`] waits on the wall clock for its
/// accept thread to stop. A connection wakes the thread at once; the bound
/// only stops a drop from hanging on one that never wakes.
const STOP_DEADLINE: Duration = Duration::from_secs(2);

/// Wakes the accept thread, which has seen `stopping`, by connecting to
/// `addr`, then joins it. One thread makes the connection and the join, and
/// one deadline bounds both. Returns whether the thread was joined: a
/// connection that fails or a join that misses `deadline` leaves the thread
/// blocked rather than waiting forever. Builds abort on panic
/// (`docs/code-quality.md`, "Panics"), so a join never carries one.
fn stop(addr: SocketAddr, accept: JoinHandle<()>, deadline: Duration) -> bool {
    let (done, joined) = mpsc::channel::<()>();
    thread::spawn(move || {
        if TcpStream::connect(addr).is_ok() && accept.join().is_ok() {
            match done.send(()) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    joined.recv_timeout(deadline).is_ok()
}

/// Whether `have` arrivals are still fewer than the `count` waited for.
/// The boundary is exact: below the count the wait continues, at it the
/// wait is already over, so `==`, `>` and `<=` here each read differently.
fn fewer_than(have: usize, count: usize) -> bool {
    have < count
}

/// A lock that outlives a panicked holder: the state is plain data, and a
/// test thread that panicked has already failed its test.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn accept_loop(listener: &TcpListener, state: &Arc<Mutex<State>>, arrived: &Arc<Condvar>) {
    for stream in listener.incoming() {
        if lock(state).stopping {
            return;
        }
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(state);
        let arrived = Arc::clone(arrived);
        // One thread per connection, so a client holding a connection open
        // never stalls another.
        let spawned = thread::Builder::new()
            .name("fake-provider-conn".to_owned())
            .spawn(move || {
                // A connection that breaks mid-request has already failed its
                // client, and there is no complete request to record.
                match serve(stream, &state, &arrived) {
                    Ok(()) | Err(_) => {}
                }
            });
        // Out of threads: the dropped connection fails its client.
        if spawned.is_err() {
            continue;
        }
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>, arrived: &Condvar) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let (request, malformed) = read_request(&mut reader)?;
    let request_path = request.path.clone();
    let response = {
        let mut state = lock(state);
        state.record(request);
        arrived.notify_all();
        // A malformed body is the client's bug: it gets a 400 and the script
        // keeps its next response.
        let response = match malformed {
            Some(why) => Response::status(
                400,
                format!(r#"{{"error":"fakes: malformed chunked body: {why}"}}"#),
            ),
            None => {
                let target = request_path.split('?').next().unwrap_or_default();
                let route = state.routes.iter().position(|(path, _)| path == target);
                route
                    .map(|index| state.routes.remove(index).1)
                    .or_else(|| state.script.pop_front())
                    .or_else(|| state.fallback.clone())
                    .unwrap_or_else(no_scripted_response)
            }
        };
        if response.drop_connection {
            // Recorded above; the open stream drops here, so the client
            // sees a connection closed with no response.
            return Ok(());
        }
        if state.hold {
            while state.hold && state.permits == 0 && !state.stopping {
                state = arrived.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
            // A still-held request takes one released permit. The wait
            // above only ends with no permit left when the hold is already
            // gone or the server is stopping, which clears the hold first,
            // so `||` or `>=` here would only re-check a ruled-out state;
            // the pattern states the rule with mutants a test can reach.
            if let (true, 1..) = (state.hold, state.permits) {
                state.permits -= 1;
            }
        }
        response
    };
    if response.stall {
        return write_stall(reader.get_mut(), &response, state, arrived);
    }
    write_response(reader.get_mut(), &response)
}

fn no_scripted_response() -> Response {
    Response::status(
        500,
        r#"{"error":"fakes: no scripted response left for this request"}"#,
    )
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// The request, and why its body's framing is malformed if it is.
fn read_request(reader: &mut impl BufRead) -> io::Result<(Request, Option<Malformed>)> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(invalid("no request line"));
    };
    let (method, path) = (method.to_owned(), fingerprint_query(target));

    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(invalid("headers end before a blank line"));
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| invalid("header without a colon"))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        let value = if CREDENTIAL_HEADERS.contains(&name.as_str()) {
            fingerprint(value)
        } else {
            value.to_owned()
        };
        headers.push((name, value));
    }

    let chunked = headers
        .iter()
        // A request with any transfer coding ends in chunked (RFC 9112,
        // section 6.1).
        .any(|(n, _)| n == "transfer-encoding");
    let mut malformed = None;
    let body = if chunked {
        let mut body = Vec::new();
        malformed = read_chunked(reader, &mut body).err();
        body
    } else {
        let length = headers
            .iter()
            .find(|(n, _)| n == "content-length")
            .map_or(Ok(0), |(_, v)| v.parse::<usize>())
            .map_err(|_| invalid("content-length is not a number"))?;
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        body
    };
    Ok((
        Request {
            method,
            path,
            headers,
            body_len: body.len(),
            body,
        },
        malformed,
    ))
}

/// Why a chunked request body could not be decoded.
type Malformed = &'static str;

const TRUNCATED: Malformed = "the chunked body ends before its framing does";

/// Decodes a chunked body (RFC 9112, section 7.1) into `body`: each chunk's
/// bytes in order, with chunk extensions and trailers dropped. On malformed
/// or truncated framing, `body` holds what decoded before it.
fn read_chunked(reader: &mut impl BufRead, body: &mut Vec<u8>) -> Result<(), Malformed> {
    loop {
        let line = framing_line(reader)?;
        let size = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size, 16).map_err(|_| "a chunk size is not a hex number")?;
        if size == 0 {
            break;
        }
        // `take` rather than a buffer of `size`, so a huge declared size
        // cannot allocate before a byte arrives.
        let read = reader
            .by_ref()
            .take(size)
            .read_to_end(body)
            .map_err(|_| TRUNCATED)?;
        if u64::try_from(read) != Ok(size) {
            return Err(TRUNCATED);
        }
        if !framing_line(reader)?.is_empty() {
            return Err("a chunk's data is not followed by CRLF");
        }
    }
    // Trailers, up to the blank line that ends the body. They are read so
    // the client's bytes are all consumed before the connection closes.
    while !framing_line(reader)?.is_empty() {}
    Ok(())
}

/// One line of chunked framing, without its CRLF.
fn framing_line(reader: &mut impl BufRead) -> Result<String, Malformed> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => Err(TRUNCATED),
        Ok(_) if !line.ends_with('\n') => Err(TRUNCATED),
        Ok(_) => line
            .strip_suffix("\r\n")
            .map(str::to_owned)
            .ok_or("a chunked framing line does not end in CRLF"),
    }
}

/// The request target with any `key` query parameter's value replaced by
/// its fingerprint.
fn fingerprint_query(target: &str) -> String {
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

fn write_response(stream: &mut TcpStream, response: &Response) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} Fake\r\n", response.status);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        response.body.len()
    ));
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// Sends the head with its declared `content-length` and the prefix it
/// carries, then holds the connection open until the client closes it: the
/// client stays blocked mid-body. Records the partial send before holding
/// and the client's close after it, so a test can wait for each.
fn write_stall(
    stream: &mut TcpStream,
    response: &Response,
    state: &Mutex<State>,
    arrived: &Condvar,
) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} Fake\r\n", response.status);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()?;
    {
        lock(state).partial += 1;
        arrived.notify_all();
    }
    // Held until the client goes away: a zero read is the close.
    let mut rest = Vec::new();
    match stream.read_to_end(&mut rest) {
        Ok(_) | Err(_) => {}
    }
    lock(state).closed += 1;
    arrived.notify_all();
    Ok(())
}

#[cfg(test)]
#[path = "provider_server_tests.rs"]
mod tests;
