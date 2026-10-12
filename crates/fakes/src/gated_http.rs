//! The gated streamable-HTTP MCP fake (`docs/testing.md`, "Fakes"): a raw
//! HTTP/1.1 server whose `tools/call` (or `resources/read`) stream is gated.
//! The handshake and `tools/list` are answered as JSON, and the gated
//! stream carries the elicitation first and the gated answer only after the
//! client's POST of the elicitation's answer arrives. The ask is therefore
//! always observed before the answer lands: no scheduler order can flip the
//! elicitation to the remover's decline. Every request body is recorded in
//! arrival order.
//!
//! Raw sockets rather than [`crate::ProviderServer`] because the gate holds
//! one connection open while another request arrives on a new one, which a
//! scripted next-response server cannot express.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::clock::SystemClock;

/// How long the fake waits for each request, on the wall clock: longer than
/// [`crate::MUST_SUCCEED_WITHIN`], so a test's own deadline names the
/// failure first on a slow machine, and the fake never blocks indefinitely.
/// On expiry the serving thread fails naming the request it waited for.
const WAIT: Duration = Duration::from_secs(30);

/// The pause between accept polls: a poll pause inside the loop, not a
/// deadline of its own (`docs/testing.md`, "Waits and timeouts").
const POLL: Duration = Duration::from_millis(1);

/// The process clock the fake bounds its requests on: wall-clock time, so
/// a trickle of bytes cannot stretch a wait past its bound
/// (`docs/testing.md`, "Waits and timeouts").
static PROCESS_CLOCK: SystemClock = SystemClock;

/// One bound for one accepted request (`docs/testing.md`, "Waits and
/// timeouts"): made when the fake accepts the connection, it covers the
/// accept wait that found it, its headers, its body and its answers.
/// Every read and write on the request's stream takes only what remains
/// of it, so repeated headers or trickled bytes cannot stretch the
/// request past the bound the way one timeout per read could.
#[derive(Clone, Copy)]
struct RequestDeadline {
    /// The clock's `now()` when the fake accepted the connection.
    start: Instant,
    /// The clock the bound runs on: the process clock in the fake, an
    /// injected clock in tests.
    clock: &'static dyn Clock,
}

impl RequestDeadline {
    /// The bound for a connection accepted now: [`WAIT`] from the clock's
    /// `now()`.
    fn start_on(clock: &'static dyn Clock) -> Self {
        Self {
            start: clock.now(),
            clock,
        }
    }

    /// What remains of the bound: zero once [`WAIT`] has passed on the
    /// clock. It never panics; a read or write handed zero fails naming
    /// the request instead of touching the socket.
    fn left(&self) -> Duration {
        WAIT.saturating_sub(self.clock.now().saturating_duration_since(self.start))
    }
}

/// The session id the fake answers `initialize` with.
const SESSION: &str = "1868a90c";

/// Which handshake the fake answers: the `tools/call` shape lists one echo
/// tool under a tools-only capability, and the `resources/read` shape lists
/// no tools under tools and resources. The wire behaviour either test
/// asserts is unchanged from the per-test fakes this replaces.
#[derive(Clone, Copy)]
enum Shape {
    /// Serves one gated `tools/call` elicitation.
    Call,
    /// Serves one gated `resources/read` elicitation.
    Read,
}

impl Shape {
    /// The `capabilities` of the `initialize` answer.
    fn capabilities(self) -> Value {
        match self {
            Self::Call => json!({"tools": {}}),
            Self::Read => json!({"tools": {}, "resources": {}}),
        }
    }

    /// The `result` of the `tools/list` answer.
    fn listed(self) -> Value {
        match self {
            Self::Call => json!({"tools": [{
                "name": "echo",
                "description": "Echoes.",
                "inputSchema": {"type": "object"},
            }]}),
            Self::Read => json!({"tools": []}),
        }
    }

