#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test support shared by unit tests, integration tests and the jigs; each uses a different part"
)]

use std::path::Path;

use contract::events::Event;
use contract::{Envelope, SessionId};
use serde_json::Value;

/// One complete envelope line ending in `\n`, `schema_version` 1.
pub(crate) fn line(kind: &str, session: &str, ts: u64, seq: u64, payload: &Value) -> String {
    let line = serde_json::json!({
        "kind": kind,
        "session_id": session,
        "ts": ts,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    });
    format!("{line}\n")
}

/// Creates `dir` and writes its `events.jsonl` as given. Partial envelopes
/// and corrupt bytes are allowed.
pub(crate) fn session_log(dir: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), bytes).unwrap();
}

/// Appends to an existing log.
pub(crate) fn append_raw(dir: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

/// Line `index`'s start and its length without the newline.
pub(crate) fn line_at(bytes: &[u8], index: usize) -> (usize, usize) {
    let mut start = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n').take(index) {
        start += line.len();
    }
    let len = bytes[start..].iter().position(|b| *b == b'\n').unwrap();
    (start, len)
}

/// Overwrites line `index` with `x` bytes of its length and returns its
/// original bytes.
pub(crate) fn corrupt(dir: &Path, index: usize) -> Vec<u8> {
    use std::os::unix::fs::FileExt as _;
    let path = dir.join("events.jsonl");
    let whole = std::fs::read(&path).unwrap();
    let (start, len) = line_at(&whole, index);
    let original = whole[start..start + len].to_vec();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .write_all_at(&vec![b'x'; len], start as u64)
        .unwrap();
    original
}

/// Truncates at byte `at` and returns the bytes cut.
pub(crate) fn cut(dir: &Path, at: usize) -> Vec<u8> {
    let path = dir.join("events.jsonl");
    let whole = std::fs::read(&path).unwrap();
    let cut = whole[at..].to_vec();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(at as u64)
        .unwrap();
    cut
}

/// An event of `kind` with `payload`, read the way a consumer reads one.
pub(crate) fn event(kind: &str, payload: Value) -> Event {
    let Value::Object(payload) = payload else {
        panic!("a payload is an object");
    };
    let line = Envelope {
        kind: kind.to_owned(),
        session_id: SessionId("s".to_owned()),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    };
    Event::from_envelope(&line).unwrap().unwrap()
}
