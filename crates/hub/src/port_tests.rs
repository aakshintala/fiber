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
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use log::diag::Severity;
use serde_json::Value;

use super::*;
use crate::connection::{Hub, lock};
use crate::fake::FakeStarter;
use crate::listen::Held;
use fakes::Deadline;
use fakes::clock::FakeClock;

/// One named deadline per expected item: the hub answers before it.
const DEADLINE: Duration = Duration::from_secs(5);

/// How long a hub-state wait lasts, in wall time.
const WITHIN: Duration = Duration::from_secs(5);

/// The smallest step of fake time.
const TICK: Duration = Duration::from_nanos(1);

/// Thirty idle minutes, the documented default.
const IDLE: Duration = Duration::from_millis(1_800_000);

const MASK: [u8; 4] = [0x51, 0x1E, 0xA2, 0xC3];

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
    let timed: Arc<dyn Clock> = clock;
    let hub = Arc::new(Hub::new(
        &temp.dir,
        "0.0.0",
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(&timed),
        crate::diag::open(&temp.dir, timed),
    ));
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
        Deadline::after(DEADLINE)
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
        Deadline::after(DEADLINE)
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
        clock.await_parked(start + STOP_CLOSE_WAIT, WITHIN),
        "the stop waits out its bound"
    );
    let mark = clock.advance_marked(STOP_CLOSE_WAIT.checked_sub(TICK).unwrap());
    assert!(
        clock.await_parked_since(&mark, Some(start + STOP_CLOSE_WAIT), WITHIN),
        "the stop reparks at its bound after the advance"
    );
    assert!(
        done.try_recv().is_err(),
        "the hub has not stopped before its bound"
    );
    clock.advance(TICK);
    assert_eq!(
        Deadline::after(DEADLINE)
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
        Deadline::after(DEADLINE)
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
    Deadline::after(DEADLINE).recv(&rx).expect("serve returns");
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
    assert!(temp.log().contains("client_disconnected"));
}
