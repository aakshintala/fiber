//! Tests for the rail's state: card numbers.

use super::super::App;
use crate::home::Launch;
use crate::link::Line;
use serde_json::{Value, json};
use std::path::PathBuf;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
const C: &str = "s_cccccccccccccccc";
const D: &str = "s_dddddddddddddddd";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// An app on home at 200x40, drawing the default card list.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.set_size(200, 40);
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

/// Parses command lines going out.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Links the app: the feed and recent ids.
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

/// A live `session_status` for `session`, idle unless `fields` say
/// otherwise.
fn live(session: &str, fields: Value) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": "/w", "project": "-w",
        "state": "idle", "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in fields.as_object().cloned().unwrap_or_default() {
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

/// A hub `session_left` for `session`, ended `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The row key of `session`'s card.
fn key(app: &App, session: &str) -> u64 {
    app.rail_cards()
        .and_then(|(cards, _)| {
            cards
                .iter()
                .find(|row| row.id.0 == session)
                .map(|row| row.key)
        })
        .unwrap_or_else(|| panic!("a card for {session}"))
}

/// The card number of `session`, if it is a card with one.
fn number(app: &App, session: &str) -> Option<usize> {
    let key = app.rail_cards().and_then(|(cards, _)| {
        cards
            .iter()
            .find(|row| row.id.0 == session)
            .map(|row| row.key)
    })?;
    app.rail_state().number(key)
}

/// A linked app on home with `sessions` live and idle, in order.
fn with(sessions: &[&str]) -> App {
    let mut app = home();
    linked(&mut app);
    for session in sessions {
        app.on_line(live(session, json!({})));
    }
    app
}

#[test]
fn a_new_row_takes_the_smallest_free_number() {
    let app = with(&[A, B, C]);
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
    assert_eq!(number(&app, C), Some(3));
}

#[test]
fn numbers_stay_through_waiting_and_back() {
    let mut app = with(&[A, B]);
    let waiting = json!({"state": "waiting", "waiting": {"request_id": "r_1",
        "kind": "approval", "summary": "shell ls"}});
    app.on_line(live(B, waiting));
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
    app.on_line(live(B, json!({})));
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
}

#[test]
fn an_exited_card_leaves_a_gap_until_a_new_session_fills_it() {
    let mut app = with(&[A, B, C]);
    app.on_line(left(B, "exited"));
    assert_eq!(number(&app, C), Some(3));
    app.on_line(live(D, json!({})));
    assert_eq!(number(&app, D), Some(2));
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, C), Some(3));
}

#[test]
fn an_exited_row_gives_up_its_number() {
    let mut app = with(&[A, B]);
    let a = key(&app, A);
    app.on_line(left(A, "exited"));
    assert_eq!(app.rail_state().number(a), None);
    assert_eq!(number(&app, B), Some(2));
}

#[test]
fn a_crashed_row_keeps_its_number() {
    let mut app = with(&[A, B]);
    app.on_line(left(A, "crashed"));
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
}

#[test]
fn a_recent_row_gets_no_number() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": recent, "result": {"sessions": [
            {"session_id": A, "project": "-w", "workspace": "/w", "name": "old",
             "how": "crashed"}]}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.rows.len() == 1)
    );
    assert_eq!(app.rail_cards().map(|(cards, _)| cards.len()), Some(0));
    app.on_line(live(B, json!({})));
    assert_eq!(number(&app, B), Some(1));
}

#[test]
fn numbers_are_kept_without_the_rail() {
    let app = with(&[A]);
    assert_eq!(number(&app, A), Some(1));
}

#[test]
fn no_home_no_numbers() {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(A.to_owned()));
    app.on_line(hello());
    app.on_line(live(A, json!({})));
    assert!(app.rail_cards().is_none());
    assert_eq!(app.rail_state().number(0), None);
}
