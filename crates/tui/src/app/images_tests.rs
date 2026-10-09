//! Tests for opening an image in the system viewer: the `read_file`
//! wire, the queued view, the notices, and forgetting across sessions.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::{App, Effect};
use crate::app::Target;
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::TargetId;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

/// An app attached to [`SESSION`] with the link up, 60 columns wide.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Folds a turn whose prompt carried an image at `path`.
fn started_with_image(app: &mut App, path: &str) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [{
            "type": "message", "source": "driver",
            "content": [
                {"type": "text", "text": "look"},
                {"type": "image", "path": path,
                    "mime_type": "image/png",
                    "width": 1280, "height": 800},
            ],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
}

/// The command lines `effect` sends, panicking for any other effect.
fn sent(effect: Effect) -> Vec<String> {
    match effect {
        Effect::Send(lines) => lines,
        Effect::None
        | Effect::Quit
        | Effect::Exit(_)
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::OpenFile(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => panic!("expected a send"),
    }
}

/// Clicks the first image and returns its `read_file` line's id.
fn click(app: &mut App) -> String {
    let lines = sent(app.view_image(1));
    assert_eq!(lines.len(), 1);
    lines[0].clone()
}

/// Answers `read_file` `id` with `result`.
fn accept(app: &mut App, id: &str, result: Value) -> Vec<String> {
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "result": result})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }))
}

/// Rejects `read_file` `id` with `message`.
fn reject(app: &mut App, id: &str, message: &str) -> Vec<String> {
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "code": "not_found", "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }))
}

#[test]
fn a_click_on_an_unknown_image_sends_nothing() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    assert_eq!(app.view_image(7), Effect::None);
}

#[test]
fn a_click_sends_read_file_with_the_logged_path() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let value: Value = serde_json::from_str(&line).unwrap_or_else(|err| panic!("a line: {err}"));
    assert_eq!(value.get("command"), Some(&json!("read_file")));
    // The path stays as logged, `artifacts/` prefix and all.
    assert_eq!(
        value.get("args"),
        Some(&json!({"session": SESSION, "path": "artifacts/shot.png"}))
    );
    assert!(value.get("session_id").is_none());
}

#[test]
fn a_second_click_while_one_is_out_does_nothing() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let _ = click(&mut app);
    assert_eq!(app.view_image(1), Effect::None);
}

#[test]
fn the_answer_queues_a_view_with_the_bytes() {
    use base64::Engine as _;
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let bytes = base64::engine::general_purpose::STANDARD.encode(b"png");
    assert!(
        accept(
            &mut app,
            &id,
            json!({"data": bytes, "mime_type": "image/png"})
        )
        .is_empty()
    );
    let mut out = app.take_image_out();
    assert_eq!(out.view.len(), 1);
    let view = out.view.pop().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.id, 1);
    assert_eq!(view.name, "shot.png");
    assert_eq!(view.session, SESSION);
    assert_eq!(view.data, bytes);
    assert_eq!(view.generation, app.images.generation);
}

#[test]
fn a_rejection_is_a_notice_and_nothing_retries() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    assert!(reject(&mut app, &id, "no such file.").is_empty());
    assert_eq!(app.notice(), Some("Could not open shot.png: no such file."));
    // The refusal stands: a new click asks nothing again.
    assert_eq!(app.view_image(1), Effect::None);
}

#[test]
fn an_answer_without_data_is_a_notice() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    assert!(accept(&mut app, &id, json!({"mime_type": "image/png"})).is_empty());
    assert_eq!(
        app.notice(),
        Some("Could not open shot.png: the answer carried no file.")
    );
    assert!(app.take_image_out().view.is_empty());
}

#[test]
fn a_click_with_the_link_down_sends_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    started_with_image(&mut app, "artifacts/shot.png");
    assert_eq!(app.view_image(1), Effect::None);
}

#[test]
fn enter_on_the_focused_image_opens_it() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    app.focus = Some(TargetId::Line(Target::Image(1)));
    match app.focus_key(&Key::Enter) {
        Some(Effect::Send(lines)) => assert_eq!(lines.len(), 1),
        Some(
            Effect::None
            | Effect::Quit
            | Effect::Exit(_)
            | Effect::ListFiles
            | Effect::ReadImage(_)
            | Effect::FindPause { .. }
            | Effect::Search { .. }
            | Effect::Editor { .. }
            | Effect::OpenFile(_)
            | Effect::Copy(_)
            | Effect::OpenLink(_),
        )
        | None => panic!("expected a send"),
    }
}

#[test]
fn y_on_an_image_copies_its_line() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    app.focus = Some(TargetId::Line(Target::Image(1)));
    assert_eq!(
        app.focus_key(&Key::Char('y')),
        Some(Effect::Copy("▣ shot.png · 1280×800".to_owned()))
    );
}

#[test]
fn a_session_change_forgets_the_viewer_fetch() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    // Home, then another session: the answer arrives too late.
    app.go_home();
    app.attach(contract::SessionId(OTHER.to_owned()));
    assert!(
        accept(
            &mut app,
            &id,
            json!({"data": "eA==", "mime_type": "image/png"})
        )
        .is_empty()
    );
    assert!(app.take_image_out().view.is_empty());
    assert_eq!(app.notice(), None);
}

#[test]
fn a_stale_viewer_completion_is_dropped() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let generation = app.images.generation;
    app.attach(contract::SessionId(OTHER.to_owned()));
    app.image_viewed(1, "shot.png", generation, Err("boom".to_owned()));
    assert_eq!(app.notice(), None);
}

#[test]
fn a_failed_viewer_completion_refuses_the_image() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let generation = app.images.generation;
    app.image_viewed(1, "shot.png", generation, Err("boom".to_owned()));
    assert_eq!(app.notice(), Some("Could not open shot.png: boom."));
    assert_eq!(app.view_image(1), Effect::None);
}

#[test]
fn a_stale_viewer_failure_does_not_refuse() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let generation = app.images.generation;
    app.image_viewed(1, "shot.png", generation + 1, Err("boom".to_owned()));
    assert_eq!(app.notice(), None);
    assert!(matches!(app.view_image(1), Effect::Send(_)));
}

#[test]
fn disconnect_drops_the_fetch() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    app.disconnected();
    assert!(
        accept(
            &mut app,
            &id,
            json!({"data": "eA==", "mime_type": "image/png"})
        )
        .is_empty()
    );
    assert!(app.take_image_out().view.is_empty());
    // Only the dropped connection shows: the late answer changed nothing.
    assert_eq!(app.notice(), Some("Connection lost."));
}
