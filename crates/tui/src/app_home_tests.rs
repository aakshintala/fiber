//! Tests for home's state on the app: when home draws, and what `start`
//! names.

use super::App;
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use serde_json::json;
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

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: serde_json::Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_rejected` for `id` with `message`.
fn refused(id: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            (
                "code".to_owned(),
                serde_json::Value::String("invalid_arguments".to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String(message.to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    })
}

/// A `session_status` for `session`, idle and named.
fn status(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// A `turn_started` for `session` with one message.
fn turn_started(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"input": [{"type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Parses outgoing command lines.
fn commands(lines: Vec<String>) -> Vec<serde_json::Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The rows home draws, as lines.
fn rows(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(_, line, _)| line).collect())
        .unwrap_or_default()
}

/// Every listed row's line, drawn or not: what the feed and `recent` folded.
fn listed(app: &App) -> Vec<String> {
    app.home
        .as_ref()
        .map(|home| {
            home.sessions
                .shown(&home.launch.project, false)
                .iter()
                .map(|row| crate::home::line(row, &home.launch.project))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn hello_sends_feed_then_recent_once() {
    let mut app = home();
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["command"], "feed");
    assert_eq!(lines[1]["command"], "recent");
    assert!(lines[1].get("args").is_none());
    assert_ne!(lines[0]["id"], lines[1]["id"]);
}

#[test]
fn no_feed_before_hello_and_none_on_a_refused_schema() {
    let mut app = home();
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    let mut newer = hello();
    if let Line::Hub(hello) = &mut newer {
        hello.schema_version = contract::SCHEMA_VERSION + 1;
    }
    assert!(app.on_line(newer).is_empty());
    assert_eq!(rows(&app), ["✓  fix the parser"]);
}

#[test]
fn a_line_after_hello_sends_nothing_more() {
    let mut app = home();
    app.on_line(hello());
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert_eq!(rows(&app), ["✓  fix the parser"]);
}

#[test]
fn a_recent_answer_for_an_older_id_changes_nothing() {
    let mut app = home();
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    // An answer for an id that is not the latest `recent`: nothing folds.
    assert!(
        app.on_line(accepted("c_deadbeefdeadbeef", json!({"sessions": []})))
            .is_empty()
    );
    assert!(rows(&app).is_empty());
    app.on_line(accepted(
        recent,
        json!({"sessions": [{
            "session_id": "s_aaaaaaaaaaaaaaaa",
            "ts": 0,
            "project": "-w",
            "workspace": "/w",
            "name": "old work",
            "how": "exited",
        }]}),
    ));
    assert_eq!(rows(&app), ["○  old work"]);
}

#[test]
fn a_rejected_feed_is_one_notice() {
    let mut app = home();
    let lines = commands(app.on_line(hello()));
    let feed = lines[0]["id"].as_str().unwrap_or_else(|| panic!("feed id"));
    assert!(app.on_line(refused(feed, "no feed")).is_empty());
    assert_eq!(app.notice(), Some("no feed"));
    assert!(rows(&app).is_empty());
}

#[test]
fn session_status_for_the_attached_session_still_reaches_the_conversation() {
    let mut app = home();
    app.on_line(hello());
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    let start = commands(lines);
    let id = start[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("start id"));
    app.on_line(accepted(id, json!({"session_id": "s_aaaaaaaaaaaaaaaa"})));
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    // The status folds into the rows, and passes through to the
    // conversation below.
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert_eq!(listed(&app), ["✓  fix the parser"]);
    assert!(app.on_line(turn_started("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
}

#[test]
fn an_attention_line_changes_nothing() {
    let mut app = home();
    app.on_line(hello());
    let attention = Line::Hub(contract::HubLine {
        kind: "attention".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": "s_aaaaaaaaaaaaaaaa"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    assert!(app.on_line(attention).is_empty());
    assert!(rows(&app).is_empty());
    assert_eq!(app.notice(), None);
}

/// A hub `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

#[test]
fn answers_and_a_left_fill_and_mark_the_list() {
    let mut app = home();
    let lines = commands(app.on_line(hello()));
    let feed = lines[0]["id"].as_str().unwrap_or_else(|| panic!("feed id"));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    // The feed's own answer carries no rows.
    assert!(app.on_line(accepted(feed, json!({}))).is_empty());
    assert!(rows(&app).is_empty());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    assert_eq!(rows(&app), ["✓  fix the parser"]);
    // `session_left` marks the row in place; one for an unknown session
    // changes nothing.
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    assert!(
        app.on_line(left("s_bbbbbbbbbbbbbbbb", "crashed"))
            .is_empty()
    );
    assert_eq!(rows(&app), ["○  fix the parser"]);
    // The recent page fills the exited rows after the feed rows.
    app.on_line(accepted(
        recent,
        json!({"sessions": [{
            "session_id": "s_bbbbbbbbbbbbbbbb",
            "ts": 0,
            "project": "-w",
            "workspace": "/w",
            "name": "old work",
            "how": "crashed",
        }]}),
    ));
    assert_eq!(rows(&app), ["○  fix the parser", "✗  old work"]);
}

#[test]
fn a_left_with_an_unknown_how_changes_nothing() {
    let mut app = home();
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    let left = Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({
            "session_id": "s_aaaaaaaaaaaaaaaa", "how": "vanished"})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    });
    assert!(app.on_line(left).is_empty());
    assert_eq!(rows(&app), ["✓  fix the parser"]);
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
