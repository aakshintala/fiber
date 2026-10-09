//! Tests for the live output fold: each running job's `job_delta` text feeds
//! its own bounded output, a `tty` job gets a grid and any other lines,
//! and completion drops the text unless the job's view is open.

use std::path::PathBuf;

use contract::{ActionId, JobId, SessionId};
use serde_json::{Value, json};

use super::super::{App, Effect};
use crate::home::Launch;
use crate::link::Line;

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

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// One envelope of the attached session with `action_id`.
fn line(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|action| ActionId(action.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
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

/// Parses command lines going out.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
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

/// Starts job `job` from tool call `action`.
fn start_job(app: &mut App, job: &str, action: &str) {
    app.on_line(line(
        "job_started",
        json!({"job_id": job, "tool": "shell",
            "description": format!("task {job}"), "output_path": "/tmp/out"}),
        Some(action),
    ));
}

/// Feeds `text` as job `job`'s delta.
fn delta(app: &mut App, job: &str, text: &str) {
    app.on_line(line(
        "job_delta",
        json!({"job_id": job, "text": text}),
        None,
    ));
}

/// Completes job `job`.
fn complete(app: &mut App, job: &str) {
    app.on_line(line(
        "job_completed",
        json!({"job_id": job, "status": "completed"}),
        None,
    ));
}

/// A `shell` call's request with `arguments`, from tool call `action`.
fn request(app: &mut App, action: &str, name: &str, arguments: Value) {
    app.on_line(line(
        "tool_call_requested",
        json!({"name": name, "arguments": arguments}),
        Some(action),
    ));
}

/// A `shell` call's request with a repair, from tool call `action`.
fn request_repaired(app: &mut App, action: &str, arguments: Value, repaired: Value) {
    app.on_line(line(
        "tool_call_requested",
        json!({"name": "shell", "arguments": arguments,
            "repaired": repaired, "repairs": []}),
        Some(action),
    ));
}

/// A tool call's start with rewritten `arguments`, from tool call `action`.
fn rewrite(app: &mut App, action: &str, arguments: Value) {
    app.on_line(line(
        "tool_call_started",
        json!({"effects": [], "reversible": false, "arguments": arguments}),
        Some(action),
    ));
}

/// Opens job `job`'s view; opening sends or nothing, never anything else.
fn open_job(app: &mut App, job: &str) {
    match app.open_item(&JobId(job.to_owned())) {
        Effect::None | Effect::Send(_) => {}
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

/// Opens job `job`'s view, then probes it with a cursor move only a grid
/// honors: `A`, home, `B` reads `B` on a grid and `AB` on lines.
fn is_tty(app: &mut App, job: &str) -> bool {
    open_job(app, job);
    delta(app, job, "A\x1b[1;1HB");
    let rows = app.item_output_rows(80, 24);
    rows.first().is_some_and(|row| row.trim_end() == "B")
}

#[test]
fn deltas_feed_one_jobs_output_in_order_and_never_anothers() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1", "a_1");
    start_job(&mut app, "j_2", "a_2");
    // A delta for a job never started, and one with no text, change
    // nothing and panic nowhere.
    delta(&mut app, "j_zzz", "lost");
    app.on_line(line("job_delta", json!({"job_id": "j_1"}), None));
    delta(&mut app, "j_1", "hello ");
    delta(&mut app, "j_2", "other");
    delta(&mut app, "j_1", "world");
    open_job(&mut app, "j_1");
    let rows = app.item_output_rows(80, 24);
    assert!(
        rows.first()
            .is_some_and(|row| row.starts_with("hello world"))
    );
    open_job(&mut app, "j_2");
    let rows = app.item_output_rows(80, 24);
    assert!(rows.first().is_some_and(|row| row.starts_with("other")));
}

#[test]
fn a_shell_call_with_tty_marks_its_job() {
    let mut app = home();
    opened(&mut app);
    request(&mut app, "a_9", "shell", json!({"tty": true}));
    start_job(&mut app, "j_9", "a_9");
    assert!(is_tty(&mut app, "j_9"));
}

#[test]
fn a_repair_with_tty_wins_over_arguments_without() {
    let mut app = home();
    opened(&mut app);
    request_repaired(&mut app, "a_9", json!({"tty": false}), json!({"tty": true}));
    start_job(&mut app, "j_9", "a_9");
    assert!(is_tty(&mut app, "j_9"));
}

#[test]
fn a_rewritten_tty_marks_its_job() {
    let mut app = home();
    opened(&mut app);
    request(&mut app, "a_9", "shell", json!({}));
    rewrite(&mut app, "a_9", json!({"tty": true}));
    start_job(&mut app, "j_9", "a_9");
    assert!(is_tty(&mut app, "j_9"));
}

#[test]
fn without_tty_the_job_gets_lines() {
    for (name, arguments) in [
        ("shell", json!({"tty": false})),
        ("shell", json!({})),
        ("read", json!({"tty": true})),
    ] {
        let mut app = home();
        opened(&mut app);
        request(&mut app, "a_9", name, arguments);
        start_job(&mut app, "j_9", "a_9");
        assert!(!is_tty(&mut app, "j_9"), "{name}");
    }
}

#[test]
fn the_mark_goes_after_its_job_started() {
    let mut app = home();
    opened(&mut app);
    request(&mut app, "a_9", "shell", json!({"tty": true}));
    start_job(&mut app, "j_9", "a_9");
    assert!(is_tty(&mut app, "j_9"));
    // The same action id starting another job finds no mark left.
    start_job(&mut app, "j_10", "a_9");
    assert!(!is_tty(&mut app, "j_10"));
}

#[test]
fn completion_drops_the_text_unless_its_view_is_open() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1", "a_1");
    delta(&mut app, "j_1", "gone");
    complete(&mut app, "j_1");
    open_job(&mut app, "j_1");
    let rows = app.item_output_rows(80, 24);
    assert!(rows.iter().all(|row| row.trim().is_empty()));
}

#[test]
fn completion_keeps_the_text_while_its_view_is_open() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1", "a_1");
    delta(&mut app, "j_1", "kept");
    open_job(&mut app, "j_1");
    complete(&mut app, "j_1");
    let rows = app.item_output_rows(80, 24);
    assert!(rows.first().is_some_and(|row| row.starts_with("kept")));
}
