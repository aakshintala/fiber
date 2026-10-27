//! Tests of the gated HTTP fake, through its own seam (`gated_http.rs`).

use crate::MUST_SUCCEED_WITHIN;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::panic::AssertUnwindSafe;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::clock::FakeClock;
use crate::deadline::Deadline;
use crate::http_server::{HttpServer, Limits};

use super::*;

/// How long the fake bounds one request in the trickle test: the core's
/// own bound, advanced on the injected clock instead of waited out.
const WAIT: Duration = Duration::from_secs(30);

/// The message a failed request wait carried: the request it names.
/// The test fails when the wait read instead.
fn failure_message<T>(caught: std::thread::Result<T>) -> String {
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
    let clock = FakeClock::new();
    let server = HttpServer::bind(clock.clone(), Limits::default()).expect("the core binds");
    let acks = server.test_acknowledgments();
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        // Inside the scope so a failure drops the client before the
        // join: the drop ends the reader's wait instead of hanging it.
        let mut client = TcpStream::connect(server.addr()).expect("the test connects");
        // The request line, one byte at a time, queued before the reader
        // starts: every underlying read succeeds, so one timeout per
        // read would never fire on this. Its first park is the headers
        // wait below, so that phase's acknowledgment is the one the test
        // waits for: an early request-line park would spend the wait's
        // one acknowledgment for this wake generation, and the headers
        // wait would park again without announcing itself.
        for byte in b"POST /mcp HTTP/1.1\r\n" {
            client
                .write_all(std::slice::from_ref(byte))
                .expect("the trickle writes");
        }
        scope.spawn(|| {
            let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let mut conn = server
                    .accept_expected("the initialize request")
                    .expect("a client connects");
                crate::http_server::request::read_request(&mut conn)
            }));
            done_tx.send(caught).expect("the test waits");
        });
        // The headers wait announces its own phase: the server is past
        // its clock check and waiting on the clock before the clock
        // moves (`docs/testing.md`, "Waits and timeouts").
        let headers_deadline = Deadline::after(WAIT);
        let headers = loop {
            let ack = headers_deadline.recv_or_fail(&acks, "the headers wait");
            if ack.phase == "to read the headers" {
                break ack;
            }
        };
        assert!(
            clock.await_thread_parked(headers.thread, headers.until, Deadline::after(WAIT)),
            "the headers wait parks on the clock before it moves"
        );
        // A header line, one byte at a time, across more than the bound
        // overall: its first bytes arrive inside the bound, the rest past
        // it, while every underlying read still succeeds.
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(WAIT / 2);
        for byte in b"content-length: 0\r\n".iter().take(10) {
            client
                .write_all(std::slice::from_ref(byte))
                .expect("the trickle writes");
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
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
    });
}

/// The most bytes a client reads from a stream it does not trust to end,
/// so a mutant that streams forever fails the test instead of growing.
const READ_CAP: u64 = 1 << 16;
/// The most lines a client reads from a response head or a stream.
const LINE_CAP: usize = 256;

/// The `host:port` a fake's `/mcp` URL names.
fn address(url: &str) -> SocketAddr {
    url.strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .and_then(|host| host.parse().ok())
        .expect("the fake's URL names an address")
}

/// Opens one request on a new connection, as the client sends it.
fn connect(url: &str, method: &str, body: &str) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(address(url)).expect("the test connects");
    stream
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN * 2))
        .expect("the test bounds its reads");
    let head = format!(
        "{method} /mcp HTTP/1.1\r\nhost: fake\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .expect("the request writes");
    stream
        .write_all(body.as_bytes())
        .expect("the request writes");
    BufReader::new(stream)
}

/// Reads a response's status line and headers: the status, and the
/// `content-length` when the response has one. The whole head reads on
/// a scoped thread under one deadline (`docs/testing.md`, "Waits and
/// timeouts"), never one timeout per line.
fn read_head(reader: &mut BufReader<TcpStream>) -> (u16, Option<u64>) {
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| if let Ok(()) = tx.send(read_head_inner(reader)) {});
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "the response head")
    })
}

