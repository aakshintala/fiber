//! Tests for the panel's card order and widget cards.

use super::{cards, rows};
use crate::app::App;
use crate::home::Launch;
use crate::link::Line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;

const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// The default card list.
fn list(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// An app with home state, attached, at `width` by `height`, drawing the
/// default card list.
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
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.set_size(width, height);
    app
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    timed_line(kind, 0, payload)
}

/// One envelope of the attached session at `ts`.
fn timed_line(kind: &str, ts: u64, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Folds a widget of `lines` into `app`.
fn fold_widget(app: &mut App, extension: &str, widget: &str, lines: &[&str]) {
    app.on_line(session_line(
        "extension_ui",
        serde_json::json!({"extension": extension, "widget": widget, "lines": lines}),
    ));
}

/// A `session_status` with `spend` and `context`.
fn status_line(spend: serde_json::Value, context: Option<serde_json::Value>) -> Line {
    let mut payload = serde_json::json!({
        "name": "work", "workspace": "/Users/you/work/fiber", "project": "-w",
        "state": "idle", "since": 0, "spend": spend,
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    if let Some(context) = context {
        payload["context"] = context;
    }
    session_line("session_status", payload)
}

/// A spend with `input`, `read`, `written`, `output`, billed `cost` and
/// `subscription` cost.
fn spend(
    input: u64,
    read: u64,
    written: u64,
    output: u64,
    cost: serde_json::Value,
    subscription: f64,
) -> serde_json::Value {
    serde_json::json!({"tokens": {"input": input, "cache_read": read,
        "cache_write": {"5m": written}, "output": output},
        "cost": cost, "subscription_cost": subscription})
}

/// A `preamble_built` with `trigger`: `None` leaves automatic handoff
/// off.
fn preamble_line(trigger: Option<u64>) -> Line {
    let mut payload = serde_json::json!({
        "reason": "start", "model": "test/model", "context_window": 1000,
        "thinking": "high", "tool_choice": "auto", "cache_lifetime": "5m",
        "system_prompt": "", "tools": [],
    });
    if let Some(trigger) = trigger {
        payload["trigger_at"] = trigger.into();
    }
    session_line("preamble_built", payload)
}

/// The drawn rows' text at `width`.
fn texts(app: &App, width: u16) -> Vec<String> {
    rows(app, width)
        .iter()
        .map(|row| row.line.to_string())
        .collect()
}

#[test]
fn cache_hits_need_input() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(0, 0, 0, 5, serde_json::json!(0.0), 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .all(|row| !row.starts_with("cache hits"))
    );
    app.on_line(status_line(
        spend(1, 0, 0, 5, serde_json::json!(0.0), 0.0),
        None,
    ));
    assert!(texts(&app, 40).iter().any(|row| row == "cache hits  0%"));
}

#[test]
fn pct_and_bar_floor() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(109, 10, 0, 5, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 119, "window": 1000})),
    ));
    let drawn = texts(&app, 40);
    assert!(drawn.iter().any(|row| row == "context  11% of 1.0k tokens"));
    let bar = "▆▆▆▆".to_owned() + &"░".repeat(25) + "│" + &"░".repeat(7);
    assert!(drawn.iter().any(|row| row == &bar));
}

#[test]
fn context_rows_need_a_window_and_a_status() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    assert!(
        texts(&app, 40)
            .iter()
            .all(|row| !row.starts_with("context") && !row.contains('▆'))
    );
}

#[test]
fn a_full_context_fills_every_cell() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(None));
    app.on_line(status_line(
        spend(1000, 0, 0, 0, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 1000, "window": 1000})),
    ));
    let drawn = texts(&app, 40);
    assert!(drawn.iter().any(|row| row == &"▆".repeat(37)));
}

#[test]
fn the_marker_at_the_window_sits_on_the_last_cell() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(1000)));
    app.on_line(status_line(
        spend(500, 0, 0, 0, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    let drawn = texts(&app, 40);
    let bar = "▆".repeat(18) + &"░".repeat(18) + "│";
    assert!(drawn.iter().any(|row| row == &bar));
}

#[test]
fn cost_rows() {
    for (cost, subscription, billed, on_subscription) in [
        (serde_json::json!(0.0), 0.0, false, false),
        (serde_json::json!(0.01), 0.0, true, false),
        (serde_json::json!(0.41), 1.10, true, true),
        (serde_json::Value::Null, 0.0, true, false),
    ] {
        let mut app = attached(160, 40);
        app.on_line(status_line(spend(1, 0, 0, 2, cost, subscription), None));
        let drawn = texts(&app, 40);
        assert_eq!(
            drawn.iter().any(|row| row.starts_with("cost billed")),
            billed,
            "{drawn:?}"
        );
        assert_eq!(
            drawn
                .iter()
                .any(|row| row.starts_with("cost on subscription")),
            on_subscription,
            "{drawn:?}"
        );
    }
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.01), 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| row == "cost billed  $0.01")
    );
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::Value::Null, 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| row == "cost billed  unknown")
    );
}

#[test]
fn turns_at_zero_are_left_out() {
    let mut app = attached(160, 40);
    assert!(rows(&app, 40).is_empty());
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": []}),
    ));
    assert!(texts(&app, 40).iter().any(|row| row == "turns  1"));
}

#[test]
fn the_model_falls_back_to_the_status() {
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 0.0),
        None,
    ));
    assert!(texts(&app, 40).iter().any(|row| row == "model  test/model"));
    app.on_line(preamble_line(Some(800)));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| row == "model  test/model · thinking high")
    );
}

