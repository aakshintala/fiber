//! The app tests' shared fixtures: the `hub_hello` line, the injected
//! clock's now, and a hub that rejects every command. Fixtures never
//! sleep, read the wall clock, or advance a clock; a test advances a
//! fake clock only after a signal that the code under test is waiting
//! on it.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Instant;

use contract::clock::Clock;
use serde_json::json;

use super::{App, Effect};
use crate::configure::ConfigureError;
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::link::Line;
use crate::remote::{Remote, Router};
use fakes::Deadline;

/// The session every app test attaches to.
pub(crate) const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// A `hub_hello` this terminal reads.
pub(crate) fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// The injected clock's now.
pub(crate) fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// An app on home at `w` by `h`, launched with `launch`: home first, then
/// the size, the order every home test already builds.
pub(crate) fn home_with(w: u16, h: u16, launch: Launch) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch);
    app.set_size(w, h);
    app
}

/// An app attached to `SESSION` at `width` by `height`: new, then the
/// size, then attach, the order every attach test already builds.
pub(crate) fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// The sender session a named message comes from: not the attached one.
pub(crate) const SENDER: &str = "s_bbbbbbbbbbbbbbbb";

/// A `session_status` for [`SENDER`], titled `name`, naming it only when
/// `named`: a status from the hub's feed carries the sender's actual
/// name, its title only as a display fallback.
pub(crate) fn sender_status(name: &str, named: bool) -> Line {
    let mut payload = json!({
        "name": name,
        "workspace": "/w",
        "project": "-w",
        "state": "idle",
        "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    if named {
        payload["named"] = json!(true);
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(SENDER.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A live `session_status` for `session`, streaming.
pub(crate) fn live(session: contract::SessionId) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: session,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model",
            "delegates": 0,
            "jobs": 0,
            "clients": 0,
            "state": "streaming",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// A remote hub whose `global_file` read is rejected: every
/// no-workspace configuration read through it fails. It records every
/// line the terminal sends and answers each at once with a rejection,
/// so an unexpected call fails the test's assertion instead of hanging
/// on a wait.
pub(crate) struct FailingHub {
    router: Arc<Router>,
    pub(crate) lines: mpsc::Receiver<serde_json::Value>,
    pub(crate) remote: Arc<Remote>,
}

impl FailingHub {
    pub(crate) fn new() -> Self {
        let router = Router::new();
        let (client, hub) = UnixStream::pair().unwrap();
        router.attach(client);
        let (tx, lines) = mpsc::channel();
        let router_ref = Arc::downgrade(&router);
        thread::spawn(move || {
            for line in BufReader::new(hub).lines() {
                let Ok(line) = line else {
                    break;
                };
                let line: serde_json::Value = serde_json::from_str(&line).unwrap();
                if tx.send(line.clone()).is_err() {
                    break;
                }
                let response: contract::HubLine = serde_json::from_value(json!({
                    "kind": "command_rejected",
                    "ts": 1,
                    "schema_version": 1,
                    "payload": {
                        "command_id": line["id"],
                        "code": "stale_request",
                        "message": "hub message"
                    }
                }))
                .unwrap();
                let Some(router) = router_ref.upgrade() else {
                    break;
                };
                // Answer now so a regression fails an assertion instead of hanging.
                let _ = router.route(&Line::Hub(response));
            }
        });
        let remote = Arc::new(Remote::new(
            Arc::new(Fake::new(vec![])),
            PathBuf::from("/client"),
            Arc::clone(&router),
        ));
        Self {
            router,
            lines,
            remote,
        }
    }

    /// Rejects the next `global_file` ask and returns its failure.
    pub(crate) fn reject_global_file(&self) -> ConfigureError {
        let deadline = Deadline::start();
        let remote = Arc::clone(&self.remote);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            tx.send(remote.global_parent()).unwrap_or(());
        });
        let line = deadline.recv_or_fail(&self.lines, "global file ask");
        assert_eq!(line["command"], "configure");
        assert_eq!(line["args"], json!({"operation": "global_file"}));
        deadline
            .recv_or_fail(&rx, "global parent answer")
            .unwrap_err()
    }
}

impl Drop for FailingHub {
    fn drop(&mut self) {
        self.router.detach();
    }
}

/// One session envelope, in `turn` when one is given.
fn envelope(
    session: &str,
    kind: &str,
    turn: Option<&str>,
    payload: serde_json::Value,
    action: Option<&str>,
) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: turn.map(|id| contract::TurnId(id.to_owned())),
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// One session envelope.
pub(crate) fn session_line(
    session: &str,
    kind: &str,
    payload: serde_json::Value,
    action: Option<&str>,
) -> Line {
    envelope(session, kind, None, payload, action)
}

/// One session envelope in `turn`, such as a usage or turn-started line.
pub(crate) fn session_line_in_turn(
    session: &str,
    kind: &str,
    turn: &str,
    payload: serde_json::Value,
    action: Option<&str>,
) -> Line {
    envelope(session, kind, Some(turn), payload, action)
}

/// Feeds `count` completed one-line driver turns into `app`, oldest
/// first, with sequence numbers from `seq`, so the conversation grows
/// taller than the window and scrolls. A wheel test scrolls up from the
/// followed bottom first, making a wheel that reaches the conversation
/// move its top.
pub(crate) fn long_turns(app: &mut App, seq: &mut u64, count: u64) {
    for at in 0..count {
        let message = format!("a_m{at}");
        let mut push = |kind: &str, action: Option<&str>, payload: serde_json::Value| {
            app.on_line(Line::Session(contract::Envelope {
                kind: kind.to_owned(),
                session_id: contract::SessionId(SESSION.to_owned()),
                ts: *seq,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: action.map(|id| contract::ActionId(id.to_owned())),
                seq: Some(contract::Seq(*seq)),
                payload: payload.as_object().cloned().unwrap_or_default(),
            }));
            *seq += 1;
        };
        push(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "prompt"}]}]}),
        );
        push("step_started", None, json!({}));
        push("assistant_message_started", Some(&message), json!({}));
        push(
            "text_completed",
            Some(&message),
            json!({"text": "a one-line reply"}),
        );
        push(
            "assistant_message_completed",
            Some(&message),
            json!({"outcome": "completed"}),
        );
        push("turn_completed", None, json!({"outcome": "completed"}));
    }
}

/// Every line an effect sends, parsed.
pub(crate) fn sent(effect: Effect) -> Vec<serde_json::Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. } => Vec::new(),
    }
}