/// The lines of a response head: the status, and the `content-length`
/// when the response has one.
fn read_head_inner(reader: &mut BufReader<TcpStream>) -> (u16, Option<u64>) {
    let mut lines = reader
        .by_ref()
        .lines()
        .map(|line| line.expect("the response head reads"));
    let status_line = lines.next().expect("the response has a status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("the status line has a code");
    let head: Vec<String> = lines
        .take(LINE_CAP)
        .take_while(|line| !line.trim().is_empty())
        .collect();
    let length = head.iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    });
    (status, length)
}

/// One request and its whole response: the status and the body text.
/// The whole exchange runs on a scoped thread under one deadline
/// (`docs/testing.md`, "Waits and timeouts"), never one timeout per
/// line.
fn send(url: &str, method: &str, body: &str) -> (u16, String) {
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut reader = connect(url, method, body);
            let (status, length) = read_head_inner(&mut reader);
            let mut text = Vec::new();
            reader
                .by_ref()
                .take(length.unwrap_or(READ_CAP))
                .read_to_end(&mut text)
                .expect("the response body reads");
            if let Ok(()) = tx.send((status, String::from_utf8_lossy(&text).into_owned())) {}
        });
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "the response")
    })
}

/// One JSON-RPC request and its JSON answer, `Null` when it has none.
fn post(url: &str, body: &Value) -> (u16, Value) {
    let (status, text) = send(url, "POST", &body.to_string());
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The `data:` lines of an event stream, as JSON.
fn events(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str::<Value>(data).expect("an event is JSON"))
        .collect()
}

/// The next `data:` event on an open stream. It reads on a scoped
/// thread under one deadline (`docs/testing.md`, "Waits and timeouts"),
/// never one timeout per line.
fn next_event(reader: &mut BufReader<TcpStream>) -> Value {
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| if let Ok(()) = tx.send(next_event_inner(reader)) {});
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "the stream's next event")
    })
}

/// The next `data:` event on an open stream.
fn next_event_inner(reader: &mut BufReader<TcpStream>) -> Value {
    reader
        .by_ref()
        .lines()
        .take(LINE_CAP)
        .map(|line| line.expect("the stream reads"))
        .find_map(|line| {
            line.strip_prefix("data: ")
                .map(|data| serde_json::from_str::<Value>(data).expect("an event is JSON"))
        })
        .expect("the stream carries an event")
}

/// The rest of an open stream, to its end. It reads on a scoped thread
/// under one deadline (`docs/testing.md`, "Waits and timeouts"), never
/// one timeout per read.
fn rest(reader: BufReader<TcpStream>) -> String {
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| if let Ok(()) = tx.send(rest_inner(reader)) {});
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "the rest of the stream")
    })
}

/// The rest of an open stream, to its end.
fn rest_inner(mut reader: BufReader<TcpStream>) -> String {
    let mut text = Vec::new();
    reader
        .by_ref()
        .take(READ_CAP)
        .read_to_end(&mut text)
        .expect("the stream reads to its end");
    String::from_utf8_lossy(&text).into_owned()
}

fn elicitation() -> String {
    json!({"jsonrpc": "2.0", "id": "el-1", "method": "elicitation/create",
        "params": {"message": "Which?"}})
    .to_string()
}

fn gated_result() -> Value {
    json!({"content": [{"type": "text", "text": "done"}]})
}

fn accept_body() -> Value {
    json!({"jsonrpc": "2.0", "id": "el-1",
        "result": {"action": "accept", "content": {"name": "prod"}}})
}

fn decline_body() -> Value {
    json!({"jsonrpc": "2.0", "id": "el-1", "result": {"action": "decline"}})
}

fn call_body() -> String {
    json!({"jsonrpc": "2.0", "id": "gated-1", "method": "tools/call",
        "params": {"name": "echo"}})
    .to_string()
}