    /// What the gated stream carries, for wait names.
    fn gated(self) -> &'static str {
        match self {
            Self::Call => "the gated call's stream",
            Self::Read => "the gated read's stream",
        }
    }
}

/// A raw streamable-HTTP fake whose gated stream carries one `elicitation`,
/// answered with `result` once the elicitation is answered. Dropping it
/// stops nothing: the serving thread ends at the client's goodbye `DELETE`,
/// or fails at [`WAIT`] naming the request it waited for.
pub struct GatedHttp {
    /// The fake's `/mcp` URL.
    url: String,
    /// Every request body, in arrival order.
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl GatedHttp {
    /// Starts the fake serving one `tools/call` `elicitation`, answered
    /// with `answer` once the elicitation is answered.
    pub fn start_call(elicitation: String, answer: Value) -> Self {
        Self::start(elicitation, answer, Shape::Call, &PROCESS_CLOCK)
    }

    /// Starts the fake serving one `resources/read` `elicitation`,
    /// answered with `contents` once the elicitation is answered.
    pub fn start_read(elicitation: String, contents: Value) -> Self {
        Self::start(elicitation, contents, Shape::Read, &PROCESS_CLOCK)
    }

    /// The fake's `/mcp` URL.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// The elicitation answer bodies: every recorded body with
    /// `id == "el-1"`, in arrival order.
    pub fn elicitation_answers(&self) -> Vec<Value> {
        // Plain data: a panicked holder has already failed its test.
        self.bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(|body| serde_json::from_slice::<Value>(body).ok())
            .filter(|body| body.get("id") == Some(&json!("el-1")))
            .collect()
    }

    /// Binds the fake and serves `shape` on a thread: the port accepts
    /// connections when this returns.
    #[allow(
        clippy::expect_used,
        reason = "a fake that cannot bind a loopback port has nothing to run; a test using it fails without it"
    )]
    fn start(elicitation: String, result: Value, shape: Shape, clock: &'static dyn Clock) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the fake binds");
        let url = format!("http://{}/mcp", listener.local_addr().expect("an address"));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&bodies);
        std::thread::spawn(move || {
            serve(listener, recorded, elicitation, result, shape, clock);
        });
        Self { url, bodies }
    }
}

