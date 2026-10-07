//! Tests for home's state on the app: when home draws, and what `start`
//! names.

use super::{App, Effect};
use crate::home::{Launch, Spot};
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use serde_json::{Value, json};
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
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
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

/// The keys of the rows home draws, in order.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(key, _, _)| key).collect())
        .unwrap_or_default()
}

/// Opens the row with `key`, with the parsed lines going out.
fn open(app: &mut App, key: u64) -> Vec<Value> {
    match app.home_click(Spot::Entry(key)) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_) => panic!("opening sends"),
    }
}

/// A live `session_status` for `session`, named `name`, in `workspace` of
/// `project`, in `state`.
fn live(session: &str, name: &str, workspace: &str, project: &str, state: Value) -> Line {
    let mut payload = json!({
        "name": name,
        "workspace": workspace,
        "project": project,
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
        session_id: contract::SessionId(session.to_owned()),
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
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), Value::String(id.to_owned()))]
            .into_iter()
            .collect(),
    })
}

/// A session `command_rejected` for `id` from `session`, with `code` and
/// `message`.
fn session_refused(session: &str, id: &str, code: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_rejected` for `id`, with `code` and `message`.
fn hub_refused(id: &str, code: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// One exited `recent` row for `session`.
fn exited(session: &str, name: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": "exited",
    })
}

/// Answers `recent` with exited rows for `sessions`.
fn answer_recent(app: &mut App, recent: &str, sessions: &[(&str, &str)]) {
    let result = json!({"sessions": sessions
        .iter()
        .map(|(session, name)| exited(session, name))
        .collect::<Vec<_>>()});
    assert!(app.on_line(accepted(recent, result)).is_empty());
}

/// Links the app: the feed and recent ids, both waiting for their
/// answers.
fn linked(app: &mut App) -> (String, String) {
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    (
        lines[0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("feed id"))
            .to_owned(),
        lines[1]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("recent id"))
            .to_owned(),
    )
}

/// Types `text` without pressing Enter.
fn type_text(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
}

/// Types `text` and presses Enter.
fn enter_text(app: &mut App, text: &str) -> Effect {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_key(Key::Enter, now)
}

/// Opens the first row home draws, with the parsed lines going out.
fn open_first(app: &mut App) -> Vec<Value> {
    let key = keys(app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    open(app, key)
}

/// Starts a session from the draft and accepts the start for `session`:
/// the app attached, its `full` subscribe in flight.
fn start_session(app: &mut App, session: &str) {
    let Effect::Send(lines) = enter_text(app, "hi") else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
    let start: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    let out: Vec<Value> = app
        .on_line(accepted(
            start["id"].as_str().unwrap_or_else(|| panic!("start id")),
            json!({"session_id": session}),
        ))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{err}")))
        .collect();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["command"], "subscribe");
}

#[test]
fn enter_on_a_focused_live_row_subscribes_full_asks_commands_and_attaches() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    // Shift+Tab focuses the last stop: the second row.
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::BackTab, now);
    let row = keys(&app)[1];
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter opens the row");
    };
    let lines: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_bbbbbbbbbbbbbbbb");
    assert_eq!(lines[0]["args"]["level"], "full");
    assert_eq!(lines[1]["command"], "commands");
    assert_eq!(lines[1]["session_id"], "s_bbbbbbbbbbbbbbbb");
    assert_ne!(lines[0]["id"], lines[1]["id"]);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_bbbbbbbbbbbbbbbb")
    );
    assert!(!app.on_home());
}

#[test]
fn opening_an_exited_row_sends_the_same_and_the_hub_resumes() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let lines = open_first(&mut app);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[0]["args"]["level"], "full");
    assert_eq!(lines[1]["command"], "commands");
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
}

#[test]
fn a_started_session_is_recorded_full_and_leaving_lowers_it() {
    let mut app = home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let Effect::Send(lines) = enter_text(&mut app, "/home") else {
        panic!("/home leaves");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "subscribe");
    assert_eq!(line["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(line["args"]["level"], "summary");
    assert!(app.on_home());
}

#[test]
fn opening_a_summary_session_sends_one_full_subscribe() {
    let mut app = home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    assert!(matches!(enter_text(&mut app, "/home"), Effect::Send(_)));
    let lines = open_first(&mut app);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["args"]["level"], "full");
    assert_eq!(lines[1]["command"], "commands");
}

#[test]
fn opening_a_full_session_sends_summary_then_full() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(first[0]["args"]["level"], "full");
    // Home during the open, before any answer: the row is not live, so no
    // lowering goes out.
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    let lines = open_first(&mut app);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["args"]["level"], "summary");
    assert_eq!(lines[1]["args"]["level"], "full");
    assert_eq!(lines[2]["command"], "commands");
    assert_ne!(lines[0]["id"], lines[1]["id"]);
}

#[test]
fn a_click_on_a_row_opens_it() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let key = keys(&app)[0];
    match app.on_click(crate::mouse::TargetId::Home(Spot::Entry(key))) {
        Effect::Send(lines) => assert_eq!(lines.len(), 2),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_) => panic!("a click opens the row"),
    }
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
}

#[test]
fn a_click_on_a_key_with_no_row_does_nothing() {
    let mut app = home();
    linked(&mut app);
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Entry(9999))),
        Effect::None
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn opening_requires_home() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let key = keys(&app).into_iter().next().unwrap_or(9999);
    assert_eq!(app.home_click(Spot::Entry(key)), Effect::None);
    assert!(app.home_screen().is_none());
}

#[test]
fn opening_needs_the_link_up() {
    let mut app = home();
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let key = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(app.home_click(Spot::Entry(key)), Effect::None);
    assert!(app.session().is_none());
}

#[test]
fn opening_while_a_start_waits_does_nothing() {
    let mut app = home();
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_key(Key::Enter, now);
    let key = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(app.home_click(Spot::Entry(key)), Effect::None);
    assert!(app.session().is_none());
}

#[test]
fn an_unreadable_row_does_not_open_and_says_so() {
    let mut app = home();
    linked(&mut app);
    let mut newer = live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    );
    if let Line::Session(envelope) = &mut newer {
        envelope.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(newer);
    let key = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(app.home_click(Spot::Entry(key)), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(
        app.notice(),
        Some("Cannot attach: this session's schema is newer than this terminal reads.")
    );
}

#[test]
fn lines_before_the_acknowledgement_fold_the_row_and_skip_the_conversation() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    open_first(&mut app);
    assert!(
        app.on_line(live(
            "s_aaaaaaaaaaaaaaaa",
            "tidy docs",
            "/w",
            "-w",
            json!({"state": "streaming"}),
        ))
        .is_empty()
    );
    assert!(app.on_line(turn_started("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(app.on_line(review()).is_empty());
    assert_eq!(listed(&app), ["●  tidy docs"]);
    assert!(app.lines().is_empty());
    assert!(app.badge().is_none());
    assert!(app.panel().is_none());
}

#[test]
fn the_acknowledgement_and_lines_after_it_reach_the_conversation() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack))
            .is_empty()
    );
    assert!(app.on_line(turn_started("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
}

#[test]
fn an_approval_before_the_acknowledgement_is_not_queued() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    open_first(&mut app);
    assert!(app.on_line(review()).is_empty());
    assert!(app.badge().is_none());
    assert!(app.panel().is_none());
}

#[test]
fn a_refused_open_goes_home_with_a_note_and_a_notice() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(hub_refused(ack, "session_held", "held by another process"))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(listed(&app), ["●  fix the parser  held by another process"]);
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_session_rejection_of_the_full_subscribe_does_the_same() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            ack,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert_eq!(listed(&app), ["●  fix the parser  held by another process"]);
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_refused_open_after_going_home_keeps_the_draft() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    assert!(matches!(enter_text(&mut app, "/home"), Effect::Send(_)));
    type_text(&mut app, "keep me");
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &ack,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
    assert_eq!(app.input().expand(), "keep me");
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_stale_refusal_for_an_earlier_open_is_ignored() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let first = open_first(&mut app);
    let stale = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"))
        .to_owned();
    assert!(matches!(enter_text(&mut app, "/home"), Effect::Send(_)));
    let second = open_first(&mut app);
    assert_eq!(second[0]["args"]["level"], "full");
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &stale,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    let ack = second[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack))
            .is_empty()
    );
    assert!(app.on_line(turn_started("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
}

#[test]
fn a_refused_raise_leaves_the_level_and_the_next_open_sends_full_again() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    let ack = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &ack,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    let second = open_first(&mut app);
    assert_eq!(second.len(), 2);
    assert_eq!(second[0]["args"]["level"], "full");
    assert_eq!(second[1]["command"], "commands");
}

#[test]
fn a_rejected_summary_step_alone_does_not_fail_the_open() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    open_first(&mut app);
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    let lines = open_first(&mut app);
    assert_eq!(lines.len(), 3);
    let summary = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"))
        .to_owned();
    let ack = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &summary,
            "invalid_arguments",
            "already at summary"
        ))
        .is_empty()
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &ack))
            .is_empty()
    );
    assert!(app.on_line(turn_started("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
}

#[test]
fn a_two_step_open_never_retries() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    open_first(&mut app);
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    let lines = open_first(&mut app);
    let ack = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &ack,
            "invalid_arguments",
            "already at full"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(app.notice(), Some("already at full"));
}

#[test]
fn a_same_level_refusal_after_a_hub_replay_retries_once_with_summary_then_full() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    let full = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("full id"))
        .to_owned();
    let out = app.on_line(session_refused(
        "s_aaaaaaaaaaaaaaaa",
        &full,
        "invalid_arguments",
        "the log cannot be read",
    ));
    assert_eq!(out.len(), 2);
    let retry: Vec<Value> = out
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    assert_eq!(retry[0]["args"]["level"], "summary");
    assert_eq!(retry[1]["args"]["level"], "full");
    assert_ne!(retry[0]["id"], retry[1]["id"]);
    assert_ne!(retry[0]["id"], full);
    let summary = retry[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"))
        .to_owned();
    let raised = retry[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("raised id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &summary))
            .is_empty()
    );
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &raised,
            "invalid_arguments",
            "the log cannot be read"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert_eq!(app.notice(), Some("the log cannot be read"));
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    let second = open_first(&mut app);
    assert_eq!(second.len(), 2);
    assert_eq!(second[0]["args"]["level"], "full");
    let again = second[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("again id"))
        .to_owned();
    let out = app.on_line(session_refused(
        "s_aaaaaaaaaaaaaaaa",
        &again,
        "invalid_arguments",
        "already at full",
    ));
    assert_eq!(out.len(), 2);
    let retry: Vec<Value> = out
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    assert_eq!(retry[0]["args"]["level"], "summary");
    assert_eq!(retry[1]["args"]["level"], "full");
    assert_ne!(retry[0]["id"], retry[1]["id"]);
    assert_ne!(retry[0]["id"], again);
    let summary = retry[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"))
        .to_owned();
    let raised = retry[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("raised id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &summary))
            .is_empty()
    );
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &raised))
            .is_empty()
    );
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "old work",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    assert!(app.on_line(turn_started("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
    // Held at full, leaving lowers it again.
    let Effect::Send(lines) = enter_text(&mut app, "/home") else {
        panic!("/home lowers");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["args"]["level"], "summary");
}

#[test]
fn a_second_same_level_refusal_fails_the_open() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    let full = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("full id"))
        .to_owned();
    let out = app.on_line(session_refused(
        "s_aaaaaaaaaaaaaaaa",
        &full,
        "invalid_arguments",
        "already at full",
    ));
    assert_eq!(out.len(), 2);
    let retry: Vec<Value> = out
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    let summary = retry[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"))
        .to_owned();
    let raised = retry[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("raised id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &summary))
            .is_empty()
    );
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &raised,
            "invalid_arguments",
            "already at full"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(listed(&app), ["○  old work  already at full"]);
    assert_eq!(app.notice(), Some("already at full"));
}

#[test]
fn leaving_a_session_that_left_sends_no_subscribe() {
    let mut app = home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    assert!(app.on_home());
}

#[test]
fn leaving_with_the_link_down_sends_nothing() {
    let mut app = home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.disconnected();
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    assert!(app.on_home());
}

#[test]
fn slash_close_sends_close_now_and_no_subscribe() {
    let mut app = home();
    linked(&mut app);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let Effect::Send(lines) = enter_text(&mut app, "/close") else {
        panic!("/close sends");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "close");
    assert_eq!(line["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(line["args"], json!({"now": true}));
    assert!(app.on_home());
}

#[test]
fn home_without_home_state_sends_nothing_on_leave() {
    let mut app = App::new(PathBuf::from("/w"));
    assert_eq!(app.leave(), Effect::None);
}

#[test]
fn home_during_an_open_of_a_resuming_session_lowers_it_once_its_status_is_live() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(first[0]["args"]["level"], "full");
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    let ack = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &ack))
            .is_empty()
    );
    let out = app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "old work",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    assert_eq!(out.len(), 1);
    let line: Value = serde_json::from_str(&out[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "subscribe");
    assert_eq!(line["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(line["args"]["level"], "summary");
    assert!(app.session().is_none());
}

#[test]
fn an_accepted_full_for_the_attached_session_sends_nothing() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack))
            .is_empty()
    );
}

#[test]
fn no_summary_while_a_subscribe_is_in_flight() {
    let mut app = home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let Effect::Send(lowered) = enter_text(&mut app, "/home") else {
        panic!("/home lowers");
    };
    assert_eq!(lowered.len(), 1);
    assert!(
        app.on_line(live(
            "s_aaaaaaaaaaaaaaaa",
            "fix the parser",
            "/w",
            "-w",
            json!({"state": "streaming"}),
        ))
        .is_empty()
    );
}

#[test]
fn no_summary_for_a_left_row() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    let ack = first[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack))
            .is_empty()
    );
    assert!(app.session().is_none());
}

#[test]
fn the_file_listing_follows_an_opened_row_outside_the_launch_workspace() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "lens work",
        "/other",
        "-other",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    assert_eq!(app.workspace(), PathBuf::from("/other"));
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack))
            .is_empty()
    );
    assert_eq!(app.workspace(), PathBuf::from("/other"));
}

#[test]
fn without_home_the_workspace_is_the_launch_directory() {
    let app = App::new(PathBuf::from("/w"));
    assert_eq!(app.workspace(), PathBuf::from("/w"));
}

/// Renders home at 80x24 and takes its targets, so keys move among drawn
/// stops.
fn drawn(app: &mut App) {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
}

/// Two live rows, the first streaming and the second idle.
fn two_rows(app: &mut App) {
    linked(app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
}

#[test]
fn down_in_an_empty_box_focuses_the_first_row() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    // Esc returns focus to the input box, and j focuses the first row
    // again.
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
}

#[test]
fn down_with_a_draft_moves_in_the_draft() {
    let mut app = home();
    two_rows(&mut app);
    type_text(&mut app, "x");
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.input().expand(), "x");
}

#[test]
fn down_with_the_search_panel_open_moves_its_selection() {
    let mut app = home();
    two_rows(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let _ = app.on_key(Key::CtrlR, now);
    assert!(app.completions().is_some());
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert!(app.completions().is_some());
}

#[test]
fn down_with_no_rows_does_nothing() {
    let mut app = home();
    linked(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn down_on_the_conversation_screen_is_unchanged() {
    let mut app = home();
    linked(&mut app);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn down_twice_moves_to_the_second_row() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
}

#[test]
fn down_on_the_last_drawn_row_focuses_the_next_and_scrolls() {
    let mut app = home();
    linked(&mut app);
    for n in 0..15u8 {
        let session = format!("s_{n:016x}");
        app.on_line(live(
            &session,
            "fix the parser",
            "/w",
            "-w",
            json!({"state": "idle"}),
        ));
    }
    assert_eq!(keys(&app).len(), 15);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    // Nine rows fit under the four-row logo's box: nine steps reach
    // the last drawn one, and the next step moves below the fold.
    for _ in 0..9 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    let shown = keys(&app);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(shown[8])))
    );
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(shown[9])))
    );
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let drawn: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(drawn.last(), Some(&shown[9]));
    assert!(!drawn.contains(&shown[0]));
}

#[test]
fn down_on_the_last_row_of_the_list_does_nothing() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
}

#[test]
fn slash_resume_from_a_session_focuses_the_list_after_the_home_frame() {
    let mut app = home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    // The conversation frame first, as the loop draws it.
    drawn(&mut app);
    assert!(matches!(enter_text(&mut app, "/resume"), Effect::Send(_)));
    assert!(app.on_home());
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let row = keys(&app)[0];
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter opens the focused row");
    };
    let lines: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
}

#[test]
fn slash_resume_with_no_rows_leaves_the_box_focused() {
    let mut app = home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    drawn(&mut app);
    assert_eq!(enter_text(&mut app, "/resume"), Effect::None);
    assert!(app.on_home());
    drawn(&mut app);
    assert_eq!(app.focused(), None);
}

#[test]
fn y_on_a_focused_row_copies_its_line() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.on_key(Key::Char('y'), now),
        Effect::Copy("✓  fix the parser".to_owned())
    );
}

/// The chips home draws, as text left to right.
fn chips(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.chips.into_iter().map(|(_, text)| text).collect())
        .unwrap_or_default()
}

/// The workspace picker home draws: its list and the selected index.
fn picker(app: &App) -> Option<(Vec<String>, usize)> {
    app.home_screen().and_then(|screen| screen.picker)
}

/// Three live rows: one in the launch directory, two elsewhere, one
/// workspace twice.
fn three_workspaces(app: &mut App) {
    linked(app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "here",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "away",
        "/other",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_cccccccccccccccc",
        "again",
        "/other",
        "-w",
        json!({"state": "idle"}),
    ));
    app.on_line(live(
        "s_dddddddddddddddd",
        "third",
        "/third",
        "-w",
        json!({"state": "idle"}),
    ));
}

#[test]
fn the_chip_defaults_to_the_launch_directory() {
    let app = home();
    assert_eq!(
        chips(&app),
        [
            "[w]",
            "[no model]",
            "[thinking: default]",
            "enter starts a session",
        ]
    );
}

#[test]
fn the_picker_lists_the_launch_directory_then_row_workspaces_once_each() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(
        picker(&app),
        Some((
            vec!["/w".to_owned(), "/other".to_owned(), "/third".to_owned(),],
            0,
        ))
    );
}

#[test]
fn the_picker_holds_at_most_ten() {
    let mut app = home();
    linked(&mut app);
    for n in 0..12u8 {
        let session = format!("s_{n:016x}");
        let workspace = format!("/w{n}");
        app.on_line(live(
            &session,
            "fix the parser",
            &workspace,
            "-w",
            json!({"state": "idle"}),
        ));
    }
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    let (list, selected) = picker(&app).unwrap_or_else(|| panic!("the picker"));
    assert_eq!(selected, 0);
    assert_eq!(list.len(), 10);
    assert_eq!(list[0], "/w");
    assert_eq!(list[9], "/w8");
}

#[test]
fn picker_keys_move_choose_and_close() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(1));
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(2));
    // The selection clamps to the list.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(2));
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(1));
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(0));
    // Enter chooses the selected workspace for the next start.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(picker(&app), None);
    assert_eq!(chips(&app)[0], "[other]");
    // Esc closes the picker, keeping the workspace as it was.
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert!(picker(&app).is_some());
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(picker(&app), None);
    assert_eq!(chips(&app)[0], "[other]");
}

#[test]
fn other_keys_do_nothing_in_the_picker() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Char('x'), now), Effect::None);
    assert!(app.input().expand().is_empty());
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(0));
    assert_eq!(app.focused(), None);
}

#[test]
fn ctrl_c_passes_through_the_picker() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(app.home_key(&Key::CtrlC), None);
    assert!(picker(&app).is_some());
}

#[test]
fn start_names_the_chosen_workspace() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    assert_eq!(line["command"], "start");
    assert_eq!(line["args"]["workspace"], "/other");
}

#[test]
fn a_click_on_a_picker_row_chooses_it() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(app.home_click(Spot::Pick(2)), Effect::None);
    assert_eq!(picker(&app), None);
    assert_eq!(chips(&app)[0], "[third]");
}

#[test]
fn a_click_past_the_picker_list_does_nothing() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(app.home_click(Spot::Pick(99)), Effect::None);
    assert!(picker(&app).is_some());
    assert_eq!(chips(&app)[0], "[w]");
}

#[test]
fn the_file_listing_follows_the_picked_workspace() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.workspace(), PathBuf::from("/w"));
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(app.home_click(Spot::Pick(1)), Effect::None);
    assert_eq!(app.workspace(), PathBuf::from("/other"));
}