fn read_body() -> String {
    json!({"jsonrpc": "2.0", "id": "gated-1", "method": "resources/read",
        "params": {"uri": "fiber://memory"}})
    .to_string()
}

/// Runs the handshake the fake answers before its gated stream.
fn handshake(url: &str) {
    let (status, reply) = post(
        url,
        &json!({"jsonrpc": "2.0", "id": "init-1", "method": "initialize", "params": {}}),
    );
    assert_eq!(status, 200, "initialize answers 200");
    assert_eq!(reply["id"], json!("init-1"));
    assert_eq!(reply["result"]["capabilities"], json!({"tools": {}}));
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(send(url, "POST", &initialized.to_string()).0, 202);
    let (status, listed) = post(
        url,
        &json!({"jsonrpc": "2.0", "id": "list-1", "method": "tools/list"}),
    );
    assert_eq!(status, 200, "tools/list answers 200");
    assert_eq!(listed["id"], json!("list-1"));
    assert_eq!(listed["result"]["tools"][0]["name"], json!("echo"));
}

/// Opens the gated `tools/call`, reads its head and its elicitation,
/// and returns the stream positioned after the elicitation.
fn open_gated(url: &str) -> (BufReader<TcpStream>, Value) {
    let mut reader = connect(url, "POST", &call_body());
    let (status, _) = read_head(&mut reader);
    assert_eq!(status, 200, "the gated call answers 200");
    let asked = next_event(&mut reader);
    (reader, asked)
}

/// Opens the gated `resources/read`, reads its head and its
/// elicitation, and returns the stream positioned after it.
fn open_gated_read(url: &str) -> (BufReader<TcpStream>, Value) {
    let mut reader = connect(url, "POST", &read_body());
    let (status, _) = read_head(&mut reader);
    assert_eq!(status, 200, "the gated read answers 200");
    let asked = next_event(&mut reader);
    (reader, asked)
}

/// The goodbye: a stray request gets `202`, then `DELETE` ends the fake
/// with `200`.
fn goodbye(url: &str) {
    let stray = json!({"jsonrpc": "2.0", "method": "notifications/cancelled"});
    assert_eq!(
        send(url, "POST", &stray.to_string()).0,
        202,
        "a stray request gets 202"
    );
    assert_eq!(
        send(url, "DELETE", "").0,
        200,
        "the goodbye DELETE gets 200"
    );
}

/// The handshake answers the shape it serves: `initialize` with its
/// capabilities, and `tools/list` with the echo tool.
#[test]
fn the_handshake_answers_with_the_shape_it_serves() {
    let fake = GatedHttp::start_call(elicitation(), gated_result());
    handshake(&fake.url());
}

/// An accepted elicitation is recorded and the gated call answered
/// with the result it was given, then the fake takes its goodbye.
#[test]
fn an_accepted_elicitation_answers_the_gated_call() {
    let fake = GatedHttp::start_call(elicitation(), gated_result());
    let url = fake.url();
    handshake(&url);
    let (gated, asked) = open_gated(&url);
    assert_eq!(asked["id"], json!("el-1"));
    assert_eq!(post(&url, &accept_body()).0, 202);
    let answered = json!({"jsonrpc": "2.0", "id": "gated-1", "result": gated_result()});
    assert_eq!(events(&rest(gated)), vec![answered]);
    assert_eq!(fake.elicitation_answers(), vec![accept_body()]);
    goodbye(&url);
}

/// A declined elicitation is recorded, and the gated call is closed
/// without an answer.
#[test]
fn a_declined_elicitation_leaves_the_gated_call_unanswered() {
    let fake = GatedHttp::start_call(elicitation(), gated_result());
    let url = fake.url();
    handshake(&url);
    let (gated, _) = open_gated(&url);
    assert_eq!(post(&url, &decline_body()).0, 202);
    assert!(
        events(&rest(gated)).is_empty(),
        "a declined elicitation carries no answer"
    );
    assert_eq!(fake.elicitation_answers(), vec![decline_body()]);
    goodbye(&url);
}

