//! Tests for the running delegates and jobs lists: the cards' rows with
//! one serial cell each, in start order, and the empty line.

use std::path::PathBuf;

use contract::SessionId;
use serde_json::{Value, json};

use super::*;
use crate::home::Launch;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

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

/// Links the app and opens the attached session through home.
fn opened(app: &mut App) {
    let lines: Vec<Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
        .collect();
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
    let out: Vec<Value> =
        match app.on_click(crate::mouse::TargetId::Home(crate::home::Spot::Entry(key))) {
            crate::app::Effect::Send(lines) => lines
                .iter()
                .map(|line| {
                    serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}"))
                })
                .collect(),
            crate::app::Effect::None
            | crate::app::Effect::Quit
            | crate::app::Effect::ListFiles
            | crate::app::Effect::FindPause { .. }
            | crate::app::Effect::Search { .. }
            | crate::app::Effect::Editor { .. }
            | crate::app::Effect::Exit(_)
            | crate::app::Effect::Copy(_)
            | crate::app::Effect::OpenLink(_)
            | crate::app::Effect::OpenFile(_)
            | crate::app::Effect::ReadImage(_) => panic!("opening sends"),
        };
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    app.on_line(session_accepted(SESSION, &id));
}

/// Folds a delegate as `job` with session `delegate`.
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

/// Folds a plain job as `job`.
fn start_job(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
}

/// Each row's serial cell, in frame order.
fn serials(frame: &Frame) -> Vec<u64> {
    frame
        .rows
        .iter()
        .map(|cells| {
            assert_eq!(cells.len(), 1);
            match cells[0].1 {
                Some(Spot::Item(serial)) => serial,
                _ => panic!("one serial cell per row"),
            }
        })
        .collect()
}

#[test]
fn delegates_frame_lists_two_rows_per_delegate_in_start_order() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    let frame = delegates_frame(&app, List::default());
    assert_eq!(frame.title, "Running delegates");
    assert_eq!(frame.rows.len(), 4);
    let first = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let second = app
        .serial_of_job(&contract::JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(serials(&frame), [first, first, second, second]);
    assert!(frame.rows[0][0].0.contains("test/model"));
    assert!(frame.rows[1][0].0.contains("task j_1"));
    assert!(frame.rows[3][0].0.contains("task j_2"));
    assert_eq!(frame.footer, "↑↓ move · Enter open · Esc close");
}

#[test]
fn jobs_frame_lists_one_row_per_job_and_no_delegate() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    start_delegate(&mut app, "j_2", "s_dddddddddddddddd");
    start_job(&mut app, "j_3");
    let frame = jobs_frame(&app, List::default());
    assert_eq!(frame.title, "Running jobs");
    assert_eq!(frame.rows.len(), 2);
    let first = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let third = app
        .serial_of_job(&contract::JobId("j_3".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(serials(&frame), [first, third]);
    assert!(frame.rows[0][0].0.contains("task j_1"));
    assert!(frame.rows[1][0].0.contains("task j_3"));
}

#[test]
fn empty_frames_draw_no_rows_and_say_nothing_running() {
    let mut app = home();
    opened(&mut app);
    for frame in [
        delegates_frame(&app, List::default()),
        jobs_frame(&app, List::default()),
    ] {
        assert!(frame.rows.is_empty());
        assert_eq!(frame.below, ["Nothing running."]);
    }
}
