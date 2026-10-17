//! Tests for home's state on the app: when home draws, and what `start`
//! names.

use super::{App, Effect};
use crate::home::{Launch, Spot};
use crate::keys::Key;
use crate::link::Line;
use crate::local_time::{new_york, turn_started_at};
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
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
fn remote_home_ignores_the_launch_project_in_sections_and_row_text() {
    let mut app = home();
    app.set_home(Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
        ..Default::default()
    });
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "first",
        "/a",
        "other",
        json!({"state":"idle"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "second",
        "/b",
        "sentinel-project",
        json!({"state":"idle"}),
    ));
    let screen = app.home_screen().unwrap();
    let heads: Vec<_> = screen
        .list
        .iter()
        .filter_map(|line| {
            if let crate::home::ListLine::Head { name, .. } = line {
                Some(name.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(heads, ["a", "b"]);
    let row = app.home.as_ref().unwrap().sessions.all()[1].key;
    let text = app.home_text(Spot::Entry(row)).unwrap();
    assert!(text.ends_with("  b"), "{text}");
    assert!(!text.contains("sentinel-project"));
}

#[test]
fn remote_project_of_uses_newest_status_then_first_row_on_ties() {
    for (first_since, second_since, expected) in [(1, 2, "q"), (2, 1, "p"), (2, 2, "p")] {
        let mut app = home();
        for (id, project, since) in [
            ("s_aaaaaaaaaaaaaaaa", "p", first_since),
            ("s_bbbbbbbbbbbbbbbb", "q", second_since),
        ] {
            let mut row = live(id, project, "/remote", project, json!({"state":"idle"}));
            if let Line::Session(envelope) = &mut row {
                envelope.payload.insert("since".to_owned(), json!(since));
            }
            app.on_line(row);
        }
        let sessions = &app.home.as_ref().unwrap().sessions;
        assert_eq!(sessions.project_of("/remote"), Some(expected));
        assert_eq!(sessions.project_of("/elsewhere"), None);
    }
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions":[exited_in("s_aaaaaaaaaaaaaaaa", "old", "/remote", "p")]}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "new",
        "/remote",
        "q",
        json!({"state":"idle"}),
    ));
    assert_eq!(
        app.home.as_ref().unwrap().sessions.project_of("/remote"),
        Some("q")
    );
    for absent_first in [false, true] {
        let mut sessions = crate::home::Sessions::default();
        let make = |id: &str, project: &str| {
            let Line::Session(envelope) =
                live(id, project, "/remote", project, json!({"state":"idle"}))
            else {
                unreachable!()
            };
            crate::home::from_status(&envelope)
        };
        let mut absent = make("s_aaaaaaaaaaaaaaaa", "absent");
        absent.status = None;
        let present = make("s_bbbbbbbbbbbbbbbb", "present");
        if absent_first {
            sessions.status(absent);
            sessions.status(present);
        } else {
            sessions.status(present);
            sessions.status(absent);
        }
        assert_eq!(sessions.project_of("/remote"), Some("present"));
    }
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
fn the_zone_survives_a_session_switch() {
    let mut app = home();
    app.set_zone(new_york());
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.go_home();
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    // 2026-10-08T14:15Z, 10:15 in New York.
    app.on_line(turn_started_at("s_bbbbbbbbbbbbbbbb", "go", 1791468900000));
    let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert_eq!(
        texts,
        vec![
            "".to_owned(),
            "▄▄▄▄▄".to_owned(),
            " go ▐".to_owned(),
            "▀▀▀▀▀".to_owned(),
            "you 10:15 ".to_owned(),
            "".to_owned()
        ]
    );
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
            "started": 0, "calls": 0, "since": 0,
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
        .map(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(_, line)| line)
                .collect()
        })
        .unwrap_or_default()
}

/// Every listed row's line, drawn or not: what the feed and `recent` folded.
fn listed(app: &App) -> Vec<String> {
    app.home
        .as_ref()
        .map(|home| {
            home.sessions
                .all()
                .iter()
                .map(|row| crate::home::line(row, home.launch_project()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn home_screen_header_counts_follow_the_state_order() {
    let mut app = home();
    app.on_line(live("s_wait", "waiting", "/w", "-w", json!({"state":"waiting", "waiting":{"request_id":"r_1", "kind":"approval", "summary":"approve"}})));
    app.on_line(live(
        "s_work",
        "working",
        "/other",
        "-other",
        json!({"state":"streaming"}),
    ));
    app.on_line(live(
        "s_retry",
        "retrying",
        "/w",
        "-w",
        json!({"state":"retrying"}),
    ));
    app.on_line(live(
        "s_crash",
        "crashed",
        "/w",
        "-w",
        json!({"state":"idle"}),
    ));
    app.on_line(left("s_crash", "crashed"));
    app.on_line(live(
        "s_ready",
        "ready",
        "/w",
        "-w",
        json!({"state":"idle"}),
    ));
    let screen = app.home_screen().unwrap();
    assert_eq!(
        screen
            .header
            .counts
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        [
            "1 waiting on you",
            "1 working",
            "1 retrying",
            "1 crashed",
            "1 ready"
        ]
    );
    assert_eq!(screen.header.spend.as_deref(), Some("$0.00 live"));
}

#[test]
fn home_screen_header_says_no_sessions_yet_without_live_rows() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_old", "exited")]);
    let screen = app.home_screen().unwrap();
    assert!(screen.header.counts.is_empty());
    assert_eq!(screen.header.spend, None);
    assert!(!screen.empty);
}

#[test]
fn section_header_counts_live_spend_and_waiting() {
    let mut app = home();
    app.on_line(live("s_wait", "waiting", "/w", "-w", json!({"state":"waiting", "waiting":{"request_id":"r_1", "kind":"approval", "summary":"approve"}, "spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":1.25,"subscription_cost":0.0}})));
    app.on_line(live("s_other", "other", "/w", "-w", json!({"state":"streaming", "spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":0.10,"subscription_cost":0.0}})));
    let screen = app.home_screen().unwrap();
    let crate::home::ListLine::Head {
        live,
        spend,
        waiting,
        ..
    } = &screen.list[0]
    else {
        panic!("project head")
    };
    assert_eq!((*live, spend.as_str(), *waiting), (2, "$1.35", 1));
}

#[test]
fn the_header_spend_is_the_sum_of_the_live_rows() {
    let mut app = home();
    app.on_line(live("s_a", "a", "/w", "-w", json!({"state":"streaming", "spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":1.25,"subscription_cost":0.0}})));
    app.on_line(live("s_b", "b", "/other", "-other", json!({"state":"idle", "spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":2.10,"subscription_cost":0.0}})));
    assert_eq!(
        app.home_screen().unwrap().header.spend.as_deref(),
        Some("$3.35 live")
    );
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
    assert_eq!(rows(&app), ["○  fix the parser", "○  old work"]);
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
    assert!(line["args"].get("content").is_none());
    // The placeholder goes with the first prompt.
    assert!(app.home_screen().is_some_and(|screen| !screen.placeholder));
}

/// The keys of the rows home draws, in order.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(key, _)| key)
                .collect()
        })
        .unwrap_or_default()
}

