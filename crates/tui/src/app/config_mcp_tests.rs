//! Tests for `/mcp` on the app: sending `mcp_servers` for the session on
//! screen, the answer filling the rows, Enter sending the per-server
//! driver command, and server events re-querying while quiet
//! (`docs/tui.md`, "Swapped views"; `docs/mcp.md`, "Servers, their state
//! and the per-server commands").

use std::path::PathBuf;
use std::sync::Arc;

use super::super::{App, Effect};
use crate::Configure;
use crate::app::testkit::{SESSION, now, sent, session_line};
use crate::configure::{Shown, WriteScope};
use crate::configure_fake::{Fake, row};
use crate::home::Launch;
use crate::keys::Key;

/// A seam over two rows.
fn fake() -> Arc<Fake> {
    Arc::new(Fake::new(vec![
        row(
            "handoff.tokens",
            Shown::Value("400000".to_owned()),
            "default",
            WriteScope::Any { repo: true },
        ),
        row(
            "tui.theme",
            Shown::Unset,
            "default",
            WriteScope::Any { repo: false },
        ),
    ]))
}

/// An app on home at 80x24 in `/w`, with `seam`.
fn home(seam: Option<Arc<Fake>>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_configure(seam.map(|seam| seam as Arc<dyn Configure>));
    app.set_size(80, 24);
    app
}

/// An app on home with the hub up and attached to [`SESSION`].
fn connected(seam: Option<Arc<Fake>>) -> App {
    let mut app = home(seam);
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Types `/mcp` and presses Enter.
fn slash_mcp(app: &mut App) -> Effect {
    for ch in "/mcp".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now())
}

/// The command lines an `on_line` answer sends, parsed.
fn sent_lines(lines: Vec<String>) -> Vec<serde_json::Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect()
}

/// Opens `/mcp` on a connected app and returns the sent command's id.
fn open_mcp(app: &mut App) -> String {
    let lines = sent(slash_mcp(app));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "mcp_servers");
    assert_eq!(lines[0]["session_id"], SESSION);
    lines[0]["id"].as_str().unwrap_or_default().to_owned()
}

/// An `mcp_servers` answer with a failed and a waiting server.
fn servers_answer() -> serde_json::Value {
    serde_json::json!({"servers": [
        {"name": "down", "layer": "project", "state": "failed",
         "reason": "The MCP server 'down' failed to start: refused.",
         "tools": 2, "approved": true, "auth_url": null},
        {"name": "remote", "layer": "global", "state": "needs-login",
         "reason": "The MCP server 'remote' failed to start: HTTP 401.",
         "tools": 0, "approved": true, "auth_url": null}]})
}

/// Answers the view's `mcp_servers` command `id`.
fn answer_servers(app: &mut App, id: &str) {
    app.on_line(session_line(
        SESSION,
        "command_accepted",
        serde_json::json!({"command_id": id, "result": servers_answer()}),
        Some("a_1"),
    ));
}

#[test]
fn without_a_seam_mcp_says_not_available() {
    let mut app = connected(None);
    assert_eq!(slash_mcp(&mut app), Effect::None);
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "MCP servers");
    assert_eq!(
        frame.below,
        vec!["Not available in this terminal.".to_owned()]
    );
}

#[test]
fn slash_mcp_sends_mcp_servers_for_the_session_on_screen() {
    let mut app = connected(Some(fake()));
    let id = open_mcp(&mut app);
    assert!(id.starts_with("c_"), "{id}");
    assert!(app.config_view_open());
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "MCP servers");
    assert_eq!(frame.command, "/mcp");
    assert!(
        frame
            .below
            .first()
            .is_some_and(|line| line == "Reading the servers…")
    );
}

#[test]
fn the_answer_to_the_sent_id_fills_the_rows() {
    let mut app = connected(Some(fake()));
    let id = open_mcp(&mut app);
    answer_servers(&mut app, &id);
    let frame = app.config_view_screen().expect("the view is open");
    let texts: Vec<String> = frame
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>()
        })
        .collect();
    assert_eq!(texts.len(), 3);
    assert!(texts[0].contains("down") && texts[0].contains("failed"));
    assert!(texts[1].contains("remote") && texts[1].contains("needs-login"));
    assert!(texts[2].contains("Per-tool on/off switches stay in `/tools`."));
}

