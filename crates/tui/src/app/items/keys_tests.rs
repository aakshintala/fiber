//! Tests for typing into a running `tty` job's view: the encoding table,
//! the boundaries against finished, plain and delegate views, and Esc,
//! Ctrl+C and a down link falling through as today.

use std::path::PathBuf;

use contract::clock::Clock;
use contract::SessionId;
use serde_json::{Value, json};

use super::super::super::{App, Effect};
use super::{encode, is_tty};
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const DELEGATE_A: &str = "s_dddddddddddddddd";

/// An app on home at 80x24, drawing the default cards.
fn home() -> App {
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
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: Value) -> Line {
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
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The `subscribe` lines among `out`.
fn subscribes(out: &[Value]) -> Vec<Value> {
    out.iter()
        .filter(|line| line["command"] == "subscribe")
        .cloned()
        .collect()
}

/// Links the app and opens the attached session through home.
fn opened(app: &mut App) {
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
fn live(session: &str, state: Value) -> Line {
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
fn session_accepted(session: &str, id: &str) -> Line {
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
fn start_delegate(app: &mut App, job: &str, delegate: &str) {
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

/// Folds a plain job as `job`, with no pseudo-terminal.
fn start_job(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
}

/// Folds a `shell` call asking for a pseudo-terminal on action `a_1`, so
/// the next `job_started` on that action takes a grid.
fn mark_tty(app: &mut App) {
    app.on_line(session_line(
        "tool_call_requested",
        json!({"name": "shell", "arguments": {"tty": true}}),
    ));
}

/// One `job_started` at `ts` from tool call `action`.
fn job_started(app: &mut App, job: &str, description: &str, ts: u64, action: &str) {
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

/// Folds the attached session's `job_completed` for `job`.
fn complete(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_completed",
        json!({"job_id": job, "status": "completed"}),
    ));
}

/// Opens `job`'s item view, returning the parsed lines going out.
fn open(app: &mut App, job: &str) -> Vec<Value> {
    match app.open_item(&contract::JobId(job.to_owned())) {
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
fn ack_all(app: &mut App, out: &[Value]) {
    for line in subscribes(out) {
        let id = line["id"].as_str().unwrap_or_else(|| panic!("an id"));
        let session = line["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("a session"));
        app.on_line(session_accepted(session, id));
    }
}

/// Opens `job`'s item view, acknowledged.
fn opened_item(app: &mut App, job: &str) {
    let out = open(app, job);
    ack_all(app, &out);
}

/// Opens a running `tty` job's view, acknowledged.
fn open_tty(app: &mut App, job: &str) {
    mark_tty(app);
    job_started(app, job, "run the editor", 0, "a_1");
    opened_item(app, job);
    assert!(app.item_open());
}

/// The `job_input` lines an effect sends.
fn job_inputs(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => commands(lines)
            .into_iter()
            .filter(|line| line["command"] == "job_input")
            .collect(),
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
        | Effect::ReadImage(_) => Vec::new(),
    }
}

#[test]
fn every_key_encodes_to_the_bytes_the_job_reads() {
    let rows: Vec<(Key, &str)> = vec![
        (Key::Char('a'), "a"),
        // Multi-byte input goes as its UTF-8.
        (Key::Char('é'), "é"),
        (Key::Enter, "\r"),
        (Key::Backspace, "\x7f"),
        (Key::Tab, "\t"),
        (Key::Up, "\x1b[A"),
        (Key::Down, "\x1b[B"),
        (Key::End, "\x1b[F"),
        (Key::PageUp, "\x1b[5~"),
        (Key::PageDown, "\x1b[6~"),
        (Key::CtrlO, "\x0f"),
        (Key::CtrlG, "\x07"),
        (Key::CtrlR, "\x12"),
        (Key::CtrlF, "\x06"),
        (Key::CtrlV, "\x16"),
        (Key::CtrlL, "\x0c"),
    ];
    for (key, expected) in rows {
        assert_eq!(encode(&key).as_deref(), Some(expected), "{key:?}");
    }
}

#[test]
fn esc_ctrl_c_and_keys_the_enum_cannot_name_send_nothing() {
    for key in [
        Key::Esc,
        Key::CtrlC,
        Key::BackTab,
        Key::F1,
        Key::AltA,
        Key::AltUp,
        Key::AltDown,
        Key::AltX,
        Key::AltP,
        Key::AltR,
        Key::AltDigit(3),
    ] {
        assert_eq!(encode(&key), None, "{key:?}");
    }
}

#[test]
fn a_tty_job_is_the_one_holding_a_grid() {
    let mut app = home();
    opened(&mut app);
    mark_tty(&mut app);
    job_started(&mut app, "j_9", "run the editor", 0, "a_1");
    start_job(&mut app, "j_2");
    let jobs = &app.items.jobs;
    assert!(is_tty(&jobs[&contract::JobId("j_9".to_owned())]));
    assert!(!is_tty(&jobs[&contract::JobId("j_2".to_owned())]));
}

#[test]
fn typing_in_a_running_tty_job_view_sends_job_input_to_the_parent() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    let lines = job_inputs(app.on_key(Key::Char('a'), clock.now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "job_input");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"job_id": "j_9", "text": "a"}));
    assert!(
        lines[0]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("c_")),
        "{}",
        lines[0]
    );
    let enter = job_inputs(app.on_key(Key::Enter, clock.now()));
    assert_eq!(enter.len(), 1);
    assert_eq!(enter[0]["args"], json!({"job_id": "j_9", "text": "\r"}));
}

#[test]
fn a_finished_tty_job_falls_through() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    complete(&mut app, "j_9");
    assert!(job_inputs(app.on_key(Key::Char('a'), clock.now())).is_empty());
}

#[test]
fn a_plain_job_view_keeps_its_notice() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    opened_item(&mut app, "j_1");
    app.draft.set("do it");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "do it");
    assert_eq!(app.notice(), Some("A job takes no input here."));
}

#[test]
fn a_delegate_view_keeps_its_draft() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    assert!(job_inputs(app.on_key(Key::Char('x'), clock.now())).is_empty());
    assert_eq!(app.draft(), "x");
}

#[test]
fn esc_closes_the_tty_view_without_sending() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    assert_eq!(app.on_key(Key::Esc, clock.now()), Effect::None);
    assert!(!app.item_open());
}

#[test]
fn ctrl_c_keeps_its_quit_gesture_in_a_tty_view() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    assert!(app.draft.is_empty());
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert!(app.item_open());
}

#[test]
fn a_down_link_sends_nothing_in_a_tty_view() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    app.connect_failed("down".to_owned());
    assert!(job_inputs(app.on_key(Key::Char('a'), clock.now())).is_empty());
    assert!(app.item_open());
}