/// The first hidden exited row's key for `project`.
fn more_key(app: &App, project: &str) -> Option<u64> {
    let home = app.home.as_ref()?;
    home.sessions
        .sections(home.launch_project())
        .into_iter()
        .find(|section| section.project == project)
        .and_then(|section| section.exited.get(section.shown_exited).map(|row| row.key))
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
        | Effect::ReadImage(_)
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("opening sends"),
    }
}

/// A live `session_status` for `session`, named `name`, in `workspace` of
/// `project`, in `state`.
fn live(session: &str, name: &str, workspace: &str, project: &str, state: Value) -> Line {
    let mut payload = json!({
        "name": name,
        "workspace": workspace,
        "project": project,
        "started": 0, "calls": 0, "since": 0,
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

/// One exited recent row in `workspace` and `project`.
fn exited_in(session: &str, name: &str, workspace: &str, project: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": project,
        "workspace": workspace,
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
/// answers. An open picker's listing goes out beside them.
fn linked(app: &mut App) -> (String, String) {
    let lines: Vec<Value> = commands(app.on_line(hello()))
        .into_iter()
        .filter(|line| line["command"] != "directories")
        .collect();
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
    assert_eq!(out.len(), 3);
    assert_eq!(out[0]["command"], "subscribe");
    assert_eq!(out[2]["command"], "prompt");
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
    // Focus the second row directly: keyboard traversal now ends at the
    // chip row, while this test checks Enter on a focused live row.
    let row = keys(&app)[1];
    app.focus = Some(crate::mouse::TargetId::Home(Spot::Entry(row)));
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
    let now = fakes::clock::FakeClock::new().now();
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
        | Effect::ReadImage(_)
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("a click opens the row"),
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
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, text, .. }
        if code == "session_held"
            && text == "Cannot attach: this session's schema is newer than this terminal reads."
    )));
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
fn a_refused_two_step_open_fails_the_open() {
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
fn a_refused_one_step_open_fails_without_a_retry() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(first.len(), 2);
    let full = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("full id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &full,
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
fn after_a_refused_open_the_next_open_sends_one_full_and_attaches() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    let full = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("full id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &full,
            "invalid_arguments",
            "the log cannot be read"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    let second = open_first(&mut app);
    assert_eq!(second.len(), 2);
    assert_eq!(second[0]["command"], "subscribe");
    assert_eq!(second[0]["args"]["level"], "full");
    assert_eq!(second[1]["command"], "commands");
    let again = second[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("again id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &again))
            .is_empty()
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    assert!(app.on_line(turn_started("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "old work",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    // Held at full, leaving lowers it.
    let Effect::Send(lines) = enter_text(&mut app, "/home") else {
        panic!("/home lowers");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "subscribe");
    assert_eq!(line["args"]["level"], "summary");
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
fn home_arrows_focus_the_list_and_entry_bar() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
}

#[test]
fn typing_into_the_empty_draft_inserts_text_with_rows_present() {
    // Printable keys reach the draft while the box has focus: only ↓
    // enters the list, and j and k move only while it has focus.
    for key in ['j', 'k', 'c', 'x'] {
        let mut app = home();
        two_rows(&mut app);
        drawn(&mut app);
        let now = fakes::clock::FakeClock::new().now();
        assert_eq!(app.on_key(Key::Char(key), now), Effect::None);
        assert_eq!(app.focused(), None, "{key} keeps the box focused");
        assert_eq!(app.input().expand(), key.to_string());
    }
}

#[test]
fn down_with_a_draft_moves_in_the_draft() {
    // A one-line draft keeps its text while ↓ focuses the chip row.
    let mut app = home();
    two_rows(&mut app);
    type_text(&mut app, "x");
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert_eq!(app.input().expand(), "x");
    // A two-line draft's ↓ moves the cursor first.
    let mut app = home();
    two_rows(&mut app);
    type_text(&mut app, "a");
    assert_eq!(app.on_edit(crate::keys::Edit::ShiftEnter), Effect::None);
    type_text(&mut app, "b");
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.input().expand(), "a\nb");
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
    // ↓ focuses the chip row, and a second ↓ changes nothing.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
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
fn up_and_j_walk_the_row_stops() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    // Every click target is a focus stop: the row's ✕ comes next.
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Stop(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
}

#[test]
fn j_does_not_move_from_a_row_before_the_last_drawn_stop() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let rows = [
        exited_in("s_aaaaaaaaaaaaaaaa", "alpha", "/work/alpha", "alpha"),
        exited_in("s_bbbbbbbbbbbbbbbb", "beta", "/work/beta", "beta"),
        exited_in("s_cccccccccccccccc", "gamma", "/work/gamma", "gamma"),
    ];
    assert!(
        app.on_line(accepted(&recent, json!({"sessions": rows})))
            .is_empty()
    );
    let (_, targets) = drawn_frame(&app);
    let entries: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(entries.len(), 3, "all three exited rows draw");
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(entries[2])))
    );
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(entries[1])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Start(entries[2])))
    );
}

#[test]
fn j_from_the_last_list_row_clears_focus_without_skipping() {
    let mut app = home();
    linked(&mut app);
    for (session, name, workspace, project) in [
        ("s_aaaaaaaaaaaaaaaa", "alpha", "/work/alpha", "alpha"),
        ("s_bbbbbbbbbbbbbbbb", "beta", "/work/beta", "beta"),
        ("s_cccccccccccccccc", "gamma", "/work/gamma", "gamma"),
    ] {
        app.on_line(live(
            session,
            name,
            workspace,
            project,
            json!({"state": "idle"}),
        ));
        app.on_line(left(session, "crashed"));
    }
    let (_, targets) = drawn_frame(&app);
    let mut entries: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    entries.dedup();
    assert_eq!(entries.len(), 3, "all three crashed rows draw");
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(entries[2])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn up_focuses_the_last_loaded_row_and_scrolls() {
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
    let all = keys(&app);
    assert_eq!(all.len(), 15);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(all[14])))
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
    assert!(drawn.contains(&all[14]));
    assert!(!drawn.contains(&all[0]));
}

#[test]
fn down_on_the_last_row_of_the_list_focuses_the_entry_bar() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
}

#[test]
fn slash_resume_from_a_session_focuses_the_first_exited_row_after_the_home_frame() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "current work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    // The conversation frame first, as the loop draws it.
    drawn(&mut app);
    assert!(matches!(enter_text(&mut app, "/resume"), Effect::Send(_)));
    assert!(app.on_home());
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let exited = *keys(&app)
        .get(1)
        .expect("the exited row follows the live row");
    let row = app
        .home
        .as_ref()
        .and_then(|home| home.sessions.by_key(exited))
        .unwrap_or_else(|| panic!("the exited row is present"));
    assert_eq!(row.left, Some(crate::home::Left::Exited));
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(exited)))
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
    assert_eq!(lines[0]["session_id"], "s_bbbbbbbbbbbbbbbb");
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
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.on_key(Key::Char('y'), now),
        Effect::Copy("✓  fix the parser".to_owned())
    );
}

/// The chips home draws, as text left to right.
fn chips(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.chips.into_iter().map(|chip| chip.text).collect())
        .unwrap_or_default()
}

