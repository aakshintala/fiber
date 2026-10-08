//! Tests for the panel's folded state: widgets and going home.

use super::super::App;
use crate::link::Line;
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

/// An app attached to `SESSION`.
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// One envelope of `session` at `ts`.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> Line {
    ts_line(session, kind, 0, payload)
}

/// One envelope of `session` at `ts`.
fn ts_line(session: &str, kind: &str, ts: u64, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An `extension_ui` widget's lines.
fn widget(extension: &str, widget: &str, lines: &[&str]) -> Line {
    session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": extension, "widget": widget, "lines": lines}),
    )
}

#[test]
fn the_latest_widget_lines_win_in_place() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    app.on_line(widget("other", "list", &["x"]));
    app.on_line(widget("plan", "tasks", &["one", "two"]));
    let widgets = app.panel_state().widgets();
    assert_eq!(widgets.len(), 2);
    assert_eq!(widgets[0].extension, "plan");
    assert_eq!(widgets[0].widget, "tasks");
    assert_eq!(widgets[0].lines, vec!["one".to_owned(), "two".to_owned()]);
    assert_eq!(widgets[1].extension, "other");
}

#[test]
fn empty_lines_remove_a_widget() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    app.on_line(widget("other", "list", &["x"]));
    app.on_line(widget("plan", "tasks", &[]));
    let widgets = app.panel_state().widgets();
    assert_eq!(widgets.len(), 1);
    assert_eq!(widgets[0].extension, "other");
}

#[test]
fn a_status_line_is_not_a_widget() {
    let mut app = attached();
    app.on_line(session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": "plan", "status": "working"}),
    ));
    assert!(app.panel_state().widgets().is_empty());
}

#[test]
fn going_home_clears_the_panel() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    assert!(!app.panel_state().widgets().is_empty());
    app.go_home();
    assert!(app.panel_state().widgets().is_empty());
}

#[test]
fn another_sessions_widget_is_not_folded() {
    let mut app = attached();
    app.on_line(session_line(
        OTHER,
        "extension_ui",
        serde_json::json!({"extension": "plan", "widget": "tasks", "lines": ["one"]}),
    ));
    assert!(app.panel_state().widgets().is_empty());
}

/// A `session_status` naming `model` in `workspace`.
fn status_line(workspace: &str, model: &str) -> Line {
    session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "work", "workspace": workspace, "project": "-w",
            "state": "idle", "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": model, "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// A `preamble_built` naming `model` with `thinking`, `window` and
/// `trigger`.
fn preamble_line(model: &str, thinking: &str, window: u64, trigger: u64) -> Line {
    session_line(
        SESSION,
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": model, "context_window": window,
            "trigger_at": trigger, "thinking": thinking,
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        }),
    )
}

/// A `model_changed` to `model` with `thinking`.
fn changed_line(model: &str, thinking: &str) -> Line {
    session_line(
        SESSION,
        "model_changed",
        serde_json::json!({
            "before": {"model": "old/model", "cache_lifetime": "5m"},
            "after": {"model": model, "thinking": thinking, "cache_lifetime": "5m"},
            "source": "driver",
        }),
    )
}

/// A `usage_recorded` of `output` tokens at `ts`.
fn usage_line(output: u64, ts: u64) -> Line {
    ts_line(
        SESSION,
        "usage_recorded",
        ts,
        serde_json::json!({
            "generation_id": "g_1", "model": "test/model",
            "tokens": {"input": 10, "cache_read": 0,
                "cache_write": {}, "output": output},
            "input_bytes": 0, "cost": 0.01,
        }),
    )
}

/// Replies at `ts` so a usage at `ts + span` spans `span`.
fn reply(app: &mut App, ts: u64) {
    app.on_line(ts_line(
        SESSION,
        "assistant_message_started",
        ts,
        serde_json::json!({}),
    ));
}

/// An `mcp_server_failed` line for `server`.
fn failed_line(server: &str) -> Line {
    session_line(
        SESSION,
        "mcp_server_failed",
        serde_json::json!({"server": server, "reason": "died",
            "will_restart": false,
            "error": {"code": "mcp_server_unavailable", "message": "died"}}),
    )
}

