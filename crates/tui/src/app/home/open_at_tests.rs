//! Tests for the terminal's launch open: the session `fiber resume <id>`
//! and `fiber continue` name, or the session list `fiber resume` with no
//! id names.

use super::super::App;
use crate::OpenAt;
use crate::home::Spot;
use crate::link::Line;
use contract::SessionId;
use serde_json::{Value, json};
use std::path::PathBuf;

fn id(session: &str) -> SessionId {
    SessionId(session.to_owned())
}

/// An app on home at 80x24, launched to open `open_at`.
fn home_at(open_at: OpenAt) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at,
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// Whether the launch's open is spent: back to home.
fn spent(app: &App) -> bool {
    app.home
        .as_ref()
        .is_some_and(|home| home.launch.open_at == OpenAt::Home)
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

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
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
            ("command_id".to_owned(), Value::String(id.to_owned())),
            (
                "code".to_owned(),
                Value::String("invalid_arguments".to_owned()),
            ),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A `session_status` for `session`, idle and named.
fn status(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
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

/// Parses outgoing command lines.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The keys of the rows home draws.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(key, _, _)| key).collect())
        .unwrap_or_default()
}

/// Renders home at 80x24 and takes its targets, so keys move among drawn
/// stops.
fn drawn(app: &mut App) {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
}

#[test]
fn a_session_launch_sends_nothing_before_the_link_is_up() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(app.session().is_none());
    assert!(!spent(&app));
}

#[test]
fn a_session_launch_opens_with_the_first_feed_and_recent() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["command"], "feed");
    assert_eq!(lines[1]["command"], "recent");
    assert_eq!(lines[2]["command"], "subscribe");
    assert_eq!(lines[2]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[2]["args"]["level"], "full");
    assert_eq!(lines[3]["command"], "commands");
    assert_eq!(lines[3]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_ne!(lines[2]["id"], lines[3]["id"]);
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
    assert!(spent(&app));
}

#[test]
fn a_second_hello_sends_no_second_open() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    assert_eq!(commands(app.on_line(hello())).len(), 4);
    assert!(app.on_line(hello()).is_empty());
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
}

#[test]
fn a_refused_launch_open_shows_the_refusal_on_home() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    let lines = commands(app.on_line(hello()));
    let ack = lines[2]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(refused(ack, "held by another process"))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_list_launch_focuses_the_list_after_the_first_recent_page() {
    let mut app = home_at(OpenAt::List);
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert!(
        app.on_line(accepted(
            recent,
            json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "old work")]}),
        ))
        .is_empty()
    );
    assert!(spent(&app));
    drawn(&mut app);
    let row = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
    assert!(spent(&app));
}

#[test]
fn a_list_launch_with_no_rows_leaves_the_box_focused() {
    let mut app = home_at(OpenAt::List);
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert!(
        app.on_line(accepted(recent, json!({"sessions": []})))
            .is_empty()
    );
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    assert!(spent(&app));
}

#[test]
fn a_list_launch_with_an_empty_first_page_but_a_live_row_focuses_the_list() {
    let mut app = home_at(OpenAt::List);
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert!(
        app.on_line(accepted(recent, json!({"sessions": []})))
            .is_empty()
    );
    assert!(spent(&app));
    drawn(&mut app);
    let row = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
}

#[test]
fn a_list_launch_on_a_rejected_first_page_leaves_the_box_focused() {
    let mut app = home_at(OpenAt::List);
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert!(app.on_line(refused(recent, "no recent")).is_empty());
    assert_eq!(app.notice(), Some("no recent"));
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    assert!(spent(&app));
}

#[test]
fn a_home_launch_never_focuses_the_list() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert!(
        app.on_line(accepted(
            recent,
            json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "old work")]}),
        ))
        .is_empty()
    );
    drawn(&mut app);
    assert_eq!(app.focused(), None);
}

#[test]
fn a_later_recent_page_never_focuses_the_list() {
    let mut app = home_at(OpenAt::List);
    app.list_answered(false, true);
    assert!(!spent(&app));
    drawn(&mut app);
    assert_eq!(app.focused(), None);
}

