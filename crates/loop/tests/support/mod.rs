//! What the loop's tests and its `turn` jig share: scripted
//! `openai-responses` streams for the fake provider server, and a session
//! wired to it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code,
    missing_docs,
    reason = "test code, helpers included; each test binary uses some helpers"
)]

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::events::TurnOutcome;
use contract::provider::{ModelCall, ModelRequest, Provider};
use contract::shapes::{ContentPart, Origin, Sender as From};
use contract::{CommandId, Envelope, SessionId};
use fakes::{ProviderServer, Response};
use log::Log;
use r#loop::{Loop, Message};
use provider::Endpoint;
use provider::openai_responses::Responses;
use serde_json::{Value, json};

/// How long a turn may take before a test fails instead of hanging.
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// The model reference the fake provider answers as.
pub(crate) const MODEL: &str = "fake/model-1";

/// A server-sent event stream, one event per value.
pub(crate) fn stream(events: &[Value]) -> Response {
    Response::stream(
        events
            .iter()
            .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
            .collect::<String>(),
    )
}

/// A reply that streams `text` in two fragments and completes.
pub(crate) fn text_reply(text: &str) -> Response {
    let (head, tail) = text.split_at(text.len() / 2);
    stream(&[
        json!({"type": "response.output_text.delta", "delta": head}),
        json!({"type": "response.output_text.delta", "delta": tail}),
        message_done(text),
        completed(),
    ])
}

/// A reply with readable reasoning, then `text`.
pub(crate) fn reasoning_reply(thought: &str, text: &str) -> Response {
    stream(&[
        json!({"type": "response.reasoning_summary_text.delta", "delta": thought}),
        json!({"type": "response.output_item.done", "item": reasoning_item(thought)}),
        json!({"type": "response.output_text.delta", "delta": text}),
        message_done(text),
        completed(),
    ])
}

/// A reply that calls a tool no one registered.
pub(crate) fn tool_call_reply() -> Response {
    stream(&[
        json!({"type": "response.output_item.added", "item": {"type": "function_call", "id": "fc_1", "name": "get_weather"}}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"city\":\"Paris\"}"}),
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1",
            "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"
        }}),
        completed(),
    ])
}

/// The reasoning item a reasoning reply carries, as the provider sent it.
pub(crate) fn reasoning_item(thought: &str) -> Value {
    json!({
        "type": "reasoning", "id": "rs_1", "encrypted_content": "gAAA-opaque",
        "summary": [{"type": "summary_text", "text": thought}]
    })
}

fn message_done(text: &str) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "message", "role": "assistant",
        "content": [{"type": "output_text", "text": text}]
    }})
}

fn completed() -> Value {
    json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }})
}

/// A driver's message.
pub(crate) fn message(text: &str) -> Message {
    Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: From {
            origin: Origin::Driver,
            command_id: CommandId(format!("c_{text}")),
        },
    }
}

/// The provider seam in front of `openai-responses`: it records each
/// request the loop builds, and sends `during` to the inbox as the first
/// call is made, so it arrives while that reply streams.
struct Seam {
    inner: Responses,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    /// The message and the inbox's sender, taken by the first call, so the
    /// loop sees the inbox close once the test drops its own sender.
    during: Mutex<Option<(Message, Sender<Message>)>>,
}

impl Provider for Seam {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        self.requests.lock().unwrap().push(request.clone());
        if let Some((message, inbox)) = self.during.lock().unwrap().take() {
            inbox.send(message).unwrap();
        }
        self.inner.call(request)
    }
}

/// A session on a fresh log, its loop wired to the fake provider server.
pub(crate) struct Session {
    pub(crate) server: ProviderServer,
    pub(crate) log: Arc<Log>,
    pub(crate) dir: PathBuf,
    pub(crate) inbox: Sender<Message>,
    lines: mpsc::Receiver<Envelope>,
    /// Every request the loop built, in order.
    pub(crate) requests: Arc<Mutex<Vec<ModelRequest>>>,
    pub(crate) looped: Option<Loop>,
    _home: TempDir,
}

impl Session {
    /// Serves `script`, one response per request. The first model call sends
    /// `during` to the inbox, when there is one.
    pub(crate) fn new(script: Vec<Response>, during: Option<Message>) -> Self {
        let home = TempDir::new();
        let server = ProviderServer::start(script).unwrap();
        let id = SessionId("s_test".into());
        let log = Arc::new(Log::create(&home.0, id.clone()).unwrap());
        let mut watcher = log.watch();
        let (forward, lines) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(line)) = watcher.recv() {
                if forward.send(line).is_err() {
                    break;
                }
            }
        });
        let (inbox, rx) = mpsc::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seam = Seam {
            inner: Responses::new(Endpoint {
                provider: "fake".into(),
                model: "model-1".into(),
                base_url: server.url(),
                ..Endpoint::default()
            }),
            requests: Arc::clone(&requests),
            during: Mutex::new(during.map(|m| (m, inbox.clone()))),
        };
        let looped = Loop::start(
            Arc::clone(&log),
            Arc::new(seam),
            MODEL.into(),
            "You are terse.".into(),
            rx,
            "/work".into(),
        )
        .unwrap();
        Self {
            server,
            dir: home.0.join(&id.0),
            log,
            inbox,
            lines,
            requests,
            looped: Some(looped),
            _home: home,
        }
    }

    /// Runs one turn on its own thread, failing the test if it outlives
    /// [`DEADLINE`].
    pub(crate) fn turn(&mut self) -> Option<TurnOutcome> {
        let mut looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        let (looped, outcome) = finished
            .recv_timeout(DEADLINE)
            .expect("the turn ended in time");
        self.looped = Some(looped);
        outcome
    }

    /// Every line emitted since the last call, ephemeral ones included,
    /// through the next `turn_completed`.
    pub(crate) fn lines(&mut self) -> Vec<Envelope> {
        let mut lines = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(DEADLINE)
                .expect("a turn_completed line");
            let last = line.kind == "turn_completed";
            lines.push(line);
            if last {
                return lines;
            }
        }
    }
}

/// The kinds of `lines`, in order.
pub(crate) fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|l| l.kind.as_str()).collect()
}

/// A directory removed when dropped.
pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fiber-loop-{}-{:x}",
            std::process::id(),
            std::collections::hash_map::RandomState::new().hash_one(())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap_or(());
    }
}

use std::hash::BuildHasher as _;