/// Folds a session with every row present: directory, model and thinking,
/// context with its bar, marker and handoff, tokens, cache hits, both
/// costs, speed, turns and a server down.
fn full_session(app: &mut App) {
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(900, 100, 50, 200, serde_json::json!(0.41), 1.10),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": []}),
    ));
    app.on_line(timed_line(
        "assistant_message_started",
        1000,
        serde_json::json!({}),
    ));
    app.on_line(timed_line(
        "usage_recorded",
        3000,
        serde_json::json!({
            "generation_id": "g_1", "model": "test/model",
            "tokens": {"input": 10, "cache_read": 0,
                "cache_write": {}, "output": 500},
            "input_bytes": 0, "cost": 0.01,
        }),
    ));
    app.on_line(session_line(
        "mcp_server_failed",
        serde_json::json!({"server": "relay", "reason": "died",
            "will_restart": false,
            "error": {"code": "mcp_server_unavailable", "message": "died"}}),
    ));
}

#[test]
fn session_card_full() {
    let mut app = attached(160, 40);
    full_session(&mut app);
    insta::assert_snapshot!("session_card_full", super::super::text(&draw_panel(&app)));
}

#[test]
fn session_card_without_trigger() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(None));
    app.on_line(status_line(
        spend(900, 100, 50, 200, serde_json::json!(0.41), 0.0),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    insta::assert_snapshot!(
        "session_card_without_trigger",
        super::super::text(&draw_panel(&app))
    );
}

#[test]
fn context_bar_marker_at_the_trigger() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(500, 0, 0, 0, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    insta::assert_snapshot!(
        "context_bar_marker_at_the_trigger",
        super::super::text(&draw_panel(&app))
    );
}

#[test]
fn session_card_at_the_panel_floor() {
    let mut app = attached(140, 24);
    full_session(&mut app);
    insta::assert_snapshot!(
        "session_card_at_the_panel_floor",
        super::super::text(&draw_panel(&app))
    );
}

#[test]
fn cards_in_configured_order() {
    let mut app = attached(160, 40);
    full_session(&mut app);
    fold_widget(&mut app, "plan", "tasks", &["one"]);
    insta::assert_snapshot!(
        "cards_in_configured_order",
        super::super::text(&draw_panel(&app))
    );
}

#[test]
fn cards_follow_the_list() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&["plan/tasks", "other/list"]), &widgets),
        vec![super::Card::Widget(0), super::Card::Widget(1)]
    );
    assert_eq!(
        cards(&list(&["other/list", "plan/tasks"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
    assert_eq!(
        cards(&list(&["session"]), &widgets),
        vec![
            super::Card::Session,
            super::Card::Widget(0),
            super::Card::Widget(1)
        ]
    );
    assert_eq!(
        cards(&list(&["session", "session"]), &widgets),
        vec![
            super::Card::Session,
            super::Card::Widget(0),
            super::Card::Widget(1)
        ]
    );
}

#[test]
fn a_listed_widget_takes_its_listed_place() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
}

#[test]
fn an_unlisted_widget_shows_after_the_listed_cards_in_arrival_order() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&[]), &widgets),
        vec![super::Card::Widget(0), super::Card::Widget(1)]
    );
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
}

#[test]
fn quota_delegates_and_unknown_names_place_nothing() {
    let none: [(&str, &str); 0] = [];
    assert!(cards(&list(&["quota"]), &none).is_empty());
    assert!(cards(&list(&["delegates"]), &none).is_empty());
    assert!(cards(&list(&["nope"]), &none).is_empty());
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(
            &list(&["quota", "delegates", "nope", "plan/tasks"]),
            &widgets
        ),
        vec![super::Card::Widget(0)]
    );
}

#[test]
fn a_listed_widget_that_is_absent_places_nothing() {
    let none: [(&str, &str); 0] = [];
    assert!(cards(&list(&["other/list"]), &none).is_empty());
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(0)]
    );
}

#[test]
fn a_name_listed_twice_places_one_card() {
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["plan/tasks", "plan/tasks"]), &widgets),
        vec![super::Card::Widget(0)]
    );
}

/// Draws only the panel rect of `app`.
fn draw_panel(app: &App) -> Buffer {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let mut buf = Buffer::empty(Rect::new(0, 0, rect.width, rect.height));
    let mut targets = Vec::new();
    super::draw(
        app,
        Rect::new(0, 0, rect.width, rect.height),
        &mut buf,
        &mut targets,
    );
    assert!(targets.is_empty());
    buf
}

#[test]
fn widget_card() {
    let mut app = attached(160, 40);
    fold_widget(&mut app, "plan", "tasks", &["one", "two", "three"]);
    let buf = draw_panel(&app);
    insta::assert_snapshot!("widget_card", super::super::text(&buf));
}

#[test]
fn widget_rows_are_cut_at_the_text_width() {
    let mut app = attached(160, 40);
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let text = usize::from(rect.width).saturating_sub(3);
    fold_widget(&mut app, "plan", "tasks", &["x".repeat(text + 10).as_str()]);
    let drawn: Vec<String> = rows(&app, rect.width)
        .iter()
        .map(|row| row.line.to_string())
        .collect();
    assert_eq!(drawn.len(), 2);
    assert!(drawn.iter().all(|row| crate::format::width(row) <= text));
}
