//! Tests for `/status` (#3013): it opens the status view over the
//! session on screen, drawn from what the terminal already holds.

use super::commands_testkit::{attached, connected, enter, session_line, table_lines};
use crate::app::Effect;
use serde_json::json;

#[test]
fn status_opens_the_status_view_and_sends_nothing() {
    let mut app = attached();
    assert_eq!(enter(&mut app, "/status"), Effect::None);
    assert_eq!(app.draft(), "");
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.title, "Status");
    assert_eq!(frame.command, "/status");
}

#[test]
fn status_draws_the_latest_status_and_credential_the_session_sent() {
    let mut app = attached();
    app.on_line(session_line(
        "preamble_built",
        json!({"reason": "start", "model": "acme/m1", "context_window": 200_000,
            "tool_choice": "auto", "cache_lifetime": "5m", "credential": "work",
            "system_prompt": "", "tools": []}),
    ));
    app.on_line(session_line(
        "session_status",
        json!({"name": "fix the parser", "workspace": "/w", "project": "-w",
            "model": "acme/m1", "state": "idle", "since": 0, "started": 0,
            "calls": 4, "turns": 2,
            "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {},
                "output": 2}, "cost": 0.25, "subscription_cost": 0.0},
            "delegates": 0, "jobs": 0, "clients": 1}),
    ));
    enter(&mut app, "/status");
    let rows: Vec<String> = app
        .session_view_screen(80)
        .expect("the view is open")
        .rows
        .iter()
        .map(|row| row.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect();
    assert_eq!(rows[0], "session      s_aaaaaaaaaaaaaaaa · fix the parser");
    assert_eq!(rows[2], "model        acme/m1");
    assert_eq!(rows[3], "credential   work");
    assert_eq!(rows[9], "turns        2 turns · 4 calls");
    // A credential switch is a `model_changed` naming the new label.
    app.on_line(session_line(
        "model_changed",
        json!({"before": {"model": "acme/m1", "cache_lifetime": "5m", "credential": "work"},
            "after": {"model": "acme/m1", "cache_lifetime": "5m", "credential": "home"},
            "source": "driver"}),
    ));
    let rows: Vec<String> = app
        .session_view_screen(80)
        .expect("the view is open")
        .rows
        .iter()
        .map(|row| row.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect();
    assert_eq!(rows[3], "credential   home");
}

#[test]
fn status_with_no_session_says_so_and_opens_nothing() {
    let mut app = connected();
    assert_eq!(enter(&mut app, "/status"), Effect::None);
    assert!(app.session_view_screen(80).is_none());
    assert_eq!(app.notice(), Some("No session on screen."));
}

#[test]
fn the_list_carries_the_status_row() {
    assert!(
        table_lines()
            .iter()
            .any(|line| line == "/status  Opens the status view.  command")
    );
}

/// The status view's rows on `app`, each row's cells joined.
fn rows(app: &crate::app::App) -> Vec<String> {
    app.session_view_screen(80)
        .expect("the view is open")
        .rows
        .iter()
        .map(|row| row.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect()
}

#[test]
fn status_names_the_hub_and_the_version_from_the_launch() {
    for (location, hub) in [
        (
            crate::LaunchLocation::Remote {
                hub: "work-box".to_owned(),
            },
            "work-box",
        ),
        (
            crate::LaunchLocation::Local {
                workspace: std::path::PathBuf::from("/w"),
                project: "-w".to_owned(),
                git: false,
            },
            "this machine",
        ),
    ] {
        let mut app = connected();
        app.set_home(crate::home::Launch {
            location,
            version: "0.4.0".to_owned(),
            ..Default::default()
        });
        app.attach(contract::SessionId(
            super::commands_testkit::SESSION.to_owned(),
        ));
        enter(&mut app, "/status");
        let rows = rows(&app);
        assert_eq!(rows[5], format!("hub          {hub}"));
        assert_eq!(rows[6], "version      0.4.0");
    }
}
