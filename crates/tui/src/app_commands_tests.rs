//! Tests for the completion panels, the built-in commands and the key map.

use crate::app::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Instant;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// An app in `/w`, connected to the hub, with no session.
fn connected() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    app
}

/// A connected app attached to [`SESSION`].
fn attached() -> App {
    let mut app = connected();
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Types `text`, checking each key does nothing else.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
}

/// Types `text` and presses Enter.
fn enter(app: &mut App, text: &str) -> Effect {
    type_text(app, text);
    app.on_key(Key::Enter, now())
}

/// The command lines an effect sends, parsed.
fn sent(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None | Effect::Quit => Vec::new(),
    }
}

/// One session envelope from [`SESSION`].
fn session_line(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An `opening_message` listing skills named `names`.
fn opening(names: &[&str]) -> Line {
    let skills: Vec<Value> = names
        .iter()
        .map(|name| {
            json!({"name": name, "description": format!("Runs {name}."),
                "path": format!("/s/{name}/SKILL.md"), "source": "repository"})
        })
        .collect();
    session_line(
        "opening_message",
        json!({
            "environment": {"date": "2026-10-06", "os": "macos", "arch": "aarch64",
                "shell": "zsh", "workspace": "/w", "session_log": "/l"},
            "instruction_files": [],
            "skills": skills,
        }),
    )
}

/// A turn starts on [`SESSION`].
fn turn_starts(app: &mut App) {
    let line = session_line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]}),
    );
    assert!(app.on_line(line).is_empty());
}

/// The completion panel's rows.
fn shown(app: &App) -> Vec<String> {
    app.completions()
        .map(|completions| completions.lines)
        .unwrap_or_default()
}

/// The selected completion row's text.
fn selected(app: &App) -> Option<String> {
    let completions = app.completions()?;
    completions.lines.get(completions.selected?).cloned()
}

#[test]
fn slash_alone_shows_every_row_from_the_top() {
    let mut app = connected();
    assert_eq!(app.completions(), None);
    type_text(&mut app, "/");
    let rows = shown(&app);
    assert_eq!(rows.len(), 8);
    assert_eq!(
        rows.first().map(String::as_str),
        Some("/home  Goes home.  command")
    );
    assert_eq!(selected(&app), rows.first().cloned());
}

#[test]
fn typing_filters_prefix_matches_first() {
    let mut app = attached();
    assert!(app.on_line(opening(&["tdd", "areview"])).is_empty());
    type_text(&mut app, "/re");
    assert_eq!(
        shown(&app),
        [
            "/reload  Reloads configuration, MCP servers and extensions.  command",
            "/areview  Runs areview.  skill",
        ]
    );
    type_text(&mut app, "zz");
    assert_eq!(app.completions(), None);
}

#[test]
fn a_skill_from_the_opening_message_is_a_row_and_runs_as_a_prompt() {
    let mut app = attached();
    assert!(app.on_line(opening(&["tdd"])).is_empty());
    type_text(&mut app, "/td");
    assert_eq!(shown(&app), ["/tdd  Runs tdd.  skill"]);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/tdd");
}

#[test]
fn another_sessions_opening_message_is_ignored() {
    let mut app = attached();
    let mut line = opening(&["tdd"]);
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    }
    assert!(app.on_line(line).is_empty());
    type_text(&mut app, "/td");
    assert_eq!(app.completions(), None);
}

#[test]
fn up_and_down_move_the_selection_clamped_and_scroll_the_window() {
    let mut app = connected();
    type_text(&mut app, "/");
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(
        selected(&app).as_deref(),
        Some("/home  Goes home.  command")
    );
    for _ in 0..7 {
        app.on_key(Key::Down, now());
    }
    // The eighth row, still in the first window.
    assert_eq!(
        selected(&app).as_deref(),
        Some("/?  Opens the key map.  command")
    );
    assert_eq!(app.completions().and_then(|c| c.selected), Some(7));
    app.on_key(Key::Down, now());
    // The ninth and last row: the window moves down by one.
    let completions = app.completions();
    assert_eq!(completions.as_ref().and_then(|c| c.selected), Some(7));
    assert_eq!(
        completions
            .and_then(|c| c.lines.first().cloned())
            .as_deref(),
        Some("/new  Goes home with the cursor in the input box.  command")
    );
    app.on_key(Key::Down, now());
    assert_eq!(
        selected(&app).as_deref(),
        Some("/help  Opens the key map.  command")
    );
    app.on_key(Key::Up, now());
    assert_eq!(
        selected(&app).as_deref(),
        Some("/?  Opens the key map.  command")
    );
    // The draft is untouched by moving.
    assert_eq!(app.draft(), "/");
}

#[test]
fn tab_completes_the_selected_name_and_closes_the_panel() {
    let mut app = connected();
    type_text(&mut app, "/han");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "/handoff ");
    assert_eq!(app.completions(), None);
}

#[test]
fn tab_with_no_panel_and_no_match_does_nothing() {
    let mut app = connected();
    type_text(&mut app, "hi");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert_eq!(app.draft(), "hi");
    let mut app = connected();
    type_text(&mut app, "/zz");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "/zz");
}

#[test]
fn enter_runs_the_selected_row() {
    let mut app = attached();
    type_text(&mut app, "/rel");
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "reload");
    assert_eq!(app.draft(), "");
}

#[test]
fn handoff_sends_its_instructions() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/handoff  do x "));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "handoff");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"instructions": "do x"}));
    assert_eq!(app.draft(), "");
    let lines = sent(enter(&mut app, "/handoff"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "handoff");
    assert_eq!(lines[0].get("args"), None);
}

#[test]
fn reload_sends_reload_without_args() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/reload"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "reload");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0].get("args"), None);
}

