//! Tests for the attention state on the app: an `attention` line queues
//! its bytes once, from home's settings.

use std::path::PathBuf;

use contract::clock::Clock;

use super::super::App;
use crate::Attention;
use crate::home::Launch;
use crate::link::Line;

/// An app with home state at 80x24, holding `attention`.
fn home(attention: Attention) -> App {
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
        attention,
    });
    app.set_size(80, 24);
    app
}

/// One waiting `attention` line for `s_aaaaaaaaaaaaaaaa`.
fn waiting() -> Line {
    Line::Hub(contract::HubLine {
        kind: "attention".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({
            "session_id": "s_aaaaaaaaaaaaaaaa",
            "name": "fix tests",
            "workspace": "/w",
            "reason": "waiting",
            "summary": "approval: Run cargo test",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn an_attention_line_queues_its_bytes_once() {
    let mut app = home(Attention::default());
    app.set_osc9(true);
    assert!(app.on_line(waiting()).is_empty());
    let body = "Fiber: fix tests needs you: approval: Run cargo test";
    let mut osc9 = format!("\x1b]9;{body}").into_bytes();
    osc9.push(0x07);
    assert_eq!(app.take_alerts(), osc9);
    assert_eq!(app.take_alerts(), Vec::<u8>::new());
}

#[test]
fn without_osc9_a_line_rings_the_bell() {
    let mut app = home(Attention::default());
    assert!(app.on_line(waiting()).is_empty());
    assert_eq!(app.take_alerts(), vec![0x07]);
}

#[test]
fn settings_come_from_home() {
    let off = Attention {
        notification: false,
        bell: false,
        title: true,
    };
    let mut app = home(off);
    assert!(app.on_line(waiting()).is_empty());
    assert_eq!(app.take_alerts(), Vec::<u8>::new());
    // Without home state the defaults ring the bell.
    let mut plain = App::new(PathBuf::from("/w"));
    plain.set_size(80, 24);
    assert!(plain.on_line(waiting()).is_empty());
    assert_eq!(plain.take_alerts(), vec![0x07]);
}

#[test]
fn a_line_that_does_not_parse_queues_nothing() {
    let mut app = home(Attention::default());
    let broken = Line::Hub(contract::HubLine {
        kind: "attention".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": "s_aaaaaaaaaaaaaaaa"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    assert!(app.on_line(broken).is_empty());
    assert_eq!(app.take_alerts(), Vec::<u8>::new());
}

/// A live `session_status` for `session` in `state`.
fn live(session: &str, state: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({
        "name": "fix tests",
        "workspace": "/w",
        "project": "-w",
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

/// A waiting `session_status` for `session` with `kind`.
fn waiting_row(session: &str, kind: &str) -> Line {
    live(
        session,
        serde_json::json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": kind, "summary": "Run cargo test"}}),
    )
}

/// An `attention` line for `session` with `reason` (`waiting` carries the
/// approval summary).
fn attention(session: &str, reason: &str) -> Line {
    let mut payload = serde_json::json!({
        "session_id": session,
        "name": "fix tests",
        "workspace": "/w",
        "reason": reason,
    });
    if reason == "waiting" {
        payload["summary"] = serde_json::json!("approval: Run cargo test");
    }
    Line::Hub(contract::HubLine {
        kind: "attention".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
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

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

#[test]
fn waiting_sets_bang_fiber_approval() {
    let mut app = home(Attention::default());
    app.on_line(waiting_row(SESSION, "approval"));
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "! fiber · approval");
}

#[test]
fn a_question_says_question() {
    let mut app = home(Attention::default());
    app.on_line(waiting_row(SESSION, "question"));
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "! fiber · question");
}

#[test]
fn an_offer_says_waiting() {
    let mut app = home(Attention::default());
    app.on_line(waiting_row(SESSION, "offer"));
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "! fiber · waiting");
}

#[test]
fn no_row_says_waiting() {
    let mut app = home(Attention::default());
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "! fiber · waiting");
}

#[test]
fn a_row_arriving_after_the_line_names_its_kind() {
    let mut app = home(Attention::default());
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "! fiber · waiting");
    app.on_line(waiting_row(SESSION, "approval"));
    assert_eq!(app.title(), "! fiber · approval");
}

#[test]
fn it_clears_when_the_row_stops_waiting() {
    let mut app = home(Attention::default());
    app.on_line(waiting_row(SESSION, "approval"));
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "! fiber · approval");
    app.on_line(live(SESSION, serde_json::json!({"state": "streaming"})));
    assert_eq!(app.title(), "fiber");
}

#[test]
fn it_clears_when_the_session_leaves() {
    let mut app = home(Attention::default());
    app.on_line(waiting_row(SESSION, "approval"));
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "! fiber · approval");
    // The row keeps `Waiting`, but it left.
    app.on_line(left(SESSION, "exited"));
    assert_eq!(app.title(), "fiber");
}

#[test]
fn finished_holds_until_a_stroke() {
    let mut app = home(Attention::default());
    app.on_line(attention(SESSION, "finished"));
    assert_eq!(app.title(), "✓ fiber · finished");
    // An unrelated hub line keeps it.
    app.on_line(live(OTHER, serde_json::json!({"state": "idle"})));
    assert_eq!(app.title(), "✓ fiber · finished");
    let stroke = crate::stroke::Stroke {
        code: crate::stroke::Code::Char('x'),
        mods: crate::stroke::Mods::NONE,
    };
    app.on_press(stroke, fakes::clock::FakeClock::new().now());
    assert_eq!(app.title(), "fiber");
}

#[test]
fn a_stroke_keeps_a_waiting_title() {
    let mut app = home(Attention::default());
    app.on_line(waiting_row(SESSION, "approval"));
    app.on_line(attention(SESSION, "waiting"));
    let stroke = crate::stroke::Stroke {
        code: crate::stroke::Code::Char('x'),
        mods: crate::stroke::Mods::NONE,
    };
    app.on_press(stroke, fakes::clock::FakeClock::new().now());
    assert_eq!(app.title(), "! fiber · approval");
}

#[test]
fn the_latest_attention_wins() {
    let mut app = home(Attention::default());
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "! fiber · waiting");
    app.on_line(attention(OTHER, "finished"));
    assert_eq!(app.title(), "✓ fiber · finished");
    app.on_line(attention(OTHER, "waiting"));
    assert_eq!(app.title(), "! fiber · waiting");
}

#[test]
fn title_off_keeps_the_normal_title() {
    let mut app = home(Attention {
        title: false,
        ..Attention::default()
    });
    app.on_line(waiting_row(SESSION, "approval"));
    app.on_line(attention(SESSION, "waiting"));
    assert_eq!(app.title(), "fiber");
    assert!(!app.take_alerts().is_empty());
}