/// Serves one gated elicitation: the handshake, the gated stream carrying
/// the elicitation, the elicitation's answer, then anything else until the
/// goodbye `DELETE`. Every accept, read and write below shares its
/// request's one [`WAIT`] bound and fails naming the request it waited
/// for, so the fake never blocks indefinitely.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
#[allow(
    clippy::expect_used,
    reason = "a fake writing to its client; a failure is the test's"
)]
fn serve(
    listener: TcpListener,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    elicitation: String,
    result: Value,
    shape: Shape,
    clock: &'static dyn Clock,
) {
    listener
        .set_nonblocking(true)
        .expect("the fake polls its listener");
    // The pause between accept polls (`docs/testing.md`, "Waits and
    // timeouts"): a condition wait, never a sleep, bounded by the
    // deadline each accept takes below.
    let pause = Arc::new((Mutex::new(()), std::sync::Condvar::new()));
    let record = |body: Vec<u8>| {
        bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(body);
    };
    // The handshake: initialize as JSON with a session id,
    // notifications `202`, `tools/list` as JSON.
    let (mut stream, _, body, deadline) =
        accept(&listener, &pause, "the initialize request", clock);
    let id = body_id(&body);
    record(body);
    let initialized = answer(
        &id,
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": shape.capabilities(),
            "serverInfo": {"name": "remote", "version": "1"},
        }),
    );
    write_response(
        &mut stream,
        200,
        &[
            ("content-type", "application/json"),
            ("mcp-session-id", SESSION),
        ],
        initialized.as_bytes(),
        deadline,
        "the initialize request",
    );
    let (mut stream, _, body, deadline) =
        accept(&listener, &pause, "the notifications request", clock);
    record(body);
    write_response(
        &mut stream,
        202,
        &[],
        b"",
        deadline,
        "the notifications request",
    );
    let (mut stream, _, body, deadline) =
        accept(&listener, &pause, "the tools/list request", clock);
    let id = body_id(&body);
    record(body);
    let listed = answer(&id, shape.listed());
    write_response(
        &mut stream,
        200,
        &[("content-type", "application/json")],
        listed.as_bytes(),
        deadline,
        "the tools/list request",
    );
    // The gated stream: the elicitation first, held open. The gated
    // answer goes out only after the client's POST of the elicitation's
    // answer arrives below.
    let (mut stream, _, gated_body, gated_deadline) =
        accept(&listener, &pause, shape.gated(), clock);
    let gated_id = body_id(&gated_body);
    record(gated_body);
    write_bounded(
        &mut stream,
        gated_deadline,
        shape.gated(),
        b"HTTP/1.1 200 Fake\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
    );
    write_bounded(
        &mut stream,
        gated_deadline,
        shape.gated(),
        format!("event: message\ndata: {elicitation}\n\n").as_bytes(),
    );
    stream.flush().expect("the elicitation flushes");
    // The next POST is the elicitation's answer: `202` it, then send
    // the gated answer when it accepts, or close unanswered when it
    // declines (without asker forwarding the remover declines, and
    // the ask wait below fails the test first).
    let (mut answered_conn, _, body, answered_deadline) =
        accept(&listener, &pause, "the elicitation's answer", clock);
    let answered: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    record(body);
    write_response(
        &mut answered_conn,
        202,
        &[],
        b"",
        answered_deadline,
        "the elicitation's answer",
    );
    if answered.get("result") == Some(&json!({"action": "accept", "content": {"name": "prod"}})) {
        let done = answer(&gated_id, result);
        write_bounded(
            &mut stream,
            gated_deadline,
            shape.gated(),
            format!("event: message\ndata: {done}\n\n").as_bytes(),
        );
        stream.flush().expect("the answer flushes");
    }
    drop(stream);
    // Drain until goodbye: anything else gets `202`, `DELETE` ends
    // the fake with `200`.
    loop {
        let (mut stream, method, body, deadline) =
            accept(&listener, &pause, "the goodbye DELETE", clock);
        record(body);
        if method == "DELETE" {
            write_response(&mut stream, 200, &[], b"", deadline, "the goodbye DELETE");
            return;
        }
        write_response(&mut stream, 202, &[], b"", deadline, "the goodbye DELETE");
    }
}

/// Accepts one connection and reads its request within one [`WAIT`] bound
/// of the clock: the stream, the method, the body and the bound its
/// answers share. Fails naming `what` on expiry, so the fake never blocks
/// indefinitely on an accept or a read, however slowly the bytes trickle.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
fn accept(
    listener: &TcpListener,
    pause: &Arc<(Mutex<()>, std::sync::Condvar)>,
    what: &str,
    clock: &'static dyn Clock,
) -> (TcpStream, String, Vec<u8>, RequestDeadline) {
    // The request's one deadline (`docs/testing.md`, "Waits and
    // timeouts"): made when the fake accepts the connection, it bounds
    // the accept wait below and every read and write after it, never one
    // timeout per read.
    let deadline = RequestDeadline::start_on(clock);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // The listener polls nonblocking, but each stream reads
                // blocking: an accepted socket inherits the flag on Linux,
                // and the request's deadline bounds each read instead.
                stream.set_nonblocking(false).unwrap_or_else(|error| {
                    panic!("the gated HTTP fake blocks its stream for {what}: {error}")
                });
                let mut stream = stream;
                let (method, body) = read_request(&mut stream, deadline, what);
                return (stream, method, body, deadline);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if deadline.left().is_zero() {
                    panic!("the gated HTTP fake waited {WAIT:?} for {what}");
                }
                let (lock, cvar) = &**pause;
                let guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
                // A poll pause, not a deadline of its own: the wait above
                // bounds the loop (`docs/testing.md`, "Waits and timeouts").
                // Plain data: a panicked holder has already failed its test.
                let _guard = cvar
                    .wait_timeout(guard, POLL)
                    .map(|(guard, _)| guard)
                    .unwrap_or_else(|error| error.into_inner().0);
            }
            Err(error) => panic!("the gated HTTP fake accept for {what} failed: {error}"),
        }
    }
}

