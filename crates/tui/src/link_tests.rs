//! Tests for hub lines out and in.

use super::{Line, parse_line, read_lines, write_line};
use crate::Input;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn written_lines_are_exact_json() {
    let (send, mut recv) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let line =
        serde_json::json!({"id": "c_1", "command": "cancel", "session_id": "s_x"}).to_string();
    write_line(&send, &line).unwrap_or_else(|err| panic!("write: {err}"));
    let mut buf = vec![0u8; line.len() + 1];
    // One deadline for the whole read, however many reads it takes.
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("link-read-exact".to_owned())
        .spawn(move || {
            done.send(recv.read_exact(&mut buf).map(|()| buf))
                .unwrap_or(())
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let buf = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the line: {err}"))
        .unwrap_or_else(|err| panic!("read: {err}"));
    assert_eq!(buf, format!("{line}\n").into_bytes());
}

#[test]
fn lines_read_split_into_hub_and_session() {
    let (client, hub) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("link-read".to_owned())
        .spawn(move || read_lines(hub, &tx))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let mut client = client;
    for line in [
        serde_json::json!({"kind": "command_accepted", "ts": 0, "schema_version": 1, "payload": {}}).to_string(),
        serde_json::json!({"kind": "turn_started", "session_id": "s_x", "ts": 0, "schema_version": 1, "payload": {"input": []}}).to_string(),
    ] {
        use std::io::Write;
        client
            .write_all(format!("{line}\n").as_bytes())
            .unwrap_or_else(|err| panic!("write: {err}"));
    }
    let first = rx
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the hub line: {err}"));
    let second = rx
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the session line: {err}"));
    assert!(matches!(first, Input::Hub(Line::Hub(_))));
    assert!(matches!(second, Input::Hub(Line::Session(_))));
    drop(client);
    let end = rx
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the disconnect: {err}"));
    assert!(matches!(end, Input::Disconnected));
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
}

#[test]
fn parse_line_splits_and_refuses_malformed() {
    assert!(matches!(
        parse_line(r#"{"kind":"hub_hello","ts":0,"schema_version":1,"payload":{}}"#),
        Some(Line::Hub(_))
    ));
    assert!(matches!(
        parse_line(
            r#"{"kind":"turn_started","session_id":"s_x","ts":0,"schema_version":1,"payload":{"input":[]}}"#
        ),
        Some(Line::Session(_))
    ));
    assert!(parse_line("not json").is_none());
    assert!(parse_line(r#"{"kind": 1}"#).is_none());
}
