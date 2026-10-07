//! What the `log` tests share: a temporary directory and events built from
//! their JSON.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "each test file uses a different part; test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use contract::events::Event;
use contract::{Envelope, SessionId};
use serde_json::{Value, json};

/// A temporary directory standing in for a project's `sessions/`, removed
/// when dropped.
pub(crate) struct TestDir(fakes::TempDir);

impl TestDir {
    pub(crate) fn new(name: &str) -> Self {
        let dir = fakes::TempDir::new(&format!("log-{name}"));
        // `Log::create` fsyncs each directory it makes, and a test counts
        // this path among them, so the name is claimed and then left absent.
        fs::remove_dir(dir.path()).unwrap();
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }

    /// The session directory of `id` under this directory.
    pub(crate) fn session(&self, id: &SessionId) -> PathBuf {
        self.path().join(&id.0)
    }
}

pub(crate) fn id(s: &str) -> SessionId {
    SessionId(s.to_owned())
}

/// An event of `kind` with `payload`, read the way a consumer reads one.
pub(crate) fn event(kind: &str, payload: Value) -> Event {
    let Value::Object(payload) = payload else {
        panic!("a payload is an object");
    };
    let line = Envelope {
        kind: kind.to_owned(),
        session_id: id("s"),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    };
    Event::from_envelope(&line).unwrap().unwrap()
}

pub(crate) fn empty(kind: &str) -> Event {
    event(kind, json!({}))
}

pub(crate) fn session_started() -> Event {
    event(
        "session_started",
        json!({"workspace": "/w",
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}}),
    )
}

pub(crate) fn delta(text: &str) -> Event {
    event("assistant_message_delta", json!({"text": text}))
}

pub(crate) fn message_completed() -> Event {
    event(
        "assistant_message_completed",
        json!({"outcome": "completed", "text": "hi"}),
    )
}

pub(crate) fn tool_call_started() -> Event {
    event(
        "tool_call_started",
        json!({"effects": [], "reversible": true}),
    )
}

pub(crate) fn tool_call_completed() -> Event {
    event(
        "tool_call_completed",
        json!({"status": "completed", "content": []}),
    )
}

/// The lines of a session directory's `events.jsonl`, as JSON.
pub(crate) fn lines(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Each line's `kind` and `seq`.
pub(crate) fn kinds_and_seqs(dir: &Path) -> Vec<(String, Option<u64>)> {
    lines(dir)
        .iter()
        .map(|l| (l["kind"].as_str().unwrap().to_owned(), l["seq"].as_u64()))
        .collect()
}

/// Overwrites line `index` (from 0) of the session's log in place with bytes
/// that do not parse, keeping its length, so every offset stays true.
pub(crate) fn corrupt(dir: &Path, index: usize) {
    let path = dir.join("events.jsonl");
    let whole = fs::read(&path).unwrap();
    let mut start = 0;
    for line in whole.split_inclusive(|b| *b == b'\n').take(index) {
        start += line.len();
    }
    let len = whole[start..].iter().position(|b| *b == b'\n').unwrap();
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all_at(&vec![b'x'; len], u64::try_from(start).unwrap())
        .unwrap();
}

/// How many lines a watcher's queue holds, and a catch-up page's size: the
/// log crate's `CAPACITY`.
pub(crate) const CAPACITY: usize = 1024;