/// Reads one HTTP request from `stream` within what remains of
/// `deadline`: the method and the body by `content-length`. One request
/// per connection, as the client sends. Fails naming `what` when the
/// bound expires or the read breaks, so trickled bytes end the wait at
/// the bound instead of stretching it.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
fn read_request(
    stream: &mut TcpStream,
    deadline: RequestDeadline,
    what: &str,
) -> (String, Vec<u8>) {
    let head = read_line(stream, deadline, what, "to read");
    let method = head
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut length = 0usize;
    loop {
        let line = read_line(stream, deadline, what, "to read the head of");
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let mut read = 0;
    while read < length {
        arm_read(stream, deadline, what, "to read the body of");
        let Some(room) = body.get_mut(read..) else {
            panic!("the gated HTTP fake waited {WAIT:?} to read the body of {what}");
        };
        match stream.read(room) {
            Ok(0) => panic!("the gated HTTP fake waited {WAIT:?} to read the body of {what}"),
            Ok(n) => read += n,
            Err(_) => panic!("the gated HTTP fake waited {WAIT:?} to read the body of {what}"),
        }
    }
    (method, body)
}

/// Arms `stream` for one read within what remains of `deadline`: the
/// socket waits at most the time left until the request's bound. Fails
/// naming `what` when none is left, before touching the socket. Every
/// read on the request's stream goes through here, so the bound covers
/// each underlying read, however slowly the bytes trickle.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
fn arm_read(stream: &mut TcpStream, deadline: RequestDeadline, what: &str, part: &str) {
    let left = deadline.left();
    if left.is_zero() {
        panic!("the gated HTTP fake waited {WAIT:?} {part} {what}");
    }
    if stream.set_read_timeout(Some(left)).is_err() {
        panic!("the gated HTTP fake sets a read deadline for {what}");
    }
}

/// Reads one `\n`-terminated line from `stream` within what remains of
/// `deadline`, one read at a time: each underlying read takes only the
/// time left until the request's bound, so a line trickled one byte at a
/// time still ends at the bound. Fails naming `what` on expiry, on a
/// closed connection, or on a broken read.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
fn read_line(stream: &mut TcpStream, deadline: RequestDeadline, what: &str, part: &str) -> String {
    let mut line = Vec::new();
    loop {
        arm_read(stream, deadline, what, part);
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => panic!("the gated HTTP fake waited {WAIT:?} {part} {what}"),
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(_) => panic!("the gated HTTP fake waited {WAIT:?} {part} {what}"),
        }
    }
    match String::from_utf8(line) {
        Ok(line) => line,
        Err(_) => panic!("the gated HTTP fake waited {WAIT:?} {part} {what}"),
    }
}