#[test]
fn a_rejected_handoff_returns_its_draft_with_the_notice() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/handoff do x"));
    let id = lines[0]["id"].clone();
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": id, "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "/handoff do x");
    assert_eq!(app.notice(), Some("Busy."));
}

#[test]
fn a_command_with_no_session_says_so() {
    for command in ["/handoff x", "/reload", "/close"] {
        let mut app = connected();
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.notice(), Some("No session on screen."), "{command}");
        assert_eq!(app.draft(), "", "{command}");
    }
}

#[test]
fn a_command_with_the_link_down_keeps_its_draft() {
    for command in ["/handoff x", "/reload", "/close"] {
        let mut app = attached();
        app.disconnected();
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.draft(), command, "{command}");
        assert!(app.session().is_some(), "{command}");
    }
}

#[test]
fn close_while_idle_sends_close_and_goes_home() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/close"));
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(commands, [&json!("close")]);
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(app.session(), None);
}

#[test]
fn close_during_a_turn_cancels_first() {
    let mut app = attached();
    turn_starts(&mut app);
    let lines = sent(enter(&mut app, "/close"));
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(commands, [&json!("cancel"), &json!("close")]);
    assert!(lines.iter().all(|line| line["session_id"] == SESSION));
    assert_eq!(app.session(), None);
    assert!(app.lines().is_empty());
}

#[test]
fn quit_quits() {
    let mut app = connected();
    assert_eq!(enter(&mut app, "/quit"), Effect::Quit);
}

#[test]
fn home_and_new_return_to_the_screen_before_a_session() {
    for command in ["/home", "/new"] {
        let mut app = attached();
        assert!(app.on_line(opening(&["tdd"])).is_empty());
        turn_starts(&mut app);
        app.set_size(80, 3);
        app.on_key(Key::PageUp, now());
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.session(), None, "{command}");
        assert!(app.lines().is_empty(), "{command}");
        assert_eq!(app.top(), None, "{command}");
        assert_eq!(app.draft(), "", "{command}");
        // The old session's skills are gone with it.
        type_text(&mut app, "/td");
        assert_eq!(app.completions(), None, "{command}");
        app.on_key(Key::Esc, now());
        app.on_key(Key::CtrlC, now());
        // The next Enter starts a new session.
        let lines = sent(enter(&mut app, "hi"));
        assert_eq!(lines.len(), 1, "{command}");
        assert_eq!(lines[0]["command"], "start", "{command}");
    }
}

#[test]
fn home_while_start_is_pending_only_clears_the_draft() {
    let mut app = connected();
    let lines = sent(enter(&mut app, "hi"));
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(enter(&mut app, "/home"), Effect::None);
    assert_eq!(app.draft(), "");
    // Still waiting on that `start`: Enter sends nothing and keeps the
    // draft, where the screen before a session would send `start`.
    assert_eq!(enter(&mut app, "again"), Effect::None);
    assert_eq!(app.draft(), "again");
}

#[test]
fn an_unknown_command_is_sent_as_written() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/foo bar"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/foo bar");
    turn_starts(&mut app);
    let lines = sent(enter(&mut app, "/foo"));
    assert_eq!(lines[0]["command"], "steer");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/foo");
}

#[test]
fn esc_closes_the_slash_panel_and_keeps_the_draft_until_it_is_emptied() {
    let mut app = attached();
    turn_starts(&mut app);
    type_text(&mut app, "/re");
    // Esc closes the panel; it does not interrupt the turn.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.draft(), "/re");
    assert_eq!(app.completions(), None);
    // Typing again does not reopen it.
    type_text(&mut app, "l");
    assert_eq!(app.completions(), None);
    // Backspace back to `/` keeps it closed; to empty, then `/`, reopens.
    for _ in 0..3 {
        app.on_key(Key::Backspace, now());
    }
    assert_eq!(app.draft(), "/");
    assert_eq!(app.completions(), None);
    app.on_key(Key::Backspace, now());
    type_text(&mut app, "/");
    assert_eq!(shown(&app).len(), 8);
    // A draft that stops starting with `/` also reopens it after Esc.
    app.on_key(Key::Esc, now());
    app.on_key(Key::CtrlC, now());
    type_text(&mut app, "/");
    assert_eq!(shown(&app).len(), 8);
}

#[test]
fn a_space_closes_the_slash_panel() {
    let mut app = connected();
    type_text(&mut app, "/handoff ");
    assert_eq!(app.completions(), None);
    app.on_key(Key::Backspace, now());
    assert_eq!(shown(&app).len(), 1);
}

#[test]
fn the_approvals_row_opens_the_waiting_queue() {
    let mut app = connected();
    assert_eq!(enter(&mut app, "/approvals"), Effect::None);
    assert_eq!(app.notice(), Some("No requests waiting."));
    assert_eq!(app.draft(), "");
}

#[test]
fn the_conversation_gives_up_the_panel_rows() {
    let mut app = connected();
    app.set_size(80, 24);
    assert_eq!(app.conversation_height(), 23);
    type_text(&mut app, "/");
    assert_eq!(app.conversation_height(), 15);
    type_text(&mut app, "han");
    assert_eq!(app.conversation_height(), 22);
}

#[test]
fn an_open_approval_panel_wins_over_the_slash_panel() {
    let mut app = attached();
    type_text(&mut app, "/");
    let request = session_line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"}}),
    );
    assert!(app.on_line(request).is_empty());
    assert!(app.panel().is_some());
    assert_eq!(app.completions(), None);
    // Esc goes to the approval panel, which puts the request aside; the
    // slash panel shows again, still open.
    app.on_key(Key::Esc, now());
    assert!(app.panel().is_none());
    assert_eq!(shown(&app).len(), 8);
}
