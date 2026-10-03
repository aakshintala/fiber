use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::TempDir;

const DEADLINE: Duration = Duration::from_secs(2);

/// `accept` on a thread, so the test's wait is the deadline below.
fn accept_within(listener: &UnixListener) -> UnixStream {
    let listener = listener.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || if let Ok(()) = tx.send(listener.accept()) {});
    let (stream, _) = rx
        .recv_timeout(DEADLINE)
        .expect("a client is accepted")
        .unwrap();
    stream
}

#[test]
fn a_paused_client_reads_nothing_until_told() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    client.slow(true);
    let mut server = accept_within(&listener);
    server.write_all(b"{\"ok\":true}\n").unwrap();
    assert!(
        client.recv(DEADLINE).is_none(),
        "a paused client reads nothing"
    );
    client.slow(false);
    let line = client
        .recv(DEADLINE)
        .expect("slow(false) starts the reader");
    assert_eq!(line["ok"], true);
}

#[test]
fn send_writes_the_line_and_a_newline() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let server = accept_within(&listener);
    client.send(r#"{"id":1}"#).unwrap();
    server.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut buf = Vec::new();
    BufReader::new(server)
        .read_until(b'\n', &mut buf)
        .expect("send writes the line and a newline");
    assert_eq!(buf, b"{\"id\":1}\n");
}

#[test]
fn recv_returns_a_buffered_line_without_waiting_for_the_socket_to_close() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"{\"ok\":true}\n").unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(
        move || {
            if let Ok(()) = tx.send(client.recv(Duration::from_secs(10))) {}
        },
    );
    let line = rx
        .recv_timeout(DEADLINE)
        .expect("recv returns when the line is buffered")
        .expect("recv returns the line");
    assert_eq!(line["ok"], true);
}

#[test]
fn a_line_keeps_no_trailing_cr_or_lf() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"hello\r\n").unwrap();
    let line = client.recv(DEADLINE).expect("a line arrives");
    assert_eq!(line, Value::String("hello".into()));
}

#[test]
fn a_running_client_stops_reading_once_paused() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"{\"n\":0}\n").unwrap();
    let first = client.recv(DEADLINE).expect("the first line arrives");
    assert_eq!(first["n"], 0);
    client.slow(true);
    for n in 1..100 {
        server
            .write_all(format!("{{\"n\":{n}}}\n").as_bytes())
            .unwrap();
    }
    let mut got = 0;
    while client.recv(DEADLINE).is_some() {
        got += 1;
    }
    assert!(
        got < 99,
        "a paused client stops reading, got {got} more lines"
    );
}

#[test]
fn drop_closes_the_socket() {
    let dir = TempDir::new("fc");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let client = Client::connect(&path).unwrap();
    let mut server = accept_within(&listener);
    server.write_all(b"{\"ok\":true}\n").unwrap();
    server.set_read_timeout(Some(DEADLINE)).unwrap();
    client.recv(DEADLINE).expect("the line arrives");
    drop(client);
    let mut buf = [0u8; 8];
    let n = server.read(&mut buf).expect("drop closes the socket");
    assert_eq!(n, 0, "drop closes the socket");
}