/// The read shape runs on the same core: its handshake lists no tools
/// under tools and resources, and its gated read carries the
/// elicitation before the contents it was given.
#[test]
fn read_shape_uses_the_same_core_and_preserves_contents() {
    let contents = json!({"contents": [{"uri": "fiber://memory", "text": "kept"}]});
    let fake = GatedHttp::start_read(elicitation(), contents.clone());
    let url = fake.url();
    let (status, reply) = post(
        &url,
        &json!({"jsonrpc": "2.0", "id": "init-1", "method": "initialize", "params": {}}),
    );
    assert_eq!(status, 200, "initialize answers 200");
    assert_eq!(
        reply["result"]["capabilities"],
        json!({"tools": {}, "resources": {}}),
        "the read shape lists both capabilities"
    );
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(send(&url, "POST", &initialized.to_string()).0, 202);
    let (status, listed) = post(
        &url,
        &json!({"jsonrpc": "2.0", "id": "list-1", "method": "tools/list"}),
    );
    assert_eq!(status, 200, "tools/list answers 200");
    assert_eq!(listed["result"]["tools"], json!([]));
    let (gated, asked) = open_gated_read(&url);
    assert_eq!(asked["id"], json!("el-1"));
    assert_eq!(post(&url, &accept_body()).0, 202);
    let answered = json!({"jsonrpc": "2.0", "id": "gated-1", "result": contents});
    assert_eq!(events(&rest(gated)), vec![answered]);
    assert_eq!(fake.elicitation_answers(), vec![accept_body()]);
    goodbye(&url);
}

/// Dropping the fake before goodbye stops its server: the serving
/// thread is reaped, and the port refuses connections.
#[test]
fn dropping_before_goodbye_stops_the_server() {
    let fake = GatedHttp::start_call(elicitation(), gated_result());
    let url = fake.url();
    handshake(&url);
    let (gated, asked) = open_gated(&url);
    assert_eq!(asked["id"], json!("el-1"));
    drop(gated);
    let probe = fake.core.drop_probe();
    drop(fake);
    assert!(
        probe.listener_closed(),
        "the dropped fake closed its listener"
    );
    assert!(
        probe.accept_joined(),
        "the dropped fake joined its accept thread"
    );
}

/// Dropping the fake refuses connections promptly: the port is already
/// closed when the drop returns, so the first connect after it is
/// refused, without waiting out any bound.
#[test]
fn a_dropped_fake_refuses_connections_at_once() {
    let fake = GatedHttp::start_call(elicitation(), gated_result());
    let url = fake.url();
    handshake(&url);
    let probe = fake.core.drop_probe();
    drop(fake);
    assert!(
        probe.listener_closed(),
        "the dropped fake closed its listener"
    );
    assert!(
        probe.accept_joined(),
        "the dropped fake joined its accept thread"
    );
}

/// Shutdown ends an accept wait at once: the wait returns instead of
/// polling out its bound, so a stopped server never waits out a bound
/// it was told to drop.
#[test]
fn shutdown_ends_an_accept_wait_at_once() {
    let clock = FakeClock::new();
    let server = HttpServer::bind(clock, Limits::default()).expect("the core binds");
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let accepted = server.accept_expected("the initialize request");
            match done_tx.send(accepted.is_some()) {
                Ok(()) | Err(_) => {}
            }
        });
        assert!(server.shutdown(), "shutdown ends the wait");
        assert!(
            !Deadline::after(MUST_SUCCEED_WITHIN * 2).recv_or_fail(&done_rx, "the stopped accept"),
            "shutdown ends the accept stopped"
        );
    });
    // An accept after shutdown ends at once too: it never waits.
    assert!(
        server.accept_expected("the initialize request").is_none(),
        "no accept waits past shutdown"
    );
}
