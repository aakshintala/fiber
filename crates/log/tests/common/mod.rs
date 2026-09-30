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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use contract::events::Event;
use contract::{Envelope, SessionId};
use serde_json::{Value, json};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary directory standing in for a project's `sessions/`, removed
/// when dropped.
pub(crate) struct TestDir(PathBuf);

impl TestDir {
    pub(crate) fn new(name: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("log-{name}-{}-{n}", std::process::id()));
        fs::remove_dir_all(&path).unwrap_or(());
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// The session directory of `id` under this directory.
    pub(crate) fn session(&self, id: &SessionId) -> PathBuf {
        self.0.join(&id.0)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap_or(());
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
    event("session_started", json!({"workspace": "/w"}))
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