#[test]
fn model_thinking_window_and_trigger_come_from_the_latest_preamble() {
    let mut app = attached();
    app.on_line(preamble_line("one/model", "low", 1000, 800));
    app.on_line(preamble_line("two/model", "high", 2000, 1600));
    let panel = app.panel_state();
    assert_eq!(panel.model(), Some("two/model"));
    assert_eq!(panel.thinking(), Some("high"));
    assert_eq!(panel.window(), Some(2000));
    assert_eq!(panel.trigger_at(), Some(1600));
}

#[test]
fn model_changed_updates_model_and_thinking() {
    let mut app = attached();
    app.on_line(preamble_line("one/model", "low", 1000, 800));
    app.on_line(changed_line("two/model", "high"));
    let panel = app.panel_state();
    assert_eq!(panel.model(), Some("two/model"));
    assert_eq!(panel.thinking(), Some("high"));
}

#[test]
fn turns_count_turn_started() {
    let mut app = attached();
    assert_eq!(app.panel_state().turns(), 0);
    let started = session_line(SESSION, "turn_started", serde_json::json!({"input": []}));
    app.on_line(started.clone());
    app.on_line(started);
    assert_eq!(app.panel_state().turns(), 2);
}

#[test]
fn output_speed_divides_output_tokens_by_the_reply_span() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 3000));
    assert_eq!(app.panel_state().speed(), Some(250));
}

#[test]
fn a_zero_span_leaves_speed_out() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 1000));
    assert_eq!(app.panel_state().speed(), None);
}

#[test]
fn a_one_millisecond_span_counts() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 1001));
    assert_eq!(app.panel_state().speed(), Some(500_000));
}

#[test]
fn a_copied_usage_is_not_the_last_reply() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 3000));
    assert_eq!(app.panel_state().speed(), Some(250));
    app.on_line(ts_line(
        SESSION,
        "usage_recorded",
        9000,
        serde_json::json!({
            "generation_id": "g_2", "model": "test/model",
            "tokens": {"input": 10, "cache_read": 0,
                "cache_write": {}, "output": 9000},
            "input_bytes": 0, "cost": 0.01, "origin_session_id": "s_cccccccccccccccc",
        }),
    ));
    assert_eq!(app.panel_state().speed(), Some(250));
}

#[test]
fn an_extensions_usage_is_not_the_last_reply() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 3000));
    assert_eq!(app.panel_state().speed(), Some(250));
    app.on_line(ts_line(
        SESSION,
        "usage_recorded",
        9000,
        serde_json::json!({
            "generation_id": "g_3", "model": "test/model",
            "tokens": {"input": 10, "cache_read": 0,
                "cache_write": {}, "output": 9000},
            "input_bytes": 0, "cost": 0.01, "extension": "plan",
        }),
    ));
    assert_eq!(app.panel_state().speed(), Some(250));
}

#[test]
fn a_usage_before_any_message_start_leaves_speed_out() {
    let mut app = attached();
    app.on_line(usage_line(500, 3000));
    assert_eq!(app.panel_state().speed(), None);
}

#[test]
fn a_failed_server_is_down_until_ready() {
    let mut app = attached();
    app.on_line(failed_line("bravo"));
    app.on_line(failed_line("alpha"));
    assert_eq!(
        app.panel_state()
            .down()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["alpha", "bravo"]
    );
    app.on_line(session_line(
        SESSION,
        "mcp_server_ready",
        serde_json::json!({"server": "alpha"}),
    ));
    assert_eq!(
        app.panel_state()
            .down()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["bravo"]
    );
}

#[test]
fn the_latest_status_wins() {
    let mut app = attached();
    app.on_line(status_line("/one", "one/model"));
    app.on_line(status_line("/two", "two/model"));
    assert_eq!(
        app.panel_state()
            .status()
            .map(|status| status.workspace.as_str()),
        Some("/two")
    );
    assert_eq!(
        app.panel_state()
            .status()
            .map(|status| status.model.as_str()),
        Some("two/model")
    );
}
