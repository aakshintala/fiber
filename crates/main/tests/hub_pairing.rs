//! The CLI's device commands and a revoked token's binary-level security boundary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

#[path = "support/hub_start.rs"]
#[allow(dead_code, reason = "this test binary seeds no paired client")]
mod hub_start;
mod support;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use support::*;

fn record(setup: &Setup) {
    write_json(
        &setup.home().join("hub/devices/phone"),
        &json!({"name":"phone","paired_at":1700000000000u64,"last_connected_at":1700000001000u64,"token_sha256":"a".repeat(64)}),
    );
}
fn run(setup: &Setup, args: &[&str]) -> std::process::Output {
    run_to_exit(setup.deadline, "the device command", setup.fiber(args))
}
fn hub_log_path(setup: &Setup) -> std::path::PathBuf {
    setup.home().join("logs/hub.log")
}
fn finish(hub: Arc<Mutex<Option<HubProc>>>, client: Socket) {
    drop(client);
    let hub = hub.lock().unwrap().take().unwrap();
    hub.kill("TERM");
    assert_eq!(hub.wait().code(), Some(143));
}
#[test]
fn hub_pair_prints_one_code_line() {
    let setup = Setup::new();
    let wrong = run(&setup, &["hub", "pair", "phone", "--address", "http://x"]);
    assert_eq!(wrong.status.code(), Some(2));
    assert!(wrong.stdout.is_empty());
    assert!(!setup.hub_socket().exists());
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let output = run(&setup, &["hub", "pair", "phone"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].len(), 10);
    assert!(
        lines[0]
            .bytes()
            .all(|b| b"0123456789abcdefghjkmnpqrstvwxyz".contains(&b))
    );
    finish(hub, client);
}
#[test]
fn hub_pair_on_a_terminal_draws_a_qr_code() {
    let setup = Setup::new();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let mut terminal = support::pty::Run::spawn(
        &setup,
        80,
        32,
        &[
            "hub",
            "pair",
            "phone",
            "--address",
            "wss://mybox.tailnet.ts.net",
        ],
        &[],
    );
    let grid = terminal.wait_screen("the code and the QR quiet zone", |grid| {
        let text = &grid.contents;
        let first = text.lines().next().unwrap_or("").trim();
        first.len() == 10
            && first
                .bytes()
                .all(|b| b"0123456789abcdefghjkmnpqrstvwxyz".contains(&b))
            && text.contains("▀")
            && text.contains("▄")
    });
    assert!(grid.contents.contains("████████"));
    finish(hub, client);
    assert_eq!(terminal.wait().status.code(), Some(0));
}
#[test]
fn hub_token_list_lists_a_paired_device() {
    let setup = Setup::new();
    record(&setup);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let output = run(&setup, &["hub", "token", "list"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "name   paired                last connected\nphone  2023-11-14T22:13:20Z  2023-11-14T22:13:21Z\n"
    );
    finish(hub, client);
}
#[test]
fn hub_token_revoke_deletes_the_record_and_a_client_sees_device_changed() {
    let setup = Setup::new();
    record(&setup);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let output = run(&setup, &["hub", "token", "revoke", "phone"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"phone\n");
    let line = recv(&client, "the revocation announcement");
    assert_eq!(line["kind"], "device_changed");
    assert_eq!(
        line["payload"],
        json!({"change":"revoked","device":"phone","by":"local"})
    );
    assert!(!setup.home().join("hub/devices/phone").exists());
    let log = hub_log_path(&setup);
    let deadline = setup.deadline;
    let seen = bounded(deadline, "the device_revoked line in hub.log", move || {
        loop {
            if fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("device_revoked")
            {
                return true;
            }
            if deadline.left().is_zero() {
                // Stops this worker thread after `bounded` has already reported
                // the timeout naming the line; the returned value never
                // decides the failure text.
                return false;
            }
            std::thread::yield_now();
        }
    });
    assert!(
        seen,
        "waited until the deadline for the device_revoked line in hub.log"
    );
    finish(hub, client);
}
#[test]
fn hub_token_revoke_of_an_unpaired_device_fails() {
    let setup = Setup::new();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let output = run(&setup, &["hub", "token", "revoke", "nope"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, b"fiber: No device named nope is paired.\n");
    assert!(output.stdout.is_empty());
    finish(hub, client);
}

#[derive(Debug)]
enum Frame {
    Text(Value),
    Close(u16),
    Eof,
}
struct Ws {
    stream: TcpStream,
    rx: mpsc::Receiver<Frame>,
}
impl Ws {
    fn connect(setup: &Setup, port: u16) -> Self {
        let deadline = setup.deadline;
        let mut stream = bounded(deadline, "the hub TCP port", move || {
            TcpStream::connect(("127.0.0.1", port))
        })
        .unwrap();
        stream.set_write_timeout(Some(deadline.left())).unwrap();
        stream.set_read_timeout(Some(deadline.left())).unwrap();
        stream.write_all(format!("GET /hub HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").as_bytes()).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        assert_eq!(first, "HTTP/1.1 101 Switching Protocols\r\n");
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            assert!(!line.is_empty(), "handshake closed before its headers");
        }
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Some(frame) = read_frame(&mut reader) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
            tx.send(Frame::Eof).unwrap_or(());
        });
        Self { stream, rx }
    }
    fn send(&mut self, setup: &Setup, value: Value) {
        let bytes = value.to_string().into_bytes();
        let mut frame = vec![0x81];
        if bytes.len() < 126 {
            frame.push(0x80 | u8::try_from(bytes.len()).unwrap());
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_be_bytes());
        }
        let mask = [1, 2, 3, 4];
        frame.extend_from_slice(&mask);
        frame.extend(
            bytes
                .iter()
                .zip(mask.iter().cycle())
                .map(|(byte, mask)| byte ^ mask),
        );
        self.stream
            .set_write_timeout(Some(setup.deadline.left()))
            .expect("set the websocket write timeout to the test's remaining deadline");
        self.stream
            .write_all(&frame)
            .expect("write the masked client frame");
    }
    fn next(&self, setup: &Setup) -> Frame {
        setup
            .deadline
            .recv_or_fail(&self.rx, "the next websocket frame")
    }
    fn text(&self, setup: &Setup) -> Value {
        match self.next(setup) {
            Frame::Text(value) => value,
            frame @ (Frame::Close(_) | Frame::Eof) => panic!("expected a JSON line, got {frame:?}"),
        }
    }
}
impl Drop for Ws {
    fn drop(&mut self) {
        self.stream.shutdown(Shutdown::Both).unwrap_or(());
    }
}
fn read_frame(read: &mut BufReader<TcpStream>) -> Option<Frame> {
    let mut head = [0; 2];
    read.read_exact(&mut head).ok()?;
    let [opcode, len] = head;
    let mut length = usize::from(len & 127);
    if length == 126 {
        let mut bytes = [0; 2];
        read.read_exact(&mut bytes).ok()?;
        length = usize::from(u16::from_be_bytes(bytes));
    } else if length == 127 {
        let mut bytes = [0; 8];
        read.read_exact(&mut bytes).ok()?;
        length = usize::try_from(u64::from_be_bytes(bytes)).ok()?;
    }
    let mut payload = vec![0; length];
    read.read_exact(&mut payload).ok()?;
    match opcode & 15 {
        1 => Some(Frame::Text(serde_json::from_slice(&payload).unwrap())),
        8 => {
            let bytes: [u8; 2] = payload.get(..2)?.try_into().ok()?;
            Some(Frame::Close(u16::from_be_bytes(bytes)))
        }
        _ => None,
    }
}
#[test]
fn a_revoked_token_is_refused_over_the_port() {
    let setup = Setup::new();
    let served = hub_start::serve_installed(
        &setup,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            write_json(
                &setup.home().join("config.json"),
                &json!({"hub":{"port":port}}),
            );
            setup.fiber(&["hub", "serve", "--installed"])
        },
    );
    let port = served.port;
    let (stream, hello) = served.hub;
    assert_eq!(hello.kind, "hub_hello");
    let local = Socket::from(setup.deadline, stream);
    let output = run(&setup, &["hub", "pair", "phone"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = String::from_utf8(output.stdout.clone())
        .unwrap()
        .trim()
        .to_owned();
    let mut client = Ws::connect(&setup, port);
    assert_eq!(client.text(&setup)["kind"], "hub_hello");
    client.send(
        &setup,
        json!({"id":"p","command":"pair","args":{"code":code}}),
    );
    let accepted = client.text(&setup);
    assert_eq!(accepted["kind"], "command_accepted");
    let token = accepted["payload"]["result"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(token.len(), 43);
    assert_eq!(client.text(&setup)["kind"], "device_changed");
    assert_eq!(
        recv(&local, "local pairing announcement")["kind"],
        "device_changed"
    );
    let revoked = run(&setup, &["hub", "token", "revoke", "phone"]);
    assert_eq!(revoked.status.code(), Some(0));
    assert_eq!(client.text(&setup)["payload"]["change"], "revoked");
    assert!(matches!(client.next(&setup), Frame::Close(1000)));
    assert!(matches!(client.next(&setup), Frame::Eof));
    let mut again = Ws::connect(&setup, port);
    assert_eq!(again.text(&setup)["kind"], "hub_hello");
    again.send(
        &setup,
        json!({"id":"a","command":"authenticate","args":{"token":token}}),
    );
    let refused = again.text(&setup);
    assert_eq!(refused["payload"]["code"], "unauthenticated");
    assert!(!refused.to_string().contains(&token));
    assert!(matches!(again.next(&setup), Frame::Close(1008)));
    assert!(matches!(again.next(&setup), Frame::Eof));
    assert!(!String::from_utf8(output.stdout).unwrap().contains(&token));
    assert!(!String::from_utf8(revoked.stdout).unwrap().contains(&token));
    assert!(
        !fs::read_to_string(hub_log_path(&setup))
            .unwrap()
            .contains(&token)
    );
    drop((client, again, local));
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
}
