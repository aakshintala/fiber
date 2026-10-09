//! Tests for home's motion: spinning session rows and the asks for the
//! frames they draw, read from the drawn buffer.

use std::path::PathBuf;

use fakes::clock::FakeClock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

use crate::app::App;
use crate::home::Launch;
use crate::link::Line;
use crate::motion::SPINNER;
use crate::view::{render, text};

const WALL: u64 = 1_700_000_000_000;

/// An app on home at `width` by `height`.
fn home(width: u16, height: u16) -> App {
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
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(width, height);
    app
}

/// A `session_status` for `session` in `state`.
fn status(session: &str, state: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": "work", "workspace": "/w", "project": "-w",
            "state": state, "since": WALL,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0,
            "clients": 0})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// The drawn screen's text at `width` by `height`.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    text(&buf)
}

#[test]
fn home_row_spinner() {
    let clock = FakeClock::new();
    let mut app = home(80, 24);
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "streaming"));
    app.set_now(clock.origin(), WALL);
    insta::assert_snapshot!("home_row_spinner", screen(&app, 80, 24));
    assert!(screen(&app, 80, 24).contains(SPINNER[0]));
    assert!(app.take_wake().is_some());
}

#[test]
fn a_home_row_below_the_foot_asks_nothing() {
    let clock = FakeClock::new();
    let mut app = home(80, 10);
    // Three idle rows fill the list; the working row lands below the
    // foot, where it draws nothing and asks for nothing.
    for session in [
        "s_aaaaaaaaaaaaaaaa",
        "s_bbbbbbbbbbbbbbbb",
        "s_cccccccccccccccc",
    ] {
        app.on_line(status(session, "idle"));
    }
    app.on_line(status("s_dddddddddddddddd", "streaming"));
    app.set_now(clock.origin(), WALL);
    let shown = screen(&app, 80, 10);
    assert!(!shown.contains(SPINNER[0]), "{shown}");
    assert_eq!(app.take_wake(), None);
}