/// Clicks the workspace chip: the picker opens browsing the chip's
/// workspace, asking for its one listing. Answers it with no
/// subdirectories and moves down onto the first recent.
fn open_on_recents(app: &mut App) {
    let Effect::Send(lines) = app.home_click(Spot::Workspace) else {
        panic!("the picker asks for its root's listing");
    };
    let asks = commands(lines);
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0]["command"], "directories");
    let id = asks[0]["id"].as_str().unwrap_or_else(|| panic!("id"));
    app.on_line(accepted(id, json!({"directories": [], "path": "/listed"})));
    app.on_key(Key::Down, fakes::clock::FakeClock::new().now());
}

/// The workspace picker home draws: its list and the selected index.
fn picker(app: &App) -> Option<(Vec<String>, usize)> {
    app.home_screen().and_then(|screen| {
        screen
            .picker
            .map(|picker| (picker.recents, picker.focus.unwrap_or(0)))
    })
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
            "[test/model]",
            "[thinking: default]",
            "Enter starts a session",
        ]
    );
}

#[test]
fn the_picker_lists_the_launch_directory_then_row_workspaces_once_each() {
    let mut app = home();
    three_workspaces(&mut app);
    open_on_recents(&mut app);
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
    open_on_recents(&mut app);
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
    open_on_recents(&mut app);
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
    open_on_recents(&mut app);
    assert!(picker(&app).is_some());
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(picker(&app), None);
    assert_eq!(chips(&app)[0], "[other]");
}

#[test]
fn other_keys_do_nothing_in_the_picker() {
    let mut app = home();
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Char('x'), now), Effect::None);
    assert_eq!(app.on_key(Key::PageDown, now), Effect::None);
    assert!(app.input().expand().is_empty());
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(0));
    assert_eq!(app.focused(), None);
}

#[test]
fn ctrl_c_passes_through_the_picker() {
    let mut app = home();
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    assert_eq!(app.home_key(&Key::CtrlC), None);
    assert!(picker(&app).is_some());
}

#[test]
fn clicking_plus_chooses_that_projects_workspace_and_focuses_the_entry_bar() {
    let mut app = git_home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "other project work",
        "/other",
        "-other",
        json!({"state": "idle", "git": {"branch": "main"}}),
    ));
    let key = app
        .home_screen()
        .and_then(|screen| {
            screen.list.into_iter().find_map(|line| match line {
                crate::home::ListLine::Head {
                    name,
                    start: Some(key),
                    ..
                } if name == "other" => Some(key),
                crate::home::ListLine::Head { .. }
                | crate::home::ListLine::Live { .. }
                | crate::home::ListLine::ExitedHead { .. }
                | crate::home::ListLine::Exited { .. }
                | crate::home::ListLine::More { .. } => None,
            })
        })
        .unwrap_or_else(|| panic!("the other project's plus target"));
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == crate::mouse::TargetId::Home(Spot::Start(key)) })
    );
    app.drawn(&targets);
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    assert!(chips(&app).contains(&"[x] new worktree".to_owned()));
    app.focus = Some(crate::mouse::TargetId::Home(Spot::Worktree));
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Start(key))),
        Effect::None
    );
    assert_eq!(app.focused(), None, "the entry bar has focus");
    assert!(chips(&app).contains(&"[ ] new worktree".to_owned()));

    let Effect::Send(lines) = enter_text(&mut app, "hi") else {
        panic!("Enter sends the start");
    };
    let start = commands(lines)[0].clone();
    assert_eq!(start["command"], "start");
    assert_eq!(start["args"]["workspace"], "/other");
    assert!(start["args"].get("worktree").is_none());
}

#[test]
fn start_names_the_chosen_workspace() {
    let mut app = home();
    three_workspaces(&mut app);
    open_on_recents(&mut app);
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
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(2)), Effect::None);
    assert_eq!(picker(&app), None);
    assert_eq!(chips(&app)[0], "[third]");
}

#[test]
fn a_click_past_the_picker_list_does_nothing() {
    let mut app = home();
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(99)), Effect::None);
    assert!(picker(&app).is_some());
    assert_eq!(chips(&app)[0], "[w]");
}

#[test]
fn the_file_listing_follows_the_picked_workspace() {
    let mut app = home();
    three_workspaces(&mut app);
    assert_eq!(app.workspace(), PathBuf::from("/w"));
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(1)), Effect::None);
    assert_eq!(app.workspace(), PathBuf::from("/other"));
}

/// An app on home inside git: scoped to the launch project.
fn git_home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: true,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A waiting `session_status` for `session` in `workspace` of `project`.
fn waiting(session: &str, name: &str, workspace: &str, project: &str) -> Line {
    live(
        session,
        name,
        workspace,
        project,
        json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "shell"}}),
    )
}

#[test]
fn the_first_recent_page_names_no_project() {
    let mut app = git_home();
    let lines = commands(app.on_line(hello()));
    let id = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert_eq!(lines[1], json!({"id": id, "command": "recent"}));
}

#[test]
fn down_from_the_last_row_asks_the_next_page_before_the_last_loaded_recent_row() {
    let mut app = git_home();
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    // ↓ on the last row asks the next page at once.
    let Effect::Send(lines) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "recent");
    assert_eq!(line["args"]["before"], "s_bbbbbbbbbbbbbbbb");
    assert!(line["args"].get("project").is_none());
    // Focus stays on the last row's line, never its ✕.
    let row = keys(&app)[1];
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
}

#[test]
fn no_page_is_asked_from_a_feed_row() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    // The `recent` page names only the feed row, so nothing lists, but
    // the answer was not empty.
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "fix the parser")]}),
    ));
    assert!(app.home.as_ref().is_some_and(|home| home.sessions.more()));
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
}

#[test]
fn down_from_the_last_row_asks_nothing_after_an_empty_page() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    let page: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    // An empty older page ends paging.
    assert!(
        app.on_line(accepted(
            page["id"].as_str().unwrap_or_else(|| panic!("page id")),
            json!({"sessions": []})
        ))
        .is_empty()
    );
    assert!(app.home.as_ref().is_some_and(|home| !home.sessions.more()));
    // ↓ returns nothing with focus on the row's line.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn enter_on_more_expands_in_place_and_focuses_the_first_revealed_row() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(
        &mut app,
        &recent,
        &[
            ("s_aaaaaaaaaaaaaaaa", "newest"),
            ("s_bbbbbbbbbbbbbbbb", "second"),
            ("s_cccccccccccccccc", "third"),
            ("s_dddddddddddddddd", "fourth"),
        ],
    );
    let key = more_key(&app, "-w").expect("the fourth row is hidden");
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == crate::mouse::TargetId::Home(Spot::More(key)) })
    );
    app.drawn(&targets);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(key)))
    );
    let screen = app.home_screen().unwrap();
    assert!(screen.list.iter().any(|line| matches!(
        line,
        crate::home::ListLine::Exited { key: shown, .. } if *shown == key
    )));
    assert!(
        !screen
            .list
            .iter()
            .any(|line| matches!(line, crate::home::ListLine::More { .. }))
    );
}

