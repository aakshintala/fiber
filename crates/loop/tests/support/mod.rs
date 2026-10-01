//! What the loop's tests and its `turn` jig share: scripted replies on the
//! provider seam, and a session wired to them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code,
    missing_docs,
    reason = "test code, helpers included; each test binary uses some helpers"
)]

use std::hash::BuildHasher as _;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::events::{
    ReasoningCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested, TurnOutcome,
};
use contract::inbox::Message;
use contract::provider::{Delta, ModelCall, ModelRequest, Provider, ReplyAction};
use contract::shapes::{ContentPart, Origin, Sender as From};
use contract::{CommandId, Envelope, SessionId};
use fakes::{Scripted, ScriptedProvider, reply};
use log::Log;
use r#loop::Loop;
use serde_json::{Value, json};

/// How long a turn may take before a test fails instead of hanging.
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// The model reference the scripted provider answers as.
pub(crate) const MODEL: &str = "fake/model-1";

/// A reply with readable reasoning, then `text`.
pub(crate) fn reasoning_reply(thought: &str, text: &str) -> Scripted {
    let mut end = reply(text);
    end.actions = vec![ReplyAction::Reasoning(ReasoningCompleted {
        text: thought.into(),
        provider_item: Some(reasoning_item(thought)),
    })];
    Scripted {
        deltas: vec![
            Delta::Reasoning(TextDelta {
                text: thought.into(),
            }),
            Delta::Text(TextDelta { text: text.into() }),
        ],
        end: Ok(end),
    }
}

/// A reply that calls `names`, tools no one registered, after `text`.
pub(crate) fn tool_call_reply(text: &str, names: &[&str]) -> Scripted {
    let mut end = reply(text);
    let mut deltas = vec![Delta::Text(TextDelta { text: text.into() })];
    for (index, name) in names.iter().enumerate() {
        deltas.push(Delta::ToolCallArguments(ToolCallArgumentsDelta {
            index: u32::try_from(index).unwrap(),
            name: Some((*name).into()),
            text: "{\"city\":\"Paris\"}".into(),
        }));
        end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
            name: (*name).into(),
            arguments: json!({"city": "Paris"}),
            provider_id: None,
            repair: None,
        }));
    }
    Scripted {
        deltas,
        end: Ok(end),
    }
}

/// The reasoning item a reasoning reply carries, as a provider sent it.
pub(crate) fn reasoning_item(thought: &str) -> Value {
    json!({"type": "reasoning", "encrypted_content": "gAAA-opaque", "summary": thought})
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

/// The scripted provider, sending `during` to the inbox as the first call is
/// made, so it arrives while that reply streams.
struct Seam {
    inner: Arc<ScriptedProvider>,
    /// Taken by the first call, so the loop sees the inbox close once the
    /// test drops its own sender.
    during: Mutex<Option<(Message, Sender<Message>)>>,
}

impl Provider for Seam {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        if let Some((message, inbox)) = self.during.lock().unwrap().take() {
            inbox.send(message).unwrap();
        }
        self.inner.call(request)
    }
}

/// A session on a fresh log, its loop wired to a scripted provider.
pub(crate) struct Session {
    pub(crate) provider: Arc<ScriptedProvider>,
    pub(crate) log: Arc<Log>,
    pub(crate) dir: PathBuf,
    pub(crate) inbox: Sender<Message>,
    lines: mpsc::Receiver<Envelope>,
    pub(crate) looped: Option<Loop>,
    _home: TempDir,
}

impl Session {
    /// Answers each model call with the next of `script`. The first model
    /// call sends `during` to the inbox, when there is one.
    pub(crate) fn new(script: Vec<Scripted>, during: Option<Message>) -> Self {
        let home = TempDir::new();
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
        let provider = Arc::new(ScriptedProvider::new(script));
        let seam = Seam {
            inner: Arc::clone(&provider),
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
            provider,
            dir: home.0.join(&id.0),
            log,
            inbox,
            lines,
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

    /// Every request the loop built, in order.
    pub(crate) fn requests(&self) -> Vec<ModelRequest> {
        self.provider.requests()
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
