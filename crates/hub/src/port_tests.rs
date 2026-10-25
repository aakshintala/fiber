//! Tests for the websocket port end to end: the handshake, authentication,
//! origins, the message rules and the stopping close, over a real
//! `127.0.0.1` port. Each test builds its own hub, never advancing its
//! fake clock; every item the client expects has one wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::SessionStatus;
use log::diag::Severity;
use serde_json::{Value, json};

use super::*;
use crate::connection::{Hub, lock};
use crate::fake::{FakeStarter, status};
use crate::listen::Held;
use crate::testkit::await_true;
use fakes::Deadline;
use fakes::clock::FakeClock;

/// The smallest step of fake time.
const TICK: Duration = Duration::from_nanos(1);

/// Thirty idle minutes, the documented default.
const IDLE: Duration = Duration::from_millis(1_800_000);

const MASK: [u8; 4] = [0x51, 0x1E, 0xA2, 0xC3];

#[path = "port_pairing_tests.rs"]
mod pairing;

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::home("hp");
        Self { dir, held }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("logs").join("hub.log")).unwrap_or_default()
    }
}

/// Whether the hub log in `home` holds a line with `code`.
fn log_has(home: &Path, code: &str) -> bool {
    fs::read_to_string(home.join("logs").join("hub.log"))
        .unwrap_or_default()
        .contains(code)
}

/// Serves a hub with a websocket port on a thread: the port number, the
/// hub, its signal flag and its exit code. The fake clock never advances.
fn serve_with_port(
    temp: &Temp,
    origins: Vec<String>,
    clock: Arc<FakeClock>,
) -> (u16, Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    serve_with_port_commit(temp, origins, clock, None)
}

/// [`serve_with_port`] for a hub built from `commit`.
fn serve_with_port_commit(
    temp: &Temp,
    origins: Vec<String>,
    clock: Arc<FakeClock>,
    commit: Option<&str>,
) -> (u16, Arc<Hub>, Arc<AtomicI32>, mpsc::Receiver<i32>) {
    let timed: Arc<dyn Clock> = clock;
    let hub = Arc::new(
        Hub::new(
            &temp.dir,
            "0.0.0",
            Arc::new(FakeStarter::hang(&temp.dir)),
            Arc::clone(&timed),
            crate::diag::open(&temp.dir, timed),
        )
        .with_commit(commit),
    );
    hub.diag
        .line(Severity::Info, None, "hub_started", "The hub started.");
    let held_lock = crate::listen::lock(&temp.dir).unwrap().unwrap();
    let bound = crate::listen::bind(&held_lock, &temp.dir).unwrap().unwrap();
    let port_listener = crate::listen::bind_port(0).unwrap();
    let port = port_listener.local_addr().unwrap().port();
    let held = Held::new(held_lock, bound).with_port(Listener {
        tcp: port_listener,
        origins: Arc::from(origins),
    });
    let got = Arc::new(AtomicI32::new(0));
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-port-run".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let got = Arc::clone(&got);
            move || {
                let exit = crate::idle::run(&hub, &held, IDLE, &got);
                held.stop();
                done_tx.send(exit.code()).unwrap_or(());
            }
        })
        .unwrap();
    (port, hub, got, done_rx)
}

/// What the test client's reader thread parsed, one channel send each.
#[derive(Debug, PartialEq)]
enum Item {
    Response(u16),
    Text(Vec<u8>),
    Close(u16, String),
    Pong(Vec<u8>),
    Eof,
}

/// A websocket client: writes masked frames, reads whole items on a
/// thread. Dropping shuts the stream down both ways, ending the reader.
struct Ws {
    write: Mutex<TcpStream>,
    rx: mpsc::Receiver<Item>,
}