#[test]
fn a_click_on_more_expands_the_same_project_only() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let rows = [
        ("s_aaaaaaaaaaaaaaaa", "a1", "/w", "-w"),
        ("s_bbbbbbbbbbbbbbbb", "a2", "/w", "-w"),
        ("s_cccccccccccccccc", "a3", "/w", "-w"),
        ("s_dddddddddddddddd", "a4", "/w", "-w"),
        ("s_eeeeeeeeeeeeeeee", "b1", "/other", "-other"),
        ("s_ffffffffffffffff", "b2", "/other", "-other"),
        ("s_1111111111111111", "b3", "/other", "-other"),
        ("s_2222222222222222", "b4", "/other", "-other"),
    ]
    .into_iter()
    .map(|(id, name, workspace, project)| exited_in(id, name, workspace, project))
    .collect::<Vec<_>>();
    assert!(
        app.on_line(accepted(&recent, json!({"sessions": rows})))
            .is_empty()
    );
    let first_hidden = more_key(&app, "-w").expect("first project has more");
    let second_hidden = more_key(&app, "-other").expect("second project has more");
    assert_eq!(app.home_click(Spot::More(first_hidden)), Effect::None);
    let screen = app.home_screen().unwrap();
    let shown = screen
        .list
        .iter()
        .filter_map(|line| match line {
            crate::home::ListLine::Head { .. }
            | crate::home::ListLine::Live { .. }
            | crate::home::ListLine::ExitedHead { .. }
            | crate::home::ListLine::More { .. } => None,
            crate::home::ListLine::Exited { key, .. } => Some(*key),
        })
        .collect::<Vec<_>>();
    assert!(shown.contains(&first_hidden));
    assert!(!shown.contains(&second_hidden));
    assert_eq!(
        screen
            .list
            .iter()
            .filter_map(|line| match line {
                crate::home::ListLine::Head { .. }
                | crate::home::ListLine::Live { .. }
                | crate::home::ListLine::ExitedHead { .. }
                | crate::home::ListLine::Exited { .. } => None,
                crate::home::ListLine::More { key, .. } => Some(*key),
            })
            .collect::<Vec<_>>(),
        [second_hidden]
    );
}

#[test]
fn y_on_more_copies_its_label() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(
        &mut app,
        &recent,
        &[
            ("s_aaaaaaaaaaaaaaaa", "newest"),
            ("s_bbbbbbbbbbbbbbbb", "second"),
            ("s_cccccccccccccccc", "third"),
            ("s_dddddddddddddddd", "fourth"),
        ],
    );
    let key = more_key(&app, "-w").expect("the fourth row is hidden");
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(
        app.on_key(Key::Char('y'), now),
        Effect::Copy("… 1 more".to_owned())
    );
}

#[test]
fn y_on_more_copies_a_positive_count_but_not_zero() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    answer_recent(
        &mut app,
        &recent,
        &[
            ("s_aaaaaaaaaaaaaaaa", "newest"),
            ("s_bbbbbbbbbbbbbbbb", "second"),
            ("s_cccccccccccccccc", "third"),
            ("s_dddddddddddddddd", "fourth"),
            ("s_eeeeeeeeeeeeeeee", "fifth"),
        ],
    );
    let key = more_key(&app, "-w").expect("two rows are hidden");
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(
        app.on_key(Key::Char('y'), now),
        Effect::Copy("… 2 more".to_owned())
    );

    app.home.as_mut().unwrap().sessions.expand("-w");
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    assert_eq!(app.on_key(Key::Char('y'), now), Effect::None);
}

#[test]
fn slash_resume_with_live_rows_but_no_exited_rows_leaves_the_box_focused() {
    let mut app = git_home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(waiting("s_bbbbbbbbbbbbbbbb", "away", "/lens", "-other"));
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "here",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    drawn(&mut app);
    assert!(matches!(enter_text(&mut app, "/resume"), Effect::Send(_)));
    assert!(app.on_home());
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    assert_eq!(app.focused(), None);
}

/// The blocker lines home draws above the box.
fn blockers(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.blockers)
        .unwrap_or_default()
}

/// Sends `text` as a `start` and answers it with `session`.
fn start_rejected(app: &mut App, id: &str, message: &str) {
    assert!(
        app.on_line(hub_refused(id, "start_failed", message))
            .is_empty()
    );
}

#[test]
fn a_rejected_start_sets_the_blocker_lines() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let Effect::Send(lines) = enter_text(&mut app, "hi") else {
        panic!("Enter sends the start");
    };
    let start: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    let id = start["id"].as_str().unwrap_or_else(|| panic!("start id"));
    let message = "No provider key.\nRun fiber login to fix it.";
    start_rejected(&mut app, id, message);
    assert_eq!(
        blockers(&app),
        ["No provider key.", "Run fiber login to fix it."]
    );
    // The refusal stays a notice as today.
    assert_eq!(app.notice(), Some(message));
    // Answering `recent` fills the list around the blockers.
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "old work")]}),
    ));
    assert_eq!(
        blockers(&app),
        ["No provider key.", "Run fiber login to fix it."]
    );
    assert_eq!(rows(&app), ["○  old work"]);
}

#[test]
fn a_rejected_prompt_sets_none() {
    let mut app = home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    let Effect::Send(lines) = enter_text(&mut app, "yo") else {
        panic!("Enter sends the prompt");
    };
    let prompt: Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("prompt: {err}"));
    let id = prompt["id"].as_str().unwrap_or_else(|| panic!("prompt id"));
    assert!(
        app.on_line(hub_refused(id, "invalid_arguments", "bad prompt"))
            .is_empty()
    );
    assert!(
        app.home
            .as_ref()
            .is_some_and(|home| home.blockers.is_empty())
    );
}

#[test]
fn the_next_start_clears_them() {
    let mut app = home();
    app.on_line(hello());
    let Effect::Send(lines) = enter_text(&mut app, "hi") else {
        panic!("Enter sends the start");
    };
    let start: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    start_rejected(
        &mut app,
        start["id"].as_str().unwrap_or_else(|| panic!("start id")),
        "No provider key.",
    );
    assert_eq!(blockers(&app), ["No provider key."]);
    let Effect::Send(_) = enter_text(&mut app, "again") else {
        panic!("Enter tries again");
    };
    assert!(blockers(&app).is_empty());
}

#[test]
fn the_feed_arriving_keeps_them() {
    let mut app = home();
    let (feed, recent) = linked(&mut app);
    let Effect::Send(lines) = enter_text(&mut app, "hi") else {
        panic!("Enter sends the start");
    };
    let start: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    start_rejected(
        &mut app,
        start["id"].as_str().unwrap_or_else(|| panic!("start id")),
        "No provider key.",
    );
    assert!(app.on_line(accepted(&feed, json!({}))).is_empty());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited("s_bbbbbbbbbbbbbbbb", "old work")]}),
    ));
    assert_eq!(blockers(&app), ["No provider key."]);
}

#[test]
fn x_on_an_unsubscribed_live_row_sends_summary_then_close_now() {
    let mut app = home();
    two_rows(&mut app);
    let lines = stop(&mut app);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[0]["args"]["level"], "summary");
    assert_eq!(lines[1]["command"], "close");
    assert_eq!(lines[1]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[1]["args"], json!({"now": true}));
    assert_ne!(lines[0]["id"], lines[1]["id"]);
}

#[test]
fn x_on_a_subscribed_row_sends_close_now_only() {
    let mut app = home();
    two_rows(&mut app);
    let first = stop(&mut app);
    let summary = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", summary))
            .is_empty()
    );
    let lines = stop(&mut app);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "close");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[0]["args"], json!({"now": true}));
}

