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

use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::deadline::Deadline;

/// How long the fake waits for each request, on the wall clock: longer than
/// [`crate::MUST_SUCCEED_WITHIN`], so a test's own deadline names the
/// failure first on a slow machine, and the fake never blocks indefinitely.
/// On expiry the serving thread fails naming the request it waited for.
const WAIT: Duration = Duration::from_secs(30);

/// The pause between accept polls: a poll pause inside the loop, not a
/// deadline of its own (`docs/testing.md`, "Waits and timeouts").
const POLL: Duration = Duration::from_millis(1);

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
        Self::start(elicitation, answer, Shape::Call)
    }

    /// Starts the fake serving one `resources/read` `elicitation`,
    /// answered with `contents` once the elicitation is answered.
    pub fn start_read(elicitation: String, contents: Value) -> Self {
        Self::start(elicitation, contents, Shape::Read)
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
    fn start(elicitation: String, result: Value, shape: Shape) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the fake binds");
        let url = format!("http://{}/mcp", listener.local_addr().expect("an address"));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&bodies);
        std::thread::spawn(move || serve(listener, recorded, elicitation, result, shape));
        Self { url, bodies }
    }
}

/// Serves one gated elicitation: the handshake, the gated stream carrying
/// the elicitation, the elicitation's answer, then anything else until the
/// goodbye `DELETE`. Every accept and read below is bounded by [`WAIT`] and
/// fails naming the request it waited for, so the fake never blocks
/// indefinitely.
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
    let (mut stream, _, body) = accept(&listener, &pause, "the initialize request");
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
    );
    let (mut stream, _, body) = accept(&listener, &pause, "the notifications request");
    record(body);
    write_response(&mut stream, 202, &[], b"");
    let (mut stream, _, body) = accept(&listener, &pause, "the tools/list request");
    let id = body_id(&body);
    record(body);
    let listed = answer(&id, shape.listed());
    write_response(
        &mut stream,
        200,
        &[("content-type", "application/json")],
        listed.as_bytes(),
    );
    // The gated stream: the elicitation first, held open. The gated
    // answer goes out only after the client's POST of the elicitation's
    // answer arrives below.
    let (mut stream, _, gated_body) = accept(&listener, &pause, shape.gated());
    let gated_id = body_id(&gated_body);
    record(gated_body);
    stream
        .write_all(
            b"HTTP/1.1 200 Fake\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        )
        .expect("the stream head writes");
    stream
        .write_all(format!("event: message\ndata: {elicitation}\n\n").as_bytes())
        .expect("the elicitation writes");
    stream.flush().expect("the elicitation flushes");
    // The next POST is the elicitation's answer: `202` it, then send
    // the gated answer when it accepts, or close unanswered when it
    // declines (without asker forwarding the remover declines, and
    // the ask wait below fails the test first).
    let (mut answered_conn, _, body) = accept(&listener, &pause, "the elicitation's answer");
    let answered: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    record(body);
    write_response(&mut answered_conn, 202, &[], b"");
    if answered.get("result") == Some(&json!({"action": "accept", "content": {"name": "prod"}})) {
        let done = answer(&gated_id, result);
        stream
            .write_all(format!("event: message\ndata: {done}\n\n").as_bytes())
            .expect("the answer writes");
        stream.flush().expect("the answer flushes");
    }
    drop(stream);
    // Drain until goodbye: anything else gets `202`, `DELETE` ends
    // the fake with `200`.
    loop {
        let (mut stream, method, body) = accept(&listener, &pause, "the goodbye DELETE");
        record(body);
        if method == "DELETE" {
            write_response(&mut stream, 200, &[], b"");
            return;
        }
        write_response(&mut stream, 202, &[], b"");
    }
}

/// Accepts one connection and reads its request within [`WAIT`] of real
/// time: the stream, the method and the body. Fails naming `what` on
/// expiry, so the fake never blocks indefinitely on an accept or a read.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
fn accept(
    listener: &TcpListener,
    pause: &Arc<(Mutex<()>, std::sync::Condvar)>,
    what: &str,
) -> (TcpStream, String, Vec<u8>) {
    let deadline = Deadline::after(WAIT);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // The listener polls nonblocking, but each stream reads
                // blocking: an accepted socket inherits the flag on Linux,
                // and the read timeout below bounds the read instead.
                stream.set_nonblocking(false).unwrap_or_else(|error| {
                    panic!("the gated HTTP fake blocks its stream for {what}: {error}")
                });
                let mut stream = stream;
                let (method, body) = read_request(&mut stream, what);
                return (stream, method, body);
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

/// Reads one HTTP request from `stream` within [`WAIT`] of real time: the
/// method and the body by `content-length`. One request per connection, as
/// the client sends. Fails naming `what` when the read expires or breaks.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
fn read_request(stream: &mut TcpStream, what: &str) -> (String, Vec<u8>) {
    if stream.set_read_timeout(Some(WAIT)).is_err() {
        panic!("the gated HTTP fake sets a read deadline for {what}");
    }
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap_or_else(|error| {
        panic!("the gated HTTP fake clones its stream for {what}: {error}")
    }));
    let mut head = String::new();
    if reader.read_line(&mut head).is_err() || head.is_empty() {
        panic!("the gated HTTP fake waited {WAIT:?} to read {what}");
    }
    let method = head
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            panic!("the gated HTTP fake waited {WAIT:?} to read the head of {what}");
        }
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
    if reader.read_exact(&mut body).is_err() {
        panic!("the gated HTTP fake waited {WAIT:?} to read the body of {what}");
    }
    (method, body)
}

/// Writes one HTTP response with `headers` and closes the connection.
#[allow(
    clippy::expect_used,
    reason = "a fake writing to its client; a failure is the test's"
)]
fn write_response(stream: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &[u8]) {
    let mut head = format!("HTTP/1.1 {status} Fake\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    stream.write_all(head.as_bytes()).expect("the fake writes");
    stream.write_all(body).expect("the fake writes");
    stream.flush().expect("the fake flushes");
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
