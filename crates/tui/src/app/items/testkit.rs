//! Shared fixture for the item-view tests: an attached app and the lines that fold its jobs.

use std::path::PathBuf;

use contract::{JobId, SessionId};
use serde_json::{Value, json};

use super::super::{App, Effect};
use crate::home::Launch;
use crate::link::Line;

/// The attached session every fixture app opens.
pub(crate) const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
/// One delegate session.
pub(crate) const DELEGATE_A: &str = "s_dddddddddddddddd";

/// An app on home at 80x24, drawing the default cards.
pub(crate) fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: ["session", "changed_files", "delegates", "jobs", "quota"]
            .map(str::to_owned)
            .to_vec(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A `hub_hello` this terminal reads.
pub(crate) fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// One envelope of the attached session.
pub(crate) fn session_line(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Parses command lines going out.
pub(crate) fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The `subscribe` lines among `out`.
pub(crate) fn subscribes(out: &[Value]) -> Vec<Value> {
    out.iter()
        .filter(|line| line["command"] == "subscribe")
        .cloned()
        .collect()
}

/// Links the app and opens the attached session through home.
pub(crate) fn opened(app: &mut App) {
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    let key = app
        .home_screen()
        .map(|screen| {
            screen
                .rows
                .into_iter()
                .map(|(key, _, _)| key)
                .collect::<Vec<_>>()
        })
        .and_then(|keys| keys.into_iter().next())
        .unwrap_or_else(|| panic!("a row"));
    let out = match app.on_click(crate::mouse::TargetId::Home(crate::home::Spot::Entry(key))) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("opening sends"),
    };
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    app.on_line(session_accepted(SESSION, &id));
}

/// A live `session_status` for `session` in `state`.
pub(crate) fn live(session: &str, state: Value) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": "/w", "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A session `command_accepted` for `id` from `session`.
pub(crate) fn session_accepted(session: &str, id: &str) -> Line {
    let mut payload = serde_json::Map::new();
    payload.insert("command_id".to_owned(), Value::String(id.to_owned()));
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    })
}

/// Folds a Fiber delegate as job `job` with session `delegate`.
pub(crate) fn start_delegate(app: &mut App, job: &str, delegate: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": job,
            "delegate_session_id": delegate,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// Folds the attached session's `job_completed` for `job`.
pub(crate) fn complete(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_completed",
        json!({"job_id": job, "status": "completed"}),
    ));
}

/// Opens `job`'s item view, returning the parsed lines going out.
pub(crate) fn open(app: &mut App, job: &str) -> Vec<Value> {
    match app.open_item(&JobId(job.to_owned())) {
        Effect::Send(lines) => commands(lines),
        Effect::None => Vec::new(),
        Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("opening sends or nothing"),
    }
}

/// Acknowledges every `subscribe` in `out` from its session.
pub(crate) fn ack_all(app: &mut App, out: &[Value]) {
    for line in subscribes(out) {
        let id = line["id"].as_str().unwrap_or_else(|| panic!("an id"));
        let session = line["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("a session"));
        app.on_line(session_accepted(session, id));
    }
}

/// Folds a plain job as `job`, with no delegate.
pub(crate) fn start_job(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
}

/// One `job_started` at `ts` from tool call `action`.
pub(crate) fn job_started(app: &mut App, job: &str, description: &str, ts: u64, action: &str) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "job_started".to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId(action.to_owned())),
        seq: None,
        payload: json!({"job_id": job, "description": description,
            "output_path": "/tmp/out"})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
}

/// Folds a `shell` call asking for a pseudo-terminal on action `a_1`, so
/// the next `job_started` on that action takes a grid.
pub(crate) fn mark_tty(app: &mut App) {
    app.on_line(session_line(
        "tool_call_requested",
        json!({"name": "shell", "arguments": {"tty": true}}),
    ));
}

/// Opens `job`'s item view, acknowledged.
pub(crate) fn opened_item(app: &mut App, job: &str) {
    let out = open(app, job);
    ack_all(app, &out);
}