impl Ws {
    /// Handshakes on `port`, with `origin` for a page's connection. Waits
    /// for nothing: the test takes each expected item itself.
    fn connect(port: u16, origin: Option<&str>) -> Self {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut head = format!(
            "GET /hub HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
        );
        if let Some(origin) = origin {
            head.push_str(&format!("Origin: {origin}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        stream.flush().unwrap();
        let read = stream.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("hub-test-ws-read".to_owned())
            .spawn(move || read_items(read, tx))
            .unwrap();
        Ws {
            write: Mutex::new(stream),
            rx,
        }
    }

    fn send_bytes(&self, bytes: &[u8]) {
        lock(&self.write).write_all(bytes).unwrap();
    }

    fn text(&self, message: &str) {
        self.send_bytes(&masked(0x1, message.as_bytes()));
    }

    fn ping(&self, payload: &[u8]) {
        self.send_bytes(&masked(0x9, payload));
    }

    fn binary(&self, payload: &[u8]) {
        self.send_bytes(&masked(0x2, payload));
    }

    fn close(&self, code: u16) {
        self.send_bytes(&masked(0x8, &code.to_be_bytes()));
    }

    /// Takes the next item, failing with `what` past the deadline: one
    /// deadline for the whole wait, so a hub that trickles fails here.
    #[track_caller]
    fn next(&self, what: &str) -> Item {
        self.next_before(Deadline::after(fakes::MUST_SUCCEED_WITHIN), what)
    }

    /// Takes the next item before `deadline`, failing with `what` past
    /// it: consecutive receives in one drain share one deadline, so the
    /// whole wait is bounded once (`docs/testing.md`, "Waits and timeouts").
    #[track_caller]
    fn next_before(&self, deadline: Deadline, what: &str) -> Item {
        deadline
            .recv(&self.rx)
            .unwrap_or_else(|_| panic!("the client reads {what} before its deadline"))
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        lock(&self.write).shutdown(Shutdown::Both).unwrap_or(());
    }
}

/// One masked client frame.
fn masked(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![opcode | 0x80];
    if payload.len() < 126 {
        out.push(0x80 | u8::try_from(payload.len()).unwrap());
    } else if payload.len() <= 65_535 {
        out.push(0x80 | 126);
        out.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&u64::try_from(payload.len()).unwrap().to_be_bytes());
    }
    out.extend_from_slice(&MASK);
    out.extend(
        payload
            .iter()
            .zip(MASK.iter().cycle())
            .map(|(byte, mask)| byte ^ mask),
    );
    out
}

/// Parses the HTTP response, then whole frames, sending one item each.
fn read_items(stream: TcpStream, tx: mpsc::Sender<Item>) {
    let mut read = BufReader::new(stream);
    let mut head = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        if read.read_exact(&mut byte).is_err() {
            tx.send(Item::Eof).unwrap_or(());
            return;
        }
        let [b] = byte;
        head.push(b);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let status = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or("")
        .split(' ')
        .nth(1)
        .unwrap_or("")
        .parse::<u16>()
        .unwrap_or(0);
    tx.send(Item::Response(status)).unwrap_or(());
    while let Some(item) = server_frame(&mut read) {
        if tx.send(item).is_err() {
            return;
        }
    }
    tx.send(Item::Eof).unwrap_or(());
}

/// Reads one unmasked server frame.
fn server_frame(read: &mut BufReader<TcpStream>) -> Option<Item> {
    let mut head = [0u8; 2];
    read.read_exact(&mut head).ok()?;
    let [b0, b1] = head;
    let opcode = b0 & 0x0F;
    let mut len = u64::from(b1 & 0x7F);
    if len == 126 {
        let mut pair = [0u8; 2];
        read.read_exact(&mut pair).ok()?;
        len = u64::from(u16::from_be_bytes(pair));
    } else if len == 127 {
        let mut wide = [0u8; 8];
        read.read_exact(&mut wide).ok()?;
        len = u64::from_be_bytes(wide);
    }
    let len = usize::try_from(len).ok()?;
    let mut payload = vec![0u8; len];
    read.read_exact(&mut payload).ok()?;
    match opcode {
        0x1 => Some(Item::Text(payload)),
        0x8 => {
            let mut bytes = payload.iter();
            let (Some(first), Some(second)) = (bytes.next(), bytes.next()) else {
                return Some(Item::Close(0, String::new()));
            };
            Some(Item::Close(
                u16::from_be_bytes([*first, *second]),
                String::from_utf8_lossy(&bytes.copied().collect::<Vec<u8>>()).into_owned(),
            ))
        }
        0xA => Some(Item::Pong(payload)),
        _ => None,
    }
}

/// The JSON of the next text item, failing when it is anything else.
#[track_caller]
fn text_of(client: &Ws, what: &str) -> Value {
    match client.next(what) {
        Item::Text(bytes) => {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| panic!("{what} is JSON"))
        }
        other @ (Item::Response(_) | Item::Close(_, _) | Item::Pong(_) | Item::Eof) => {
            panic!("{what} is text, got {other:?}")
        }
    }
}

/// Authenticates `client` with `token` as `id`, returning the answer.
#[track_caller]
fn authenticate(client: &Ws, id: &str, token: &str) -> Value {
    client.text(&format!(
        "{{\"id\":\"{id}\",\"command\":\"authenticate\",\"args\":{{\"token\":\"{token}\"}}}}"
    ));
    text_of(client, "the authentication answer")
}

/// Writes `hub/devices/name` holding `token`'s hash.
fn pair_device(home: &Path, name: &str, token: &str) {
    let dir = home.join("hub").join("devices");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(name),
        format!(
            "{{\"token_sha256\":\"{}\"}}",
            crate::devices::sha256_hex(token.as_bytes())
        ),
    )
    .unwrap();
}

/// Connects, handshakes and reads `hub_hello`.
#[track_caller]
fn helloed(port: u16, origin: Option<&str>) -> Ws {
    let client = Ws::connect(port, origin);
    assert_eq!(client.next("the handshake answer"), Item::Response(101));
    let hello = text_of(&client, "hub_hello");
    assert_eq!(hello["kind"], "hub_hello");
    assert_eq!(hello["payload"]["fiber_version"], "0.0.0");
    client
}

/// Signals the hub as a caught SIGTERM does.
fn signal(hub: &Hub, got: &AtomicI32) {
    got.store(signal_hook::consts::SIGTERM, Ordering::SeqCst);
    hub.waker().wake();
}

/// Waits until the hub counts no client.
#[track_caller]
fn await_zero(hub: &Arc<Hub>) {
    crate::testkit::await_true("the count returns to 0", {
        let hub = Arc::clone(hub);
        move || hub.clients() == 0
    });
}

#[test]
fn the_handshake_gives_101_then_one_hub_hello() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, _hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    let client = helloed(port, None);
    // The pong proves the hub reached its first-message read: a duplicate
    // hello would arrive before it, on the same channel.
    client.ping(b"ack");
    assert_eq!(
        client.next("the pong after hub_hello"),
        Item::Pong(b"ack".to_vec())
    );
    assert!(client.rx.try_recv().is_err(), "nothing follows hub_hello");
}