#[test]
fn the_foot_names_the_key_maps_bound_key() {
    let mut app = home();
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!("f2"));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F2 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_leaves_the_key_map_out_when_unbound() {
    let mut app = home();
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(foot(&app), "↑ sessions · ↓ chips · Ctrl+C twice to quit");
}

#[test]
fn x_with_the_link_down_sends_nothing() {
    let mut app = home();
    app.connect_failed("gone".to_owned());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    assert_eq!(click_stop(&mut app), Effect::None);
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn a_refused_close_notes_the_row() {
    let mut app = home();
    two_rows(&mut app);
    let lines = stop(&mut app);
    let close = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("close id"));
    let message = "Session `s_aaaaaaaaaaaaaaaa` is held.";
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            close,
            "session_held",
            message
        ))
        .is_empty()
    );
    assert_eq!(
        listed(&app),
        [
            format!("●  fix the parser  {message}"),
            "✓  tidy docs".to_owned(),
        ]
    );
    assert_eq!(app.notice(), Some(message));
}

#[test]
fn x_on_an_exited_row_asks_to_delete() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    assert_eq!(click_stop(&mut app), Effect::None);
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn backspace_on_a_focused_exited_row_asks() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn delete_on_a_focused_crashed_row_asks() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [crashed("s_aaaaaaaaaaaaaaaa", "dead work")]}),
    ));
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_edit(crate::keys::Edit::Delete), Effect::None);
    assert_eq!(
        question(&app),
        "Delete dead work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn backspace_on_a_focused_live_row_does_nothing() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn backspace_with_focus_on_another_stop_is_still_swallowed() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert_eq!(app.on_edit(crate::keys::Edit::Delete), Effect::None);
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
    assert!(app.input().expand().is_empty());
}

#[test]
fn enter_sends_delete_without_cascade() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let lines = commands(lines);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "delete");
    assert_eq!(lines[0]["args"]["session"], "s_aaaaaaaaaaaaaaaa");
    assert!(lines[0]["args"].get("cascade").is_none());
}

#[test]
fn esc_sends_nothing() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
}

/// Draws `app` at 80x24, returning its text and targets.
fn drawn_frame(app: &App) -> (String, Vec<crate::mouse::Target>) {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), targets)
}

#[test]
fn clicking_the_drawn_cross_keeps_the_session() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    // The draw holds exactly one ✕ target, ending the title row.
    let (text, targets) = drawn_frame(&app);
    let title = text
        .lines()
        .position(|row| row.contains("Delete session"))
        .unwrap_or_else(|| panic!("a title row"));
    let keep: Vec<_> = targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::Home(Spot::Keep))
        .collect();
    assert_eq!(keep.len(), 1, "one ✕ target");
    assert_eq!(keep[0].rect.y, u16::try_from(title).unwrap_or(u16::MAX));
    assert_eq!((keep[0].rect.width, keep[0].rect.height), (1, 1));
    let row = text
        .lines()
        .nth(title)
        .unwrap_or_else(|| panic!("a title row"));
    let before: String = row.chars().take(keep[0].rect.x as usize).collect();
    assert!(
        before.contains("Delete session"),
        "the ✕ ends the title: {row}"
    );
    assert_eq!(
        row.chars().nth(keep[0].rect.x as usize),
        Some('✕'),
        "the target is the ✕: {row}"
    );
    // Clicking it closes the question without sending a command.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Keep)),
        Effect::None
    );
    assert!(question(&app).is_empty(), "the question closes");
    assert_eq!(
        app.home_screen()
            .map(|screen| crate::home::home_test_rows(&screen).len())
            .unwrap_or_default(),
        1,
        "the session row remains"
    );
}

#[test]
fn clicking_the_cross_on_the_cascade_question_keeps_everything() {
    let mut app = home();
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(
            &delete,
            "session_has_dependents",
            "Session `s_0123456789abcdef` has sessions that continue it: \
            `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too."
        ))
        .is_empty()
    );
    let (_, targets) = drawn_frame(&app);
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.id == crate::mouse::TargetId::Home(Spot::Keep))
            .count(),
        1,
        "one ✕ target"
    );
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Keep)),
        Effect::None
    );
    assert!(question(&app).is_empty(), "the question closes");
    assert_eq!(
        app.home_screen()
            .map(|screen| crate::home::home_test_rows(&screen).len())
            .unwrap_or_default(),
        1,
        "the session row remains"
    );
}

#[test]
fn a_stale_keep_click_changes_nothing() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    assert!(question(&app).is_empty(), "no question is open");
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Keep)),
        Effect::None
    );
    assert!(question(&app).is_empty(), "no question opens");
}

#[test]
fn a_keep_click_leaves_the_quit_question_open() {
    let mut app = home();
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "busy work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.quit();
    assert!(app.quit_open(), "the quit question is open");
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Keep)),
        Effect::None
    );
    assert!(app.quit_open(), "the quit question stays open");
}

#[test]
fn clicks_on_the_delete_frame_do_nothing() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let asked = question(&app);
    assert!(!asked.is_empty(), "the question is open");
    let (text, targets) = drawn_frame(&app);
    // One slab-wide target over every frame row, under the ✕.
    let slabs: Vec<_> = targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::Home(Spot::Question))
        .collect();
    assert_eq!(slabs.len(), 1, "one slab target");
    let slab = slabs[0].rect;
    let rows: Vec<&str> = text.lines().collect();
    let title = u16::try_from(
        rows.iter()
            .position(|row| row.contains("Delete session"))
            .unwrap_or_else(|| panic!("a title row")),
    )
    .unwrap_or(u16::MAX);
    let body = u16::try_from(
        rows.iter()
            .position(|row| row.contains("Delete old work"))
            .unwrap_or_else(|| panic!("a body row")),
    )
    .unwrap_or(u16::MAX);
    let footer = u16::try_from(
        rows.iter()
            .position(|row| row.contains("Enter delete · Esc keep"))
            .unwrap_or_else(|| panic!("a footer row")),
    )
    .unwrap_or(u16::MAX);
    // The title row away from the ✕, both edge rows, a pad row, a
    // body row and the footer row all hit the slab, and clicking
    // opens nothing and replaces no question.
    for (col, row) in [
        (slab.x.saturating_add(2), title),
        (slab.x.saturating_add(5), slab.y),
        (slab.x.saturating_add(5), slab.y.saturating_add(1)),
        (slab.x.saturating_add(5), body),
        (slab.x.saturating_add(5), footer),
        (slab.x.saturating_add(5), slab.bottom().saturating_sub(1)),
    ] {
        assert_eq!(
            crate::mouse::hit(&targets, col, row),
            Some(crate::mouse::TargetId::Home(Spot::Question)),
            "a frame cell at {col},{row} hits the slab"
        );
        assert_eq!(
            app.on_click(crate::mouse::TargetId::Home(Spot::Question)),
            Effect::None
        );
    }
    assert_eq!(question(&app), asked, "the question stays");
    assert!(app.session().is_none(), "no session opens");
}

