//! Tests for the attention state on the app: an `attention` line queues
//! its bytes once, from home's settings.

use std::path::PathBuf;

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