#[test]
fn the_websocket_hub_hello_carries_the_hubs_commit() {
    let temp = Temp::new();
    let sha = "2121f38d1e4f0a9b6c3d2e1f0a9b8c7d6e5f4a3b";
    let (port, _hub, _got, _done) =
        serve_with_port_commit(&temp, Vec::new(), FakeClock::new(), Some(sha));
    let client = Ws::connect(port, None);
    assert_eq!(client.next("the handshake answer"), Item::Response(101));
    let hello = text_of(&client, "hub_hello");
    assert_eq!(
        hello["payload"],
        json!({"fiber_version": "0.0.0", "fiber_commit": sha})
    );
}

#[test]
fn authenticate_answers_accepted_then_status_serves() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(accepted["payload"]["command_id"], "c1");
    assert!(
        accepted["payload"].get("result").is_none(),
        "authenticate carries no result: {accepted}"
    );
    client.text(r#"{"id":"s1","command":"status"}"#);
    let status = text_of(&client, "status");
    assert_eq!(status["payload"]["result"]["clients"], 1);
    crate::testkit::await_true("the hub logs the connection", {
        let home = temp.dir.clone();
        move || log_has(&home, "client_connected")
    });
    assert_eq!(hub.clients(), 1);
}

#[test]
fn pumped_lines_drop_the_pair_terminator_from_websocket_text() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, _hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");

    client.text(r#"{"id":"s1","command":"status"}"#);
    let Item::Text(bytes) = client.next("the pumped status") else {
        panic!("the pumped status is text");
    };
    assert!(
        !bytes.ends_with(b"\n"),
        "the pair line terminator is not in the websocket message"
    );
    let status: Value = serde_json::from_slice(&bytes).expect("the pumped status is JSON");
    assert_eq!(status["payload"]["command_id"], "s1");
}

#[test]
fn an_unknown_token_is_unauthenticated_and_closed() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    client.text(r#"{"id":"c1","command":"authenticate","args":{"token":"wrong"}}"#);
    let rejected = text_of(&client, "the refusal");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "unauthenticated");
    assert_eq!(rejected["payload"]["command_id"], "c1");
    assert_eq!(
        rejected["payload"]["message"],
        "The first command on the hub's port is `authenticate` with a device token the hub holds, or `pair`."
    );
    assert_eq!(
        client.next("the close"),
        Item::Close(1008, "unauthenticated".to_owned())
    );
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
    let log = temp.log();
    assert!(log.contains("client_unauthenticated"), "{log}");
    assert!(!log.contains("wrong"), "no token in the log: {log}");
    assert!(!log.contains("client_connected"), "{log}");
    // A new client still authenticates after the refusal.
    let second = helloed(port, None);
    let accepted = authenticate(&second, "c2", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
}

#[test]
fn another_first_command_is_unauthenticated() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    let client = helloed(port, None);
    client.text(r#"{"id":"c1","command":"status"}"#);
    let rejected = text_of(&client, "the refusal");
    assert_eq!(rejected["payload"]["code"], "unauthenticated");
    assert_eq!(rejected["payload"]["command_id"], "c1");
    assert_eq!(
        client.next("the close"),
        Item::Close(1008, "unauthenticated".to_owned())
    );
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
}

#[test]
fn a_non_json_first_message_is_unauthenticated_without_an_id() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    let client = helloed(port, None);
    client.text("not json");
    let rejected = text_of(&client, "the refusal");
    assert_eq!(rejected["payload"]["code"], "unauthenticated");
    assert!(
        rejected["payload"].get("command_id").is_none(),
        "no id is readable: {rejected}"
    );
    assert_eq!(
        client.next("the close"),
        Item::Close(1008, "unauthenticated".to_owned())
    );
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
}

#[test]
fn shaped_wrong_authentications_are_unauthenticated() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let extra = helloed(port, None);
    extra.text(r#"{"id":"c1","command":"authenticate","args":{"token":"t0k3n","extra":1}}"#);
    let rejected = text_of(&extra, "the refusal");
    assert_eq!(rejected["payload"]["code"], "unauthenticated");
    assert_eq!(
        extra.next("the close"),
        Item::Close(1008, "unauthenticated".to_owned())
    );
    let scoped = helloed(port, None);
    scoped.text(
        r#"{"id":"c2","command":"authenticate","session_id":"s_0000000000000001","args":{"token":"t0k3n"}}"#,
    );
    let rejected = text_of(&scoped, "the refusal");
    assert_eq!(rejected["payload"]["code"], "unauthenticated");
    assert_eq!(
        scoped.next("the close"),
        Item::Close(1008, "unauthenticated".to_owned())
    );
    await_zero(&hub);
}

#[test]
fn a_first_pair_is_pairing_failed() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    let client = helloed(port, None);
    client.text(r#"{"id":"p1","command":"pair","args":{"code":"123456"}}"#);
    let rejected = text_of(&client, "the refusal");
    assert_eq!(rejected["payload"]["code"], "pairing_failed");
    assert_eq!(rejected["payload"]["command_id"], "p1");
    assert_eq!(
        rejected["payload"]["message"],
        "The pairing code is wrong, used or more than 10 minutes old."
    );
    assert_eq!(
        client.next("the close"),
        Item::Close(1008, "pairing_failed".to_owned())
    );
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
    assert!(temp.log().contains("client_unauthenticated"));
}

