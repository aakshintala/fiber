//! Tests for the rail's motion: spinning glyphs, the waiting pulse and
//! the asks for the frames they draw, read from the drawn buffer.

use std::path::PathBuf;
use std::time::Duration;

use fakes::clock::FakeClock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::{draw, pulse};
use crate::app::App;
use crate::home::Launch;
use crate::keys::{Mouse, MouseKind};
use crate::link::Line;
use crate::markdown::Role;
use crate::motion::SPINNER;
use crate::view::text;
use contract::SessionId;
use ratatui::style::Modifier;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
/// The wall time every test sets: 2023-11-14T22:13:20Z.
const WALL: u64 = 1_700_000_000_000;

/// An app with home state at 200x40, attached to `A`.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: ["session", "changed_files", "delegates", "jobs", "quota"]
            .map(str::to_owned)
            .to_vec(),
        ..Default::default()
    });
    app.attach(SessionId(A.to_owned()));
    app.set_size(200, 40);
    app.set_wall(WALL);
    app
}

/// A `session_status` for `session` in `state` since `since`.
fn status(session: &str, state: Value, since: u64) -> Line {
    let mut payload = json!({
        "name": "work", "workspace": "/w", "project": "-w", "since": since,
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
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A waiting `session_status` for `session` since `since`.
fn waiting(session: &str, since: u64) -> Line {
    status(
        session,
        json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "shell"}}),
        since,
    )
}

