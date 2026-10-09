//! Tests for the Delegates card's motion: spinning state rows and the
//! asks for the frames they draw, read from the rows and the wake.

use std::path::PathBuf;

use fakes::clock::FakeClock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

use super::{ask, rows};
use crate::app::App;
use crate::home::Launch;
use crate::link::Line;
use crate::motion::SPINNER;
use crate::view::{render, text};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// An app with home state, attached, at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A running Fiber delegate `job`, described `description`.
fn delegate(app: &mut App, job: &str, description: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": description, "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": job, "delegate_session_id": format!("s_{job:0>16}"),
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// The card's rows as text at `text` columns.
fn lines(app: &App, text: usize) -> Vec<String> {
    rows(app, text)
        .into_iter()
        .map(|row| row.line.to_string())
        .collect()
}

/// Subscribes `job`'s delegate session in `state`.
fn subscribe(app: &mut App, job: &str, state: serde_json::Value) {
    let session = format!("s_{job:0>16}");
    let mut payload = json!({
        "name": "delegate", "workspace": "/w", "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        "parent": SESSION,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    app.on_line(Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }));
}

#[test]
fn delegates_card_spinner() {
    let clock = FakeClock::new();
    let mut app = attached(200, 40);
    delegate(&mut app, "j_1", "alpha");
    subscribe(&mut app, "j_1", json!({"state": "streaming"}));
    delegate(&mut app, "j_2", "beta");
    app.set_now(clock.origin(), 0);
    let shown = lines(&app, 97).join("\n");
    insta::assert_snapshot!("delegates_card_spinner", shown);
    assert!(shown.contains(SPINNER[0]), "{shown}");
}

#[test]
fn delegates_rows_spinner_narrow() {
    let clock = FakeClock::new();
    let mut app = attached(100, 30);
    delegate(&mut app, "j_1", "alpha");
    app.set_now(clock.origin(), 0);
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    insta::assert_snapshot!("delegates_rows_spinner_narrow", shown);
    assert!(shown.contains(SPINNER[0]), "{shown}");
}

#[test]
fn delegates_rows_collapsed_into_the_status_line_ask_nothing() {
    let clock = FakeClock::new();
    // At 11 rows the conversation would keep 5 of 11: the rows drop into
    // the status line.
    let mut app = attached(100, 11);
    app.on_line(session_line(
        "session_status",
        json!({
            "name": "one", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0}),
    ));
    delegate(&mut app, "j_1", "alpha");
    app.set_now(clock.origin(), 0);
    let area = Rect::new(0, 0, 100, 11);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    assert!(text(&buf).contains("1 delegate running"));
    assert_eq!(app.take_wake(), None);
}

#[test]
fn a_delegates_card_scrolled_out_of_the_panel_asks_nothing() {
    let clock = FakeClock::new();
    let mut app = attached(200, 40);
    // The Session card draws first, so the Delegates card's span starts
    // past it.
    app.on_line(session_line(
        "session_status",
        json!({
            "name": "one", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0}),
    ));
    delegate(&mut app, "j_1", "alpha");
    app.set_now(clock.origin(), 0);
    // A short panel ends before the Delegates card's span starts: the
    // drawn rows miss it, so no state row asks.
    let (_, span) = crate::view::panel::rows_and_delegates(&app, 200);
    let span = span.expect("a delegates span");
    assert!(span.start >= 1);
    let height = u16::try_from(span.start).unwrap_or(u16::MAX);
    let area = Rect::new(0, 0, 200, height);
    let mut buf = Buffer::empty(area);
    crate::view::panel::draw(&app, area, &mut buf, &mut Vec::new());
    assert_eq!(app.take_wake(), None);
}

#[test]
fn only_a_description_row_drawn_asks_nothing() {
    let clock = FakeClock::new();
    let mut app = attached(200, 40);
    delegate(&mut app, "j_1", "alpha");
    app.set_now(clock.origin(), 0);
    // The panel scrolled so only the delegate's description row shows.
    let (_, span) = crate::view::panel::rows_and_delegates(&app, 200);
    let span = span.expect("a delegates span");
    ask(&app, span.start + 1..span.start + 2);
    assert_eq!(app.take_wake(), None);
    // Its state row asks.
    ask(&app, span.start..span.start + 1);
    assert!(app.take_wake().is_some());
}

#[test]
fn delegates_ask_table() {
    let clock = FakeClock::new();
    // One spinning delegate (unsubscribed) and one still delegate
    // (subscribed idle): state rows 0 and 2, description rows 1 and 3.
    let mut app = attached(200, 40);
    delegate(&mut app, "j_1", "alpha");
    delegate(&mut app, "j_2", "beta");
    subscribe(&mut app, "j_2", json!({"state": "idle"}));
    app.set_now(clock.origin(), 0);
    for (drawn, asks) in [
        (0..0, false),
        (1..2, false),
        (0..1, true),
        (2..3, false),
        (0..4, true),
    ] {
        ask(&app, drawn.clone());
        assert_eq!(app.take_wake().is_some(), asks, "{drawn:?}");
    }
}

#[test]
fn counting_rows_for_the_fit_asks_nothing() {
    let clock = FakeClock::new();
    let mut app = attached(100, 30);
    delegate(&mut app, "j_1", "alpha");
    app.set_now(clock.origin(), 0);
    // Counting the rows for the fit builds them without drawing.
    let _ = app.narrow_fit();
    assert_eq!(app.take_wake(), None);
}