#[test]
fn an_unlisted_origin_gets_403_and_a_listed_one_101() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let origins = vec!["https://a.example".to_owned()];
    let (port, hub, _got, _done) = serve_with_port(&temp, origins, clock);
    let refused = Ws::connect(port, Some("https://b.example"));
    assert_eq!(refused.next("the refusal answer"), Item::Response(403));
    assert_eq!(refused.next("the end"), Item::Eof);
    assert_eq!(hub.clients(), 0, "a refused origin is never counted");
    let log = temp.log();
    assert!(log.contains("hub_started"), "{log}");
    for code in [
        "client_connected",
        "client_unauthenticated",
        "client_disconnected",
    ] {
        assert!(
            !log.contains(code),
            "a refused origin writes no log line: {log}"
        );
    }
    let admitted = Ws::connect(port, Some("https://a.example"));
    assert_eq!(admitted.next("the handshake answer"), Item::Response(101));
}

#[test]
fn messages_holding_newlines_parse_before_the_pair() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, _hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "auth", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    // A raw newline between tokens is JSON whitespace: accepted.
    client.text("{\"id\":\"a\",\n\"command\":\"status\"}");
    let first = text_of(&client, "the first status");
    assert_eq!(first["payload"]["command_id"], "a");
    // A raw newline inside a string is invalid JSON: malformed, no id.
    client.text("{\"id\":\"b\",\"command\":\"sta\ntus\"}");
    let second = text_of(&client, "the malformed answer");
    assert_eq!(second["kind"], "command_rejected");
    assert_eq!(second["payload"]["code"], "malformed");
    assert!(
        second["payload"].get("command_id").is_none(),
        "malformed carries no id: {second}"
    );
    // The connection still serves after the malformed line.
    client.text(r#"{"id":"c","command":"status"}"#);
    let third = text_of(&client, "the second status");
    assert_eq!(third["payload"]["command_id"], "c");
}

#[test]
fn pings_pong_before_and_after_authentication() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, _hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    client.ping(b"p1");
    assert_eq!(client.next("the first pong"), Item::Pong(b"p1".to_vec()));
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    client.ping(b"p2");
    assert_eq!(client.next("the second pong"), Item::Pong(b"p2".to_vec()));
}

#[test]
fn a_client_close_echoes_then_disconnects() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    client.close(1000);
    assert_eq!(client.next("the echo"), Item::Close(1000, String::new()));
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
    assert!(temp.log().contains("client_disconnected"));
}

#[test]
fn a_binary_message_after_authentication_closes_1003() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    client.binary(b"x");
    assert_eq!(client.next("the close"), Item::Close(1003, String::new()));
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
}

/// A `stop` over a websocket answers and ends: the answer goes out
/// behind the drain barrier, and the ack, the close frame and EOF carry
/// no promised relative order, so the test matches them by shape.
#[test]
fn a_websocket_stop_answers_before_the_close_then_ends() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, done) = serve_with_port(&temp, Vec::new(), Arc::clone(&clock));
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    client.text(r#"{"id":"c_stop","command":"stop","args":{}}"#);
    // One deadline bounds the whole drain: each receive takes what remains
    // of it, so a hub that trickles one item per bound fails here
    // (`docs/testing.md`, "Waits and timeouts").
    let drain = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let mut ack: Option<Value> = None;
    let mut closed = false;
    loop {
        match client.next_before(drain, "the stop acknowledgement, the close or the end") {
            Item::Text(bytes) => {
                // Every driver command is answered exactly once
                // (`docs/events.md`, "Command acknowledgements").
                assert!(ack.is_none(), "the stop answers once");
                ack = Some(
                    serde_json::from_slice(&bytes)
                        .unwrap_or_else(|_| panic!("the stop acknowledgement is JSON")),
                );
            }
            Item::Close(code, reason) => {
                // `stop` ends the hub as SIGTERM does (`docs/invocation.md`,
                // "Driver commands"), and SIGTERM sends each websocket one
                // close message (`docs/invocation.md`, "The terminal and the
                // hub").
                assert!(!closed, "the connection closes once");
                assert_eq!(code, 1000);
                assert!(reason.is_empty(), "the close carries no reason");
                closed = true;
            }
            // Read until the stream ends; no EOF count is promised.
            Item::Eof => break,
            other @ (Item::Response(_) | Item::Pong(_)) => {
                panic!("the stop sends no greeting or pong, got {other:?}")
            }
        }
    }
    let ack = ack.expect("the stop is acknowledged");
    assert_eq!(ack["kind"], "command_accepted");
    assert_eq!(ack["payload"]["command_id"], "c_stop");
    assert!(
        ack["payload"].get("result").is_none(),
        "a bare stop carries no result: {ack}"
    );
    assert!(closed, "the connection closes");
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    await_zero(&hub);
}