/// The rail rect drawn alone at its own width, 40 rows.
fn rail(app: &App) -> Buffer {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.rail)
        .unwrap_or_else(|| panic!("a rail"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    draw(app, area, &mut buf, None, &mut Vec::new());
    buf
}

/// The style of the first card's glyph cell and its stripe cell: the
/// `!` on the first text row, and the `▌` opening it.
fn glyph_and_stripe(buf: &Buffer) -> (ratatui::style::Style, ratatui::style::Style) {
    let glyph = (0..buf.area.width)
        .filter_map(|x| buf.cell((x, 2)))
        .find(|cell| cell.symbol() == "!")
        .unwrap_or_else(|| panic!("no glyph cell"));
    let stripe = buf.cell((0, 2)).unwrap_or_else(|| panic!("no stripe cell"));
    assert_eq!(stripe.symbol(), "▌");
    (glyph.style(), stripe.style())
}

#[test]
fn rail_card_spinner() {
    let clock = FakeClock::new();
    let mut app = app();
    app.on_line(status(A, json!({"state": "streaming"}), WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    app.set_now(clock.origin(), WALL);
    insta::assert_snapshot!("rail_card_spinner", text(&rail(&app)));
    assert!(text(&rail(&app)).contains(SPINNER[0]));
    assert!(app.take_wake().is_some());
}

#[test]
fn rail_card_retrying_spinner() {
    let clock = FakeClock::new();
    let mut app = app();
    app.on_line(status(A, json!({"state": "retrying"}), WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    app.set_now(clock.origin(), WALL);
    insta::assert_snapshot!("rail_card_retrying_spinner", text(&rail(&app)));
    assert!(text(&rail(&app)).contains(SPINNER[0]));
}

#[test]
fn rail_card_pulse_bright() {
    let clock = FakeClock::new();
    let mut app = app();
    app.on_line(waiting(A, WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    app.set_now(clock.origin(), WALL);
    insta::assert_snapshot!("rail_card_pulse_bright", text(&rail(&app)));
    let (glyph, stripe) = glyph_and_stripe(&rail(&app));
    assert_eq!(glyph.fg, Some(Role::Attention.color()));
    assert_eq!(stripe.fg, Some(Role::Attention.color()));
    assert!(app.take_wake().is_some());
}

#[test]
fn a_crashed_waiting_card_does_not_pulse() {
    let clock = FakeClock::new();
    let mut app = app();
    app.on_line(waiting(A, WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    app.set_now(clock.origin(), WALL);
    let mut card = app
        .rail_cards()
        .and_then(|(cards, _)| cards.into_iter().next())
        .expect("a rail card")
        .clone();
    card.left = Some(crate::home::Left::Crashed);
    // The row is Waiting but already left; either fact alone cannot make
    // the rail's live-card pulse branch true.
    assert_eq!(pulse(&app, &card), None);
}

#[test]
fn rail_card_pulse_dim() {
    let clock = FakeClock::new();
    let origin = clock.origin();
    let mut app = app();
    app.on_line(waiting(A, WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    app.set_now(origin, WALL);
    app.set_now(
        origin
            .checked_add(Duration::from_millis(480))
            .expect("after the origin"),
        WALL + 480,
    );
    insta::assert_snapshot!("rail_card_pulse_dim", text(&rail(&app)));
    let (glyph, stripe) = glyph_and_stripe(&rail(&app));
    assert!(glyph.add_modifier.contains(Modifier::DIM));
    assert!(stripe.add_modifier.contains(Modifier::DIM));
}

#[test]
fn rail_card_after_ten_seconds_holds_still() {
    let clock = FakeClock::new();
    let origin = clock.origin();
    let mut app = app();
    app.on_line(waiting(A, WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    app.set_now(origin, WALL);
    app.set_now(
        origin
            .checked_add(Duration::from_millis(10_000))
            .expect("after the origin"),
        WALL + 10_000,
    );
    insta::assert_snapshot!("rail_card_after_ten_seconds_holds_still", text(&rail(&app)));
    let (glyph, stripe) = glyph_and_stripe(&rail(&app));
    assert_eq!(glyph.fg, Some(Role::Attention.color()));
    assert!(!glyph.add_modifier.contains(Modifier::DIM));
    assert_eq!(stripe.fg, Some(Role::Attention.color()));
    assert_eq!(app.take_wake(), None);
}

#[test]
fn rail_card_reduced_never_pulses() {
    let clock = FakeClock::new();
    let mut app = app();
    app.on_line(waiting(A, WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    app.set_reduced_motion(true);
    app.set_now(clock.origin(), WALL);
    insta::assert_snapshot!("rail_card_reduced_never_pulses", text(&rail(&app)));
    assert_eq!(app.take_wake(), None);
}

/// Two cards at 200x10, wheeled down `times` over the rail.
fn scrolled(times: usize) -> App {
    let mut app = app();
    app.set_size(200, 10);
    app.on_line(status(A, json!({"state": "streaming"}), WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    for _ in 0..times {
        app.on_wheel(&Mouse {
            kind: MouseKind::WheelDown,
            col: 5,
            row: 5,
        });
    }
    app
}

#[test]
fn a_card_scrolled_to_its_edge_asks_nothing() {
    let clock = FakeClock::new();
    // Two wheels down at 200x10: the working card scrolls wholly above.
    let mut app = scrolled(2);
    app.set_now(clock.origin(), WALL);
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, None, &mut Vec::new());
    assert_eq!(app.take_wake(), None);
}

#[test]
fn a_jobs_card_asks_nothing() {
    let clock = FakeClock::new();
    let mut app = app();
    app.on_line(status(A, json!({"state": "jobs"}), WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    app.set_now(clock.origin(), WALL);
    let _ = rail(&app);
    assert_eq!(app.take_wake(), None);
}

#[test]
fn a_card_clipped_above_its_glyph_row_asks_no_spin() {
    let clock = FakeClock::new();
    // Two wheels down at 200x10 stops mid-card: rows 2-4 show while the
    // glyph row does not.
    let mut app = scrolled(2);
    app.set_now(clock.origin(), WALL);
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, None, &mut Vec::new());
    assert_eq!(app.take_wake(), None);
}

#[test]
fn a_waiting_card_clipped_above_its_glyph_row_still_pulses() {
    let clock = FakeClock::new();
    let mut app = app();
    app.set_size(200, 10);
    app.on_line(waiting(A, WALL));
    app.on_line(status(B, json!({"state": "idle"}), WALL));
    for _ in 0..2 {
        app.on_wheel(&Mouse {
            kind: MouseKind::WheelDown,
            col: 5,
            row: 5,
        });
    }
    app.set_now(clock.origin(), WALL);
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, None, &mut Vec::new());
    // The spin ask is clipped with the glyph row; the pulse still asks.
    assert!(app.take_wake().is_some());
}