#[test]
fn hovering_the_delete_slab_tints_nothing() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut plain = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut plain, None);
    let slab = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Home(Spot::Question))
        .unwrap_or_else(|| panic!("a slab target"))
        .rect;
    let keep = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Home(Spot::Keep))
        .unwrap_or_else(|| panic!("a ✕ target"))
        .rect;
    // A body cell under the pointer draws as without one.
    let mut hovered = ratatui::buffer::Buffer::empty(area);
    crate::view::render(
        &app,
        area,
        &mut hovered,
        Some((slab.x.saturating_add(5), slab.y.saturating_add(4))),
    );
    assert_eq!(hovered, plain, "no lift on the slab");
    // The ✕ still lifts, so the comparison above is sensitive.
    let mut crossed = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut crossed, Some((keep.x, keep.y)));
    assert_ne!(crossed, plain, "the ✕ lifts");
}

#[test]
fn other_keys_do_nothing_in_the_question() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Char('x'), now), Effect::None);
    assert!(app.input().expand().is_empty());
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn ctrl_c_passes_through_the_question() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlC, now), Effect::None);
    assert!(app.hint());
}

#[test]
fn up_and_down_scroll_the_delete_question() {
    let mut app = home();
    // A small screen: the cascade question wraps past what fits, so the
    // offset scrolls by one row.
    app.set_size(60, 12);
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(
            &delete,
            "session_has_dependents",
            "Session `s_0123456789abcdef` has sessions that continue it: \
            `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too."
        ))
        .is_empty()
    );
    // Down and Up scroll the wrapped rows past the screen: Down toward
    // the later rows, holding at the last one, Up back toward the
    // first, holding at the top.
    assert_eq!(question_scroll(&app), 0);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(question_scroll(&app), 1);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(question_scroll(&app), 1);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(question_scroll(&app), 0);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(question_scroll(&app), 0);
    assert!(!question(&app).is_empty(), "the question stays open");
}

#[test]
fn an_accepted_delete_removes_the_row_and_asks_recent_again() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited("s_bbbbbbbbbbbbbbbb", "older work"),
        ]}),
    ));
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let lines = commands(app.on_line(accepted(&delete, json!({}))));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "recent");
    assert!(lines[0].get("args").is_none());
    assert_eq!(rows(&app), ["○  older work"]);
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn dependents_ask_again_naming_them_and_enter_sends_cascade_with_expect() {
    let mut app = home();
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let message = "Session `s_0123456789abcdef` has sessions that continue it: \
        `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too.";
    assert!(
        app.on_line(hub_refused(&delete, "session_has_dependents", message))
            .is_empty()
    );
    assert_eq!(
        question(&app),
        "Delete fix the parser (s_0123456789abcdef) and 2 sessions that continue it: \
        s_1111111111111111, s_2222222222222222? It cannot be undone · Enter delete all · Esc keep"
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes all");
    };
    let lines = commands(lines);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "delete");
    assert_eq!(
        lines[0]["args"],
        json!({
            "session": "s_0123456789abcdef",
            "cascade": true,
            "expect": [
                "s_0123456789abcdef",
                "s_1111111111111111",
                "s_2222222222222222",
            ],
        })
    );
}

#[test]
fn a_stale_cascade_asks_again_with_the_new_set() {
    let mut app = home();
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(
            &delete,
            "session_has_dependents",
            "Session `s_0123456789abcdef` has sessions that continue it: \
            `s_1111111111111111`. `--cascade` deletes them too."
        ))
        .is_empty()
    );
    // One other session reads singular.
    assert_eq!(
        question(&app),
        "Delete fix the parser (s_0123456789abcdef) and 1 session that continues it: \
        s_1111111111111111? It cannot be undone · Enter delete all · Esc keep"
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes all");
    };
    let cascade = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("cascade id"))
        .to_owned();
    let message = "The sessions this delete would remove are now `s_0123456789abcdef`, \
        `s_1111111111111111`, `s_3333333333333333`. Nothing was deleted.";
    assert!(
        app.on_line(hub_refused(&cascade, "stale_request", message))
            .is_empty()
    );
    assert_eq!(
        question(&app),
        "Delete fix the parser (s_0123456789abcdef) and 2 sessions that continue it: \
        s_1111111111111111, s_3333333333333333? It cannot be undone · Enter delete all · Esc keep"
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes all");
    };
    let lines = commands(lines);
    assert_eq!(
        lines[0]["args"]["expect"],
        json!([
            "s_0123456789abcdef",
            "s_1111111111111111",
            "s_3333333333333333",
        ])
    );
}

#[test]
fn a_dependents_refusal_with_no_other_ids_notes_the_row() {
    let mut app = home();
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let message = "Session `s_0123456789abcdef` has sessions that continue it: \
        `s_0123456789abcdef`. `--cascade` deletes them too.";
    assert!(
        app.on_line(hub_refused(&delete, "session_has_dependents", message))
            .is_empty()
    );
    assert_eq!(listed(&app), [format!("○  fix the parser  {message}")]);
    assert_eq!(app.notice(), Some(message));
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
    // The notice carries the refusal's own code, not a default.
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, text, .. }
        if code == "session_has_dependents" && text == message
    )));
}

#[test]
fn a_refusal_code_is_the_payload_code_or_malformed() {
    // An empty code is no code: the notice then reads `malformed`.
    let payload = |value: Value| value.as_object().cloned().unwrap_or_default();
    assert_eq!(
        super::refusal_code(&payload(json!({"code": "session_held"}))),
        "session_held"
    );
    assert_eq!(
        super::refusal_code(&payload(json!({"code": ""}))),
        "malformed"
    );
    assert_eq!(super::refusal_code(&payload(json!({}))), "malformed");
}

#[test]
fn a_refused_cascade_delete_notes_the_row() {
    let mut app = home();
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(
            &delete,
            "session_has_dependents",
            "Session `s_0123456789abcdef` has sessions that continue it: \
            `s_1111111111111111`. `--cascade` deletes them too."
        ))
        .is_empty()
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes all");
    };
    let cascade = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("cascade id"))
        .to_owned();
    let message = "Session `s_0123456789abcdef` is held.";
    assert!(
        app.on_line(hub_refused(&cascade, "session_held", message))
            .is_empty()
    );
    assert_eq!(listed(&app), [format!("○  fix the parser  {message}")]);
    assert_eq!(app.notice(), Some(message));
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn unreadable_rows_have_no_x() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let mut envelope = live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    );
    if let crate::link::Line::Session(status) = &mut envelope {
        status.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(envelope);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited("s_cccccccccccccccc", "old work")]}),
    ));
    // A readable live row ends in a ✕; exited and unreadable rows have
    // none.
    let screen = app.home_screen().unwrap_or_else(|| panic!("home screen"));
    let rows = crate::home::home_test_rows(&screen);
    let has_x: Vec<bool> = screen
        .list
        .iter()
        .filter_map(|line| match line {
            crate::home::ListLine::Live { stop, .. } => Some(*stop),
            crate::home::ListLine::Exited { .. } => Some(false),
            crate::home::ListLine::Head { .. }
            | crate::home::ListLine::ExitedHead { .. }
            | crate::home::ListLine::More { .. } => None,
        })
        .collect();
    assert_eq!(rows.len(), 3);
    assert!(has_x[0]);
    assert!(!has_x[1]);
    assert!(!has_x[2]);
}