#[test]
fn a_websocket_sessions_stop_answers_the_count_before_the_close() {
    // One live session through a scripted socket: answers `subscribe`
    // and `close`, then closes, so the loop confirms it.
    const SID: &str = "s_0123456789abcdef";
    let temp = Temp::new();
    let clock = FakeClock::new();
    let run = temp.dir.join("run");
    fs::create_dir_all(&run).unwrap();
    let listener = UnixListener::bind(run.join(SID)).unwrap();
    let (port, hub, _got, done) = serve_with_port(&temp, Vec::new(), Arc::clone(&clock));
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    thread::Builder::new()
        .name("hub-test-scripted-session".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(fakes::MUST_SUCCEED_WITHIN))
                .unwrap();
            let mut read = BufReader::new(stream.try_clone().unwrap());
            let mut write = stream;
            for _ in 0..2 {
                let mut line = String::new();
                read.read_line(&mut line).unwrap();
                let command: Value = serde_json::from_str(&line).unwrap();
                let id = command["id"].as_str().unwrap_or("").to_owned();
                let mut bytes = serde_json::to_vec(&json!({
                    "kind": "command_accepted",
                    "ts": 0,
                    "schema_version": 1,
                    "payload": {"command_id": id},
                }))
                .unwrap();
                bytes.push(b'\n');
                write.write_all(&bytes).unwrap();
                write.flush().unwrap();
            }
            write.shutdown(Shutdown::Both).unwrap_or(());
        })
        .unwrap();
    client.text(r#"{"id":"c_stop","command":"stop","args":{"sessions":true}}"#);
    let ack = text_of(&client, "the stop acknowledgement");
    assert_eq!(ack["kind"], "command_accepted");
    assert_eq!(ack["payload"]["command_id"], "c_stop");
    // docs/events.md:1389 defines `sessions_stopped` as the sessions that confirmed closed.
    assert_eq!(
        ack["payload"]["result"],
        json!({"sessions_stopped": 1}),
        "the ack counts the closed session"
    );
    assert_eq!(client.next("the close"), Item::Close(1000, String::new()));
    assert_eq!(client.next("the end"), Item::Eof);
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    await_zero(&hub);
}

/// A `stop` with `sessions` over a websocket interrupted mid-scan tells
/// the asker what is still running: one live session through a scripted
/// socket answers both lines but never closes, so the signal lands while
/// the loop waits for its EOF. The client reads the partial rejection
/// frame, then the close frame, then EOF, in that order.
#[test]
fn a_websocket_stop_interrupted_mid_scan_names_what_is_still_running() {
    const SID: &str = "s_0123456789abcdef";
    let temp = Temp::new();
    let clock = FakeClock::new();
    let run = temp.dir.join("run");
    fs::create_dir_all(&run).unwrap();
    let listener = UnixListener::bind(run.join(SID)).unwrap();
    let (port, hub, got, done) = serve_with_port(&temp, Vec::new(), Arc::clone(&clock));
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    // One live session: answers `subscribe` and `close`, then holds the
    // socket open without EOF, so the loop parks awaiting its close.
    // The feed scanner connects first at startup; the loop connects
    // after the stop. Every connection answers what it is asked; the
    // one asked `close` is the loop's.
    let (answered_tx, answered_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-scripted-session".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                stream
                    .set_read_timeout(Some(fakes::MUST_SUCCEED_WITHIN))
                    .unwrap();
                let answered_tx = answered_tx.clone();
                thread::Builder::new()
                    .name("hub-test-scripted-conn".to_owned())
                    .spawn(move || {
                        let mut read = BufReader::new(stream.try_clone().unwrap());
                        let mut write = stream;
                        loop {
                            let mut line = String::new();
                            match read.read_line(&mut line) {
                                Ok(0) | Err(_) => return,
                                Ok(_) => {}
                            }
                            let command: Value = match serde_json::from_str(&line) {
                                Ok(command) => command,
                                Err(_) => continue,
                            };
                            let name = command["command"].as_str().unwrap_or("").to_owned();
                            let id = command["id"].as_str().unwrap_or("").to_owned();
                            // One `write_all`, as every session writes: a
                            // split line loses its head on the loop's
                            // non-blocking read.
                            let mut bytes = serde_json::to_vec(&json!({
                                "kind": "command_accepted",
                                "ts": 0,
                                "schema_version": 1,
                                "payload": {"command_id": id},
                            }))
                            .unwrap();
                            bytes.push(b'\n');
                            write.write_all(&bytes).unwrap();
                            write.flush().unwrap();
                            if name == "close" {
                                answered_tx.send(()).unwrap_or(());
                                // Held open: the read ends when the hub
                                // exits and closes.
                                let mut tail = String::new();
                                read.read_line(&mut tail).unwrap_or(0);
                                return;
                            }
                        }
                    })
                    .unwrap();
            }
        })
        .unwrap();
    client.text(r#"{"id":"c_stop","command":"stop","args":{"sessions":true}}"#);
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&answered_rx)
        .expect("the session answers both lines");
    signal(&hub, &got);
    let rejected = text_of(&client, "the partial rejection");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "io_failed");
    assert_eq!(rejected["payload"]["command_id"], "c_stop");
    assert_eq!(
        rejected["payload"]["message"],
        "Session s_0123456789abcdef is still running."
    );
    assert_eq!(client.next("the close"), Item::Close(1000, String::new()));
    assert_eq!(client.next("the end"), Item::Eof);
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    await_zero(&hub);
}