/// A session `notice` line, as the hub forwards a refused switch.
fn notice(session: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "notice".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            (
                "code".to_owned(),
                Value::String("credential_missing".to_owned()),
            ),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A session `command_accepted` line for `id`.
fn session_accepted(session: &str, command: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), Value::String(command.to_owned()))]
            .into_iter()
            .collect(),
    })
}

#[test]
fn a_session_launch_with_a_label_puts_it_on_the_first_subscribe() {
    let mut app = home_at(OpenAt::Session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some("work".to_owned()),
    ));
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[2]["command"], "subscribe");
    assert_eq!(
        lines[2]["args"],
        json!({"level": "full", "credential": "work"}),
        "the complete args ride on the first subscribe"
    );
    assert_eq!(lines[3]["command"], "commands");
    assert!(spent(&app), "the launch carries the label once");
    // Spent: a second hello sends no second open, label or not.
    assert!(app.on_line(hello()).is_empty());
}

#[test]
fn a_session_launch_without_a_label_sends_no_credential_key() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines[2]["command"], "subscribe");
    assert_eq!(lines[2]["args"], json!({"level": "full"}));
    assert!(
        lines[2]["args"].get("credential").is_none(),
        "an absent label leaves the key out, never null"
    );
}

#[test]
fn an_expected_full_open_puts_the_label_on_the_first_subscribe_only() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some(crate::home::Level::Full),
        Some("work".to_owned()),
    ));
    let subscribes: Vec<&Value> = lines
        .iter()
        .filter(|line| line["command"] == "subscribe")
        .collect();
    assert_eq!(subscribes.len(), 2);
    assert_eq!(
        subscribes[0]["args"],
        json!({"level": "summary", "credential": "work"})
    );
    assert_eq!(subscribes[1]["args"], json!({"level": "full"}));
}

#[test]
fn a_row_open_and_a_reconnect_carry_no_label() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None));
    let subscribes: Vec<&Value> = lines
        .iter()
        .filter(|line| line["command"] == "subscribe")
        .collect();
    assert_eq!(subscribes.len(), 1);
    assert_eq!(subscribes[0]["args"], json!({"level": "full"}));
    // An ordinary subscription carries none either.
    let line: Value = serde_json::from_str(
        &app.subscribe(&id("s_aaaaaaaaaaaaaaaa"), crate::home::Level::Summary),
    )
    .unwrap();
    assert_eq!(line["args"], json!({"level": "summary"}));
}

#[test]
fn a_refused_launch_open_with_a_label_fails_home_as_before() {
    let mut app = home_at(OpenAt::Session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some("work".to_owned()),
    ));
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines[2]["args"]["credential"], "work");
    let ack = lines[2]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(refused(ack, "held by another process"))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_running_refusal_notice_between_acceptances_keeps_the_screen_attached() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some(crate::home::Level::Full),
        Some("work".to_owned()),
    ));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let last = lines[1]["id"].as_str().unwrap_or_else(|| panic!("last id"));
    // The first acceptance lands, then the switch refusal as a notice.
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first))
            .is_empty()
    );
    assert!(
        app.on_line(notice("s_aaaaaaaaaaaaaaaa", "no credential named `nope`"))
            .is_empty()
    );
    assert_eq!(app.notice(), Some("no credential named `nope`"));
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
    // Historical lines are still gated behind the final acceptance.
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    // The final acceptance ends the gate.
    app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", last));
    assert!(!app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
}

#[test]
fn a_gated_non_notice_line_is_dropped_without_a_notice() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some(crate::home::Level::Full),
        Some("work".to_owned()),
    ));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first))
            .is_empty()
    );
    // A gated line that is not a notice is dropped without becoming
    // one: only an opening notice reports while the replay stays
    // gated, so `||` here would file a spurious `rejected` notice.
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", "unknown"))
            .is_empty()
    );
    assert_eq!(app.notice(), None);
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
}
