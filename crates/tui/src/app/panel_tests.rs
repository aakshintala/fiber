//! Tests for the panel's folded state: widgets and going home.

use super::super::App;
use crate::link::Line;
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

/// An app attached to `SESSION`.
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// One envelope of `session`.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An `extension_ui` widget's lines.
fn widget(extension: &str, widget: &str, lines: &[&str]) -> Line {
    session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": extension, "widget": widget, "lines": lines}),
    )
}

#[test]
fn the_latest_widget_lines_win_in_place() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    app.on_line(widget("other", "list", &["x"]));
    app.on_line(widget("plan", "tasks", &["one", "two"]));
    let widgets = app.panel_state().widgets();
    assert_eq!(widgets.len(), 2);
    assert_eq!(widgets[0].extension, "plan");
    assert_eq!(widgets[0].widget, "tasks");
    assert_eq!(widgets[0].lines, vec!["one".to_owned(), "two".to_owned()]);
    assert_eq!(widgets[1].extension, "other");
}

#[test]
fn empty_lines_remove_a_widget() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    app.on_line(widget("other", "list", &["x"]));
    app.on_line(widget("plan", "tasks", &[]));
    let widgets = app.panel_state().widgets();
    assert_eq!(widgets.len(), 1);
    assert_eq!(widgets[0].extension, "other");
}

#[test]
fn a_status_line_is_not_a_widget() {
    let mut app = attached();
    app.on_line(session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": "plan", "status": "working"}),
    ));
    assert!(app.panel_state().widgets().is_empty());
}

#[test]
fn going_home_clears_the_panel() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    assert!(!app.panel_state().widgets().is_empty());
    app.go_home();
    assert!(app.panel_state().widgets().is_empty());
}

#[test]
fn another_sessions_widget_is_not_folded() {
    let mut app = attached();
    app.on_line(session_line(
        OTHER,
        "extension_ui",
        serde_json::json!({"extension": "plan", "widget": "tasks", "lines": ["one"]}),
    ));
    assert!(app.panel_state().widgets().is_empty());
}