#[test]
fn a_signal_closes_every_websocket_saying_the_hub_is_stopping() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, got, done) = serve_with_port(&temp, Vec::new(), Arc::clone(&clock));
    pair_device(&temp.dir, "phone", "t0k3n");
    let first = helloed(port, None);
    let accepted = authenticate(&first, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    let second = helloed(port, None);
    signal(&hub, &got);
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    for (client, what) in [(&first, "the first"), (&second, "the second")] {
        assert_eq!(
            client.next(&format!("{what} close")),
            Item::Close(1001, "The hub is stopping.".to_owned())
        );
        assert_eq!(client.next(&format!("{what} end")), Item::Eof);
    }
}

#[test]
fn failed_stop_spawns_still_shut_every_websocket_without_waiting() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), Arc::clone(&clock));
    let first = helloed(port, None);
    let second = helloed(port, None);
    let webs = hub.webs();
    assert_eq!(webs.len(), 2);
    *lock(&hub.fail_spawn) = Some("hub-ws-stop");

    let start = clock.now();
    let (stopped_tx, stopped_rx) = mpsc::channel();
    let stopping_hub = Arc::clone(&hub);
    thread::Builder::new()
        .name("hub-test-stop-all".to_owned())
        .spawn(move || {
            stop_all(&stopping_hub, webs);
            stopped_tx.send(()).unwrap_or(());
        })
        .unwrap();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&stopped_rx)
        .expect("stop_all returns after every stop spawn fails");

    assert_eq!(
        clock.now(),
        start,
        "stop_all returns without advancing time"
    );
    for (client, what) in [(&first, "the first"), (&second, "the second")] {
        assert_eq!(client.next(&format!("{what} end")), Item::Eof);
    }
    await_zero(&hub);
}

#[test]
fn a_stalled_sink_bounds_the_stop_but_still_shuts_down() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, got, done) = serve_with_port(&temp, Vec::new(), Arc::clone(&clock));
    pair_device(&temp.dir, "phone", "t0k3n");
    let first = helloed(port, None);
    let accepted = authenticate(&first, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    let second = helloed(port, None);
    let accepted = authenticate(&second, "c2", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    // Holds the first sink, standing in for a writer blocked on a client
    // that does not read.
    let webs = hub.webs();
    assert_eq!(webs.len(), 2);
    let guard = webs.first().unwrap().hold();
    let start = clock.now();
    signal(&hub, &got);
    // The second client still gets its close while the first is held.
    assert_eq!(
        second.next("the second close"),
        Item::Close(1001, "The hub is stopping.".to_owned())
    );
    assert!(
        clock.await_parked(
            start + STOP_CLOSE_WAIT,
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the stop waits out its bound"
    );
    let mark = clock.advance_when_parked(
        Some(start + STOP_CLOSE_WAIT),
        None,
        STOP_CLOSE_WAIT.checked_sub(TICK).unwrap(),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert!(
        clock.await_parked_since(
            &mark,
            Some(start + STOP_CLOSE_WAIT),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the stop reparks at its bound after the advance"
    );
    assert!(
        done.try_recv().is_err(),
        "the hub has not stopped before its bound"
    );
    clock.advance_when_parked(
        Some(start + STOP_CLOSE_WAIT),
        None,
        TICK,
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
    // The held client gets EOF with no close frame: no byte could reach it.
    assert_eq!(first.next("the first end"), Item::Eof);
    drop(guard);
}

#[test]
fn a_close_waiting_on_the_sink_goes_out_once_it_frees() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, got, done) = serve_with_port(&temp, Vec::new(), Arc::clone(&clock));
    pair_device(&temp.dir, "phone", "t0k3n");
    let first = helloed(port, None);
    let accepted = authenticate(&first, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    let second = helloed(port, None);
    let accepted = authenticate(&second, "c2", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    let webs = hub.webs();
    assert_eq!(webs.len(), 2);
    let guard = webs.first().unwrap().hold();
    signal(&hub, &got);
    drop(guard);
    // The close waited for the sink rather than skipping it.
    assert_eq!(
        first.next("the first close"),
        Item::Close(1001, "The hub is stopping.".to_owned())
    );
    assert_eq!(first.next("the first end"), Item::Eof);
    assert_eq!(
        second.next("the second close"),
        Item::Close(1001, "The hub is stopping.".to_owned())
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&done)
            .expect("the hub stops"),
        143
    );
}

#[test]
fn a_handshake_with_stop_set_gets_101_then_eof() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = clock;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(&timed),
        crate::diag::open(&temp.dir, timed),
    ));
    let listener = crate::listen::bind_port(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(true));
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-once".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            move || {
                let (stream, _) = listener.accept().unwrap();
                serve(stream, hub, stop, Arc::from([]));
                tx.send(()).unwrap_or(());
            }
        })
        .unwrap();
    let client = Ws::connect(port, None);
    assert_eq!(client.next("the handshake answer"), Item::Response(101));
    assert_eq!(client.next("the end"), Item::Eof);
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("serve returns");
}

#[test]
fn an_oversized_first_message_closes_1009() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    let client = helloed(port, None);
    client.send_bytes(&masked(0x1, &vec![b'a'; frame::FIRST_MESSAGE_MAX + 1]));
    assert_eq!(client.next("the close"), Item::Close(1009, String::new()));
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
}