#[test]
fn x_on_an_unreadable_row_says_so() {
    let mut app = home();
    linked(&mut app);
    let mut envelope = status("s_aaaaaaaaaaaaaaaa");
    if let crate::link::Line::Session(status) = &mut envelope {
        status.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(envelope);
    assert_eq!(click_stop(&mut app), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Cannot attach: this session's schema is newer than this terminal reads.")
    );
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, text, .. }
        if code == "session_held"
            && text == "Cannot attach: this session's schema is newer than this terminal reads."
    )));
}

#[test]
fn enter_with_the_link_down_keeps_the_question() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    app.disconnected();
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn an_accepted_close_changes_nothing() {
    let mut app = home();
    two_rows(&mut app);
    let lines = stop(&mut app);
    let close = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("close id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", close))
            .is_empty()
    );
    assert_eq!(listed(&app), ["●  fix the parser", "✓  tidy docs"]);
    assert_eq!(app.notice(), None);
}

#[test]
fn backspace_with_the_box_focused_edits_the_draft() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    type_text(&mut app, "x");
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert!(app.input().expand().is_empty());
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn delete_edit_in_the_question_does_nothing() {
    let mut app = home();
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    assert_eq!(app.on_edit(crate::keys::Edit::Delete), Effect::None);
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn stop_on_a_key_with_no_row_does_nothing() {
    let mut app = home();
    two_rows(&mut app);
    assert_eq!(app.home_click(Spot::Stop(999)), Effect::None);
    assert_eq!(
        foot(&app),
        "↑ sessions · ↓ chips · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn y_on_a_focused_x_copies_nothing() {
    let mut app = home();
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    // ↑ focuses the last row, then another ↑ reaches the first; j steps
    // onto its ✕.
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Stop(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Char('y'), now), Effect::None);
}

#[test]
fn a_crashed_left_marks_the_known_row_crashed() {
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
    // A crash marks the known row in place; the other row keeps working.
    assert!(
        app.on_line(left("s_aaaaaaaaaaaaaaaa", "crashed"))
            .is_empty()
    );
    assert_eq!(rows(&app), ["✗  fix the parser", "✓  tidy docs"]);
}

#[test]
fn an_accepted_hub_acknowledgement_ends_the_open() {
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
    // Accepting the open's last subscribe ends the gate: the session
    // stays attached, with no note and no notice.
    assert!(app.on_line(accepted(&ack, json!({}))).is_empty());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    assert!(!app.on_home());
    assert!(app.notice().is_none());
}

#[test]
fn statuses_gate_only_the_opening_session() {
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
        json!({"state": "streaming"}),
    ));
    open_first(&mut app);
    // While the first row opens, its statuses fold into its row and are
    // dropped; another session's statuses still reach the conversation.
    assert!(
        app.home_line(&live(
            "s_aaaaaaaaaaaaaaaa",
            "renamed",
            "/w",
            "-w",
            json!({"state": "streaming"}),
        ))
        .is_some()
    );
    assert!(
        app.home_line(&live(
            "s_bbbbbbbbbbbbbbbb",
            "renamed",
            "/w",
            "-w",
            json!({"state": "streaming"}),
        ))
        .is_none()
    );
}

#[test]
fn a_rejected_lower_while_home_sends_no_subscribe() {
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
        json!({"state": "streaming"}),
    ));
    // The second row opens at `full`; going home lowers it, with the
    // lowering subscribe in flight.
    let second = keys(&app)[1];
    let lines = open(&mut app, second);
    let full = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("full id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_bbbbbbbbbbbbbbbb", &full))
            .is_empty()
    );
    let Effect::Send(lines) = enter_text(&mut app, "/home") else {
        panic!("going home lowers");
    };
    assert_eq!(lines.len(), 1);
    let lower: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(lower["command"], "subscribe");
    assert_eq!(lower["args"]["level"], "summary");
    let lowering = lower["id"]
        .as_str()
        .unwrap_or_else(|| panic!("lowering id"))
        .to_owned();
    // Refusing the lowering while home sends nothing more: the accepted
    // level stays `full` without another subscribe.
    assert!(
        app.on_line(session_refused(
            "s_bbbbbbbbbbbbbbbb",
            &lowering,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
}

#[test]
fn home_rows_include_an_unscoped_project_below_the_fold() {
    let mut app = home();
    linked(&mut app);
    for n in 0..9u8 {
        let session = format!("s_{n:016x}");
        app.on_line(live(
            &session,
            "fix the parser",
            "/w",
            "-w",
            json!({"state": "idle"}),
        ));
    }
    // The tenth row lives in another project: outside git it still
    // lists, below the fold.
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "away",
        "/lens",
        "-other",
        json!({"state": "idle"}),
    ));
    for n in 9..14u8 {
        let session = format!("s_{n:016x}");
        app.on_line(live(
            &session,
            "fix the parser",
            "/w",
            "-w",
            json!({"state": "idle"}),
        ));
    }
    let screen = app
        .home_screen()
        .unwrap_or_else(|| panic!("the home screen"));
    assert_eq!(crate::home::home_test_rows(&screen).len(), 15);
    assert!(
        crate::home::home_test_rows(&screen)
            .iter()
            .any(|(_, row)| row.contains("away")),
        "the other project's session remains visible"
    );
}

#[test]
fn an_accepted_delete_closes_its_question() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited("s_bbbbbbbbbbbbbbbb", "older work"),
        ]}),
    ));
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    app.on_line(accepted(&delete, json!({})));
    assert_eq!(rows(&app), ["○  older work"]);
    // Its answer closes the question it asked about: the prompt clears,
    // so the drawn question goes with it.
    assert!(
        app.home
            .as_ref()
            .is_some_and(|home| home.questions.prompt().is_none())
    );
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.question.is_none())
    );
}

#[test]
fn an_answer_for_another_delete_keeps_the_newer_question() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited("s_bbbbbbbbbbbbbbbb", "older work"),
        ]}),
    ));
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    // A newer question asks about the second row; the first row's answer
    // leaves it open.
    let second = keys(&app)[1];
    app.home_click(Spot::Stop(second));
    app.on_line(accepted(&delete, json!({})));
    assert_eq!(rows(&app), ["○  older work"]);
    assert!(
        question(&app).contains("s_bbbbbbbbbbbbbbbb"),
        "the newer question stays open"
    );
}

#[test]
fn an_accepted_delete_asks_the_first_page_unscoped() {
    let mut app = git_home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited("s_bbbbbbbbbbbbbbbb", "older work"),
        ]}),
    ));
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let lines = commands(app.on_line(accepted(&delete, json!({}))));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "recent");
    assert!(lines[0].get("args").is_none());
}

#[test]
fn the_next_page_in_git_is_unscoped() {
    let mut app = git_home();
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "recent");
    assert_eq!(line["args"], json!({"before": "s_bbbbbbbbbbbbbbbb"}));
}

/// A live `session_status` for `session` in `workspace`, in git on
/// `main`.
fn git_live(session: &str, name: &str, workspace: &str) -> Line {
    live(
        session,
        name,
        workspace,
        "-w",
        json!({"state": "idle", "git": {"branch": "main"}}),
    )
}