/// Writes one HTTP response with `headers` and closes the connection,
/// within what remains of `deadline`. Fails naming `what` on expiry, so
/// an answer never blocks past its request's bound.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
#[allow(
    clippy::expect_used,
    reason = "a fake writing to its client; a failure is the test's"
)]
fn write_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
    deadline: RequestDeadline,
    what: &str,
) {
    let mut head = format!("HTTP/1.1 {status} Fake\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    write_bounded(stream, deadline, what, head.as_bytes());
    write_bounded(stream, deadline, what, body);
    stream.flush().expect("the fake flushes");
}

/// Writes `bytes` to `stream` within what remains of `deadline`: the
/// socket waits at most the time left until the request's bound. Fails
/// naming `what` when none is left, before touching the socket. Every
/// write on the request's stream goes through here.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
fn write_bounded(stream: &mut TcpStream, deadline: RequestDeadline, what: &str, bytes: &[u8]) {
    let mut written = 0;
    while written < bytes.len() {
        let left = deadline.left();
        if left.is_zero() {
            panic!("the gated HTTP fake waited {WAIT:?} to write {what}");
        }
        if stream.set_write_timeout(Some(left)).is_err() {
            panic!("the gated HTTP fake sets a write deadline for {what}");
        }
        let Some(chunk) = bytes.get(written..) else {
            panic!("the gated HTTP fake waited {WAIT:?} to write {what}");
        };
        match stream.write(chunk) {
            Ok(0) => panic!("the gated HTTP fake waited {WAIT:?} to write {what}"),
            Ok(n) => written += n,
            Err(_) => panic!("the gated HTTP fake waited {WAIT:?} to write {what}"),
        }
    }
}

/// The `id` of a JSON-RPC body, `Null` when it has none.
fn body_id(body: &[u8]) -> Value {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or(Value::Null)
}

/// One JSON-RPC answer string.
fn answer(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use crate::clock::FakeClock;
    use crate::deadline::Deadline;

    use super::*;

    /// A clock that lives as long as the test process, as a request
    /// deadline's clock must.
    fn fake_clock() -> &'static FakeClock {
        Box::leak(Box::new(FakeClock::new()))
    }

    /// The message a failed request wait carried: the request it names.
    /// The test fails when the wait read instead.
    fn failure_message(
        caught: std::thread::Result<(TcpStream, String, Vec<u8>, RequestDeadline)>,
    ) -> String {
        match caught {
            Ok(_) => panic!("the trickled request read instead of failing"),
            Err(payload) => payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&str>()
                        .map(|line| (*line).to_owned())
                })
                .expect("the failure names its request"),
        }
    }

    /// Trickled bytes stay inside one request's bound (`docs/testing.md`,
    /// "Waits and timeouts"): a client that sends a header line one byte
    /// at a time, slower than the bound overall, gets a failure naming the
    /// request within the bound, instead of stretching each underlying read
    /// past it. Fake time carries the trickle past the bound, so the test
    /// is deterministic, takes milliseconds on the wall clock, and never
    /// sleeps.
    #[test]
    fn trickled_header_bytes_fail_within_the_request_bound() {
        let clock: &'static FakeClock = fake_clock();
        let listener = TcpListener::bind("127.0.0.1:0").expect("the test binds");
        let addr = listener.local_addr().expect("an address");
        listener.set_nonblocking(true).expect("the test polls");
        let pause = Arc::new((Mutex::new(()), std::sync::Condvar::new()));
        let waiting = Arc::clone(&pause);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                accept(&listener, &waiting, "the initialize request", clock)
            }));
            done_tx.send(caught).expect("the test waits");
        });
        let mut client = TcpStream::connect(addr).expect("the test connects");
        // The request line, one byte at a time: every underlying read
        // succeeds, so one timeout per read would never fire on this.
        for byte in b"POST /mcp HTTP/1.1\r\n" {
            client
                .write_all(std::slice::from_ref(byte))
                .expect("the trickle writes");
        }
        // A header line, one byte at a time, across more than the bound
        // overall: its first bytes arrive inside the bound, the rest past
        // it, while every underlying read still succeeds.
        clock.advance(WAIT / 2);
        for byte in b"content-length: 0\r\n".iter().take(10) {
            client
                .write_all(std::slice::from_ref(byte))
                .expect("the trickle writes");
        }
        clock.advance(WAIT / 2 + Duration::from_secs(1));
        client.write_all(b"0").expect("the trickle writes");
        let caught = Deadline::after(WAIT)
            .recv(&done_rx)
            .expect("the bound names the trickled request");
        let message = failure_message(caught);
        assert!(
            message.contains("the initialize request"),
            "the failure names its request, got: {message}"
        );
    }
}