#[test]
fn a_large_message_after_authentication_has_no_cap() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, _hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    // A megabyte past the first-message cap still arrives: the cap ends
    // at authentication.
    let big = format!("\"{}\"", "a".repeat(1_048_576));
    client.send_bytes(&masked(0x1, big.as_bytes()));
    let answered = text_of(&client, "the malformed answer");
    assert_eq!(answered["kind"], "command_rejected");
    assert_eq!(answered["payload"]["code"], "malformed");
}

#[test]
fn two_clients_each_get_only_their_own_answers() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, _hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "one", "t1");
    pair_device(&temp.dir, "two", "t2");
    let one = helloed(port, None);
    let accepted = authenticate(&one, "a1", "t1");
    assert_eq!(accepted["kind"], "command_accepted");
    let two = helloed(port, None);
    let accepted = authenticate(&two, "b1", "t2");
    assert_eq!(accepted["kind"], "command_accepted");
    one.text(r#"{"id":"aa","command":"status"}"#);
    two.text(r#"{"id":"bb","command":"status"}"#);
    let first = text_of(&one, "the first status");
    assert_eq!(first["payload"]["command_id"], "aa");
    let second = text_of(&two, "the second status");
    assert_eq!(second["payload"]["command_id"], "bb");
}

#[test]
fn a_serve_spawn_failure_closes_1011_without_counting() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    *lock(&hub.fail_spawn) = Some("hub-ws-serve");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(client.next("the close"), Item::Close(1011, String::new()));
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
    assert_eq!(hub.pairing.live(), 0);
    let log = temp.log();
    assert!(!log.contains("client_connected"), "{log}");
    assert!(!log.contains("client_disconnected"), "{log}");
}

#[test]
fn an_out_pump_spawn_failure_disconnects_through_the_pair() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, _got, _done) = serve_with_port(&temp, Vec::new(), clock);
    pair_device(&temp.dir, "phone", "t0k3n");
    *lock(&hub.fail_spawn) = Some("hub-ws-out");
    let client = helloed(port, None);
    let accepted = authenticate(&client, "c1", "t0k3n");
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(client.next("the close"), Item::Close(1011, String::new()));
    assert_eq!(client.next("the end"), Item::Eof);
    await_zero(&hub);
    assert_eq!(hub.pairing.live(), 0);
    assert!(temp.log().contains("client_disconnected"));
}

/// A `waiting` session status whose summary is `summary`.
fn waiting_with(summary: &str) -> SessionStatus {
    let mut payload = status("n", "/w", "idle", None);
    let map = payload.as_object_mut().unwrap();
    map.insert("state".to_owned(), json!("waiting"));
    map.insert(
        "waiting".to_owned(),
        json!({
            "request_id": "r1",
            "kind": "approval",
            "summary": summary,
        }),
    );
    serde_json::from_value(payload).unwrap()
}