/// Whether home draws the new worktree switch.
fn shows_switch(app: &App) -> bool {
    chips(app).iter().any(|chip| chip.contains("new worktree"))
}

#[test]
fn the_switch_is_hidden_outside_git() {
    // Even with a git row in the launch workspace: outside git the
    // launch directory decides, and it is not in git.
    let mut app = home();
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "fix the parser", "/w"));
    assert!(!shows_switch(&app));
    assert_eq!(
        chips(&app),
        [
            "[w]",
            "[test/model]",
            "[thinking: default]",
            "Enter starts a session",
        ]
    );
}

#[test]
fn a_chosen_row_workspace_takes_its_git_flag() {
    let mut app = git_home();
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "git work", "/git-ws"));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "plain work",
        "/plain-ws",
        "-w",
        json!({"state": "idle"}),
    ));
    // The launch directory is in git, so the switch shows, off.
    assert_eq!(
        chips(&app),
        [
            "[w]",
            "[ ] new worktree",
            "[test/model]",
            "[thinking: default]",
            "Enter starts a session",
        ]
    );
    // Turning it on, then choosing the workspace without git: hidden,
    // and the choice turns the switch off.
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    assert_eq!(chips(&app)[1], "[x] new worktree");
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[0], "[plain-ws]");
    assert!(!shows_switch(&app));
    // Choosing the workspace with git shows the switch again, off: the
    // earlier choice turned it off.
    open_on_recents(&mut app);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[0], "[git-ws]");
    assert_eq!(chips(&app)[1], "[ ] new worktree");
}

#[test]
fn choosing_the_launch_workspace_with_no_rows_keeps_the_switch() {
    // No row names the launch directory, so reading the feed would
    // lose the flag: the choice keeps the launch flag.
    let mut app = git_home();
    linked(&mut app);
    assert!(shows_switch(&app));
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[0], "[w]");
    assert!(shows_switch(&app));
}

#[test]
fn a_chosen_workspace_keeps_the_switch_after_its_row_leaves() {
    // The flag is read when the workspace is chosen, so dropping its
    // backing row keeps the switch.
    let mut app = git_home();
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "git work", "/git-ws"));
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(1)), Effect::None);
    assert_eq!(chips(&app)[0], "[git-ws]");
    assert!(shows_switch(&app));
    app.home
        .as_mut()
        .unwrap_or_else(|| panic!("home"))
        .sessions
        .remove(&contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert!(rows(&app).is_empty());
    assert_eq!(chips(&app)[0], "[git-ws]");
    assert!(shows_switch(&app));
}

#[test]
fn outside_git_choosing_a_plain_workspace_hides_the_switch() {
    // The launch directory is outside git, and a git row elsewhere
    // must not leak its flag into the chosen workspace.
    let mut app = home();
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "git work", "/git-ws"));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "plain work",
        "/plain-ws",
        "-w",
        json!({"state": "idle"}),
    ));
    assert!(!shows_switch(&app));
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[0], "[plain-ws]");
    assert!(!shows_switch(&app));
}

#[test]
fn clicking_a_picker_row_resets_the_switch_off() {
    let mut app = git_home();
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "git work", "/git-ws"));
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    assert_eq!(chips(&app)[1], "[x] new worktree");
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(1)), Effect::None);
    assert_eq!(chips(&app)[0], "[git-ws]");
    assert_eq!(chips(&app)[1], "[ ] new worktree");
}

#[test]
fn clicking_the_switch_toggles_it() {
    let mut app = git_home();
    assert_eq!(chips(&app)[1], "[ ] new worktree");
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Worktree)),
        Effect::None
    );
    assert_eq!(chips(&app)[1], "[x] new worktree");
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Worktree)),
        Effect::None
    );
    assert_eq!(chips(&app)[1], "[ ] new worktree");
    // The switch copies and opens nothing.
    assert_eq!(app.home_text(Spot::Worktree), None);
}

#[test]
fn enter_on_the_focused_switch_toggles_it() {
    let mut app = git_home();
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::Worktree));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[1], "[x] new worktree");
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[1], "[ ] new worktree");
}

#[test]
fn start_sends_worktree_only_when_the_switch_is_on_and_shown() {
    // Shown and on: the key goes out with the picked workspace.
    let mut app = git_home();
    app.on_line(hello());
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    assert_eq!(line["command"], "start");
    assert_eq!(line["args"]["workspace"], "/w");
    assert_eq!(line["args"]["worktree"], true);
    // Shown and off: the key is absent.
    let mut app = git_home();
    assert!(app.start_args().get("worktree").is_none());
    // Hidden, even with the switch on: the key is absent.
    let mut app = home();
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    let args = app.start_args();
    assert_eq!(args["workspace"], "/w");
    assert!(args.get("worktree").is_none());
}

#[test]
fn start_carries_the_terminal_layer_in_overrides() {
    // No layer, no key: the shape every existing start keeps.
    let mut app = home();
    let bare = app.start_args();
    assert_eq!(bare["workspace"], "/w");
    assert!(bare.get("overrides").is_none());
    // The layer rides in order, beside the workspace.
    app.set_launch_overrides(vec![
        "model=acme/m".to_owned(),
        "retry.attempts=2".to_owned(),
    ]);
    let args = app.start_args();
    assert_eq!(args["workspace"], "/w");
    assert_eq!(
        args["overrides"],
        json!(["model=acme/m", "retry.attempts=2"])
    );
    assert!(args.get("worktree").is_none());
    // Beside the worktree switch: both keys ride.
    let mut app = git_home();
    app.launch_overrides = vec!["model=acme/m".to_owned()];
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    let args = app.start_args();
    assert_eq!(args["overrides"], json!(["model=acme/m"]));
    assert_eq!(args["worktree"], true);
}

/// Focuses the last list stop from the entry bar.
fn focus_last(app: &mut App) {
    drawn(app);
    let last = keys(app)
        .into_iter()
        .last()
        .unwrap_or_else(|| panic!("a row"));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(last)))
    );
}

/// The foot home draws.
fn foot(app: &App) -> String {
    app.home_screen()
        .map(|screen| screen.foot)
        .unwrap_or_default()
}

/// The delete question home draws, unwrapped.
fn question(app: &App) -> String {
    app.home_screen()
        .and_then(|screen| screen.question)
        .unwrap_or_default()
}

/// The delete question's scroll offset: Up shows earlier rows, Down
/// later ones.
fn question_scroll(app: &App) -> usize {
    app.home_screen()
        .map(|screen| screen.question_scroll)
        .unwrap_or_default()
}

/// Clicks the first row's ✕, with the parsed lines going out.
fn stop(app: &mut App) -> Vec<Value> {
    let key = keys(app)[0];
    match app.home_click(Spot::Stop(key)) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("the ✕ sends"),
    }
}

/// Clicks the first row's ✕.
fn click_stop(app: &mut App) -> Effect {
    let key = keys(app)[0];
    app.home_click(Spot::Stop(key))
}

/// One crashed `recent` row for `session`.
fn crashed(session: &str, name: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": "crashed",
    })
}

/// An exited row from `recent`, by `session`.
fn exited_row(app: &mut App, session: &str, name: &str) {
    let (_, recent) = linked(app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited(session, name)]}),
    ));
}
