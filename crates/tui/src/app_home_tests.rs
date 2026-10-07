//! Tests for home's state on the app: when home draws, and what `start`
//! names.

use super::App;
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use std::path::PathBuf;

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
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

/// A `permission_requested` review, which opens the approval panel.
fn review() -> Line {
    Line::Session(contract::Envelope {
        kind: "permission_requested".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({
            "request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"},
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn an_app_without_home_draws_the_conversation_screen() {
    let app = App::new(PathBuf::from("/w"));
    assert!(!app.on_home());
    assert!(app.home_screen().is_none());
}

#[test]
fn an_attached_app_draws_the_conversation_screen() {
    let mut app = home();
    assert!(app.on_home());
    assert!(app.home_screen().is_some());
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert!(!app.on_home());
    assert!(app.home_screen().is_none());
}

#[test]
fn the_key_map_draws_over_home() {
    let mut app = home();
    app.open_keymap();
    assert!(!app.on_home());
    assert!(app.home_screen().is_none());
}

#[test]
fn an_approval_panel_draws_over_home() {
    let mut app = home();
    app.on_line(review());
    assert!(app.panel().is_some());
    assert!(!app.on_home());
    assert!(app.home_screen().is_none());
}

#[test]
fn start_names_the_launch_workspace() {
    let mut app = home();
    app.on_line(hello());
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
    let line: serde_json::Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    assert_eq!(line["command"], "start");
    assert_eq!(line["args"]["workspace"], "/w");
    assert_eq!(line["args"]["content"][0]["text"], "hi");
    // The placeholder goes with the first prompt.
    assert!(app.home_screen().is_some_and(|screen| !screen.placeholder));
}