#[test]
fn overflow_on_a_websocket_bridge_shuts_the_unix_pair_and_the_tcp_stream() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = clock;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(&timed),
        crate::diag::open(&temp.dir, timed),
    ));
    // The TCP side of the bridge: the peer never reads.
    let tcp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_addr = tcp_listener.local_addr().unwrap();
    let tcp_client = TcpStream::connect(tcp_addr).unwrap();
    tcp_client
        .set_read_timeout(Some(fakes::MUST_SUCCEED_WITHIN))
        .unwrap();
    let mut tcp_client = tcp_client;
    let (tcp_server, _) = tcp_listener.accept().unwrap();
    let web = Arc::new(Web {
        sink: Mutex::new(Sink {
            stream: tcp_server.try_clone().unwrap(),
            closed: false,
        }),
        control: tcp_server,
        drained: AtomicBool::new(false),
    });
    // The Unix side of the bridge, served as an authenticated connection.
    let (hub_end, bridge_end) = UnixStream::pair().unwrap();
    let sub_sock = hub_end.try_clone().unwrap();
    let closer_sock = hub_end.try_clone().unwrap();
    let retired_sock = hub_end.try_clone().unwrap();
    let n = hub
        .register(&hub_end)
        .expect("the hub counts the connection");
    let serve_hub = Arc::clone(&hub);
    let serve_web = Arc::clone(&web);
    let (serve_tx, serve_rx) = mpsc::channel();
    thread::spawn(move || {
        crate::connection::serve_authenticated(
            hub_end,
            serve_hub,
            n,
            crate::pairing::Who::Local,
            Some(serve_web),
        );
        serve_tx.send(()).unwrap_or(());
    });
    let pump_web = Arc::clone(&web);
    let (pump_tx, pump_rx) = mpsc::channel();
    thread::spawn(move || {
        pump_lines(&pump_web, bridge_end);
        pump_tx.send(()).unwrap_or(());
    });
    await_true("the connection to listen", {
        let hub = Arc::clone(&hub);
        move || hub.feed.attention.listeners() == 1
    });
    // Saturate the TCP stream first: 16 KiB lines to the connection's own
    // listener until the bound removes it. The peer never reads, so TCP
    // must fill; the pump then stops draining the bridge, the bridge
    // fills, and the connection's writer blocks with a full queue. Small
    // lines alone could otherwise drain through the pump into TCP buffers
    // despite an unread client, and TCP buffers grow past any single line
    // here. Each fill line is dropped unshut: only the unregister matters.
    let fill = Arc::new(AtomicU64::new(0));
    let filling = Arc::clone(&hub);
    await_true("the bridge backpressure to fill", move || {
        if filling.feed.attention.listeners() == 0 {
            return true;
        }
        let n = fill.fetch_add(1, Ordering::SeqCst);
        if n > 20_000 {
            return true;
        }
        let session = format!("fill_{n:08}");
        let status = waiting_with(&"f".repeat(16 * 1024));
        drop(
            filling
                .feed
                .attention
                .notify(&session, None, &status, false),
        );
        false
    });
    assert_eq!(
        hub.feed.attention.listeners(),
        0,
        "the fill phase removed the connection listener at the bound"
    );
    // A test-owned subscriber on a clone of the bridged socket, carrying
    // the websocket's shutdown like the connection's own registrations.
    let owned = Arc::new(Mutex::new(sub_sock));
    let closer = crate::feed::Closer {
        stream: closer_sock,
        web: Some(Arc::clone(&web)),
    };
    let retired = crate::feed::Closer {
        stream: retired_sock,
        web: Some(Arc::clone(&web)),
    };
    hub.feed
        .attention
        .listen(Arc::clone(&owned), closer, retired)
        .unwrap();
    // Force the drained order from the outside peer on every run: read
    // until the first small line's bytes arrive, which proves the pump
    // drained the bridge and the line went through, then stop reading.
    // The bound wait below refills from here whatever the drain left.
    let first = waiting_with("run r0");
    drop(
        hub.feed
            .attention
            .notify("s_0000000000000000", None, &first, false),
    );
    let drained = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let needle: &[u8] = b"run r0";
    let mut overlap = Vec::new();
    loop {
        if drained.left().is_zero() {
            panic!("the TCP peer to drain the first small line");
        }
        let mut chunk = [0u8; 4096];
        let n = match tcp_client.read(&mut chunk) {
            Ok(0) => panic!("the TCP peer to drain the first small line: the stream closed"),
            Ok(n) => n,
            Err(error) => panic!("the TCP peer to drain the first small line: {error}"),
        };
        // The frame may split the needle across reads, so each search
        // covers the new chunk plus the previous needle-length overlap.
        overlap.extend_from_slice(&chunk[..n]);
        if overlap.windows(needle.len()).any(|w| w == needle) {
            break;
        }
        let keep = overlap.len().min(needle.len() - 1);
        overlap.drain(..overlap.len() - keep);
    }
    // Notify one small line per wait poll until the queue bound fires
    // and hands back the shutdowns: the peer stops reading after the
    // drain above, so the bridge refills and the bound fires whatever
    // the drain left, and a bridge that never fills fails this wait by
    // name. The cap fails by name through the assert below.
    // The drain's first line is already out: resume numbering after it.
    let bound_lines = Arc::new(AtomicU64::new(1));
    let bound_hub = Arc::clone(&hub);
    let bound_closers: Arc<Mutex<Vec<crate::feed::Closer>>> = Arc::new(Mutex::new(Vec::new()));
    let bound_out = Arc::clone(&bound_closers);
    await_true("the bound to fire", move || {
        let mut out = lock(&bound_out);
        if !out.is_empty() {
            return true;
        }
        let n = bound_lines.fetch_add(1, Ordering::SeqCst);
        if n > 20_000 {
            return true;
        }
        let session = format!("s_{n:016x}");
        let status = waiting_with(&format!("run r{n}"));
        out.extend(
            bound_hub
                .feed
                .attention
                .notify(&session, None, &status, false),
        );
        !out.is_empty()
    });
    let closers = {
        let mut out = lock(&bound_closers);
        std::mem::take(&mut *out)
    };
    assert!(!closers.is_empty(), "the bound fired");
    // The block wait runs on a thread the test joins: joining it ends the
    // last other clone before the count assert, so the assert reads the
    // joined state at once. Feed's stop already joined every writer.
    let (block_tx, block_rx) = mpsc::channel();
    let blocked = Arc::clone(&owned);
    let block_wait = thread::spawn(move || {
        while blocked.try_lock().is_ok() {
            thread::yield_now();
        }
        block_tx.send(()).unwrap_or(());
    });
    Deadline::after(crate::testkit::DEADLINE)
        .recv(&block_rx)
        .expect("the Unix writer to block in its write");
    block_wait.join().expect("the block wait thread to join");
    crate::feed::close_connections(closers, UnixStream::shutdown);
    assert_eq!(
        hub.feed.attention.listeners(),
        0,
        "the overflow unregistered the listener"
    );
    // The pump ends, the TCP side closes, and the served read loop
    // returns: a closer that shut only the Unix halves would leave the
    // pump and the TCP stream hanging.
    // One deadline for the pump's end and the TCP drain together: the
    // per-read timeout is only a poll pause, so a stalled drain fails
    // this wait by name instead of hanging the loop.
    let shut = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    shut.recv(&pump_rx).expect("the pump to end");
    let mut buf = [0; 8];
    loop {
        let left = shut.left();
        if left.is_zero() {
            panic!("the TCP side closes");
        }
        tcp_client.set_read_timeout(Some(left)).unwrap();
        match tcp_client.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => panic!("the TCP side closes: {error}"),
        }
    }
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&serve_rx)
        .expect("the read loop to return");
    assert_eq!(hub.clients(), 0, "the count released");
    crate::testkit::stop_within(&hub.feed);
    assert_eq!(Arc::strong_count(&owned), 1, "the writer ended and joined");
}