#[test]
fn enter_on_failed_sends_mcp_reconnect_and_its_answer_requeries() {
    let mut app = connected(Some(fake()));
    let id = open_mcp(&mut app);
    answer_servers(&mut app, &id);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "mcp_reconnect");
    assert_eq!(lines[0]["args"]["server"], "down");
    let reconnect = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    // The reconnect's answer carries no result: its id clears the wait
    // and the rows query again.
    let lines = sent_lines(app.on_line(session_line(
        SESSION,
        "command_accepted",
        serde_json::json!({"command_id": reconnect}),
        Some("a_1"),
    )));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "mcp_servers");
}

#[test]
fn the_requery_after_a_reconnect_refills_the_rows() {
    // `expect` names the re-sent command: a mutant dropping it leaves
    // the rows stale, and the answered reconnect never shows.
    let mut app = connected(Some(fake()));
    let id = open_mcp(&mut app);
    answer_servers(&mut app, &id);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    let reconnect = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    let lines = sent_lines(app.on_line(session_line(
        SESSION,
        "command_accepted",
        serde_json::json!({"command_id": reconnect}),
        Some("a_1"),
    )));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "mcp_servers");
    let requery = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(session_line(
        SESSION,
        "command_accepted",
        serde_json::json!({"command_id": requery, "result": serde_json::json!({"servers": [
            {"name": "down", "layer": "project", "state": "ready",
             "reason": "", "tools": 2, "approved": true, "auth_url": null},
            {"name": "remote", "layer": "global", "state": "needs-login",
             "reason": "The MCP server 'remote' failed to start: HTTP 401.",
             "tools": 0, "approved": true, "auth_url": null}]})}),
        Some("a_1"),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    let texts: Vec<String> = frame
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>()
        })
        .collect();
    assert!(
        texts[0].contains("down") && texts[0].contains("ready"),
        "{texts:?}"
    );
}

#[test]
fn enter_on_needs_login_sends_mcp_login_and_a_rejection_says() {
    let mut app = connected(Some(fake()));
    let id = open_mcp(&mut app);
    answer_servers(&mut app, &id);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "mcp_login");
    assert_eq!(lines[0]["args"]["server"], "remote");
    let login = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(session_line(
        SESSION,
        "command_rejected",
        serde_json::json!({"command_id": login, "code": "busy", "message": "busy"}),
        Some("a_1"),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.below, vec!["busy".to_owned()]);
}

#[test]
fn a_server_event_requeries_while_quiet_but_not_mid_answer() {
    let mut app = connected(Some(fake()));
    let id = open_mcp(&mut app);
    // The answer is still in flight: the event changes nothing.
    let lines = sent_lines(app.on_line(session_line(
        SESSION,
        "mcp_server_ready",
        serde_json::json!({"server": "down"}),
        Some("a_1"),
    )));
    assert!(lines.is_empty());
    answer_servers(&mut app, &id);
    // Quiet now: the same event queries again.
    let lines = sent_lines(app.on_line(session_line(
        SESSION,
        "mcp_server_ready",
        serde_json::json!({"server": "down"}),
        Some("a_1"),
    )));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "mcp_servers");
}

#[test]
fn enter_on_unapproved_says_approve_and_sends_nothing() {
    let mut app = connected(Some(fake()));
    let id = open_mcp(&mut app);
    app.on_line(session_line(
        SESSION,
        "command_accepted",
        serde_json::json!({"command_id": id, "result": {"servers": [
            {"name": "repo", "layer": "repository", "state": "failed",
             "reason": "The MCP server 'repo' is not approved.",
             "tools": 0, "approved": false, "auth_url": null}]}}),
        Some("a_1"),
    ));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(
        frame.below,
        [
            "`repo` is declared by the repository but not approved; run `fiber approve` in the repository."
        ]
    );
}
