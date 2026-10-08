//! Tests for answering and declining a question form through the app:
//! keys and pastes to the exact command lines.

use std::path::PathBuf;

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::app::{App, Effect};
use crate::keys::{Edit, Key};
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// An app connected to the hub and attached to `S_A`.
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    app.attach(contract::SessionId(S_A.to_owned()));
    app
}

/// One envelope from `S_A` without an action.
fn envelope(kind: &str, payload: Value) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// Folds one session envelope, which sends nothing.
fn feed(app: &mut App, line: contract::Envelope) {
    assert!(app.on_line(Line::Session(line)).is_empty());
}

/// The form `r_4f`: single-choice `Base`, then free-text `Name`.
fn asked() -> contract::Envelope {
    envelope(
        "interaction_requested",
        json!({"request_id": "r_4f", "kind": "form", "action_ids": ["a_1"], "fields": [
            {"header": "Base", "question": "Which branch?", "options": [
                {"label": "main (Recommended)", "description": "the default"},
                {"label": "dev"}]},
            {"header": "Name", "question": "What name?"}]}),
    )
}

/// An app attached to `S_A` showing the form `r_4f`.
fn showing() -> App {
    let mut app = attached();
    feed(&mut app, asked());
    assert_eq!(header(&app), format!("question · {S_A} · 1 of 1"));
    app
}

/// The panel's lines, or none when it is closed.
fn panel(app: &App) -> Vec<String> {
    app.panel().map(|panel| panel.lines).unwrap_or_default()
}

/// The panel's header line.
fn header(app: &App) -> String {
    panel(app).first().cloned().unwrap_or_default()
}

/// Presses `key`, which sends nothing.
fn press(app: &mut App, key: Key) {
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(key, now), Effect::None);
}

/// The single line `key` sends, parsed, with its `id` taken out.
fn sends(app: &mut App, key: Key) -> Value {
    let now = fakes::clock::FakeClock::new().now();
    let effect = app.on_key(key, now);
    let Effect::Send(lines) = effect else {
        panic!("expected one line, got {effect:?}");
    };
    assert_eq!(lines.len(), 1, "{lines:?}");
    let mut value: Value = lines
        .first()
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap_or_default();
    let id = value
        .as_object_mut()
        .and_then(|line| line.remove("id"))
        .unwrap_or_default();
    assert!(id.as_str().is_some_and(|id| id.starts_with("c_")), "{id}");
    value
}

#[test]
fn enter_through_the_form_sends_one_reply() {
    let mut app = showing();
    press(&mut app, Key::Enter);
    for ch in "fiber-cli".chars() {
        press(&mut app, Key::Char(ch));
    }
    press(&mut app, Key::Enter);
    assert_eq!(
        sends(&mut app, Key::Enter),
        json!({"command": "reply", "session_id": S_A, "args": {"request_id": "r_4f",
            "answers": [{"labels": ["main (Recommended)"]},
                {"labels": [], "text": "fiber-cli"}]}})
    );
    assert!(app.panel().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn esc_sends_one_declined_reply() {
    let mut app = showing();
    assert_eq!(
        sends(&mut app, Key::Esc),
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_4f", "declined": true}})
    );
    assert!(app.panel().is_none());
    assert!(app.badge().is_none());
}

#[test]
fn a_decline_with_the_link_down_sends_nothing_and_keeps_the_form() {
    let mut app = showing();
    app.disconnected();
    press(&mut app, Key::Esc);
    assert_eq!(header(&app), format!("question · {S_A} · 1 of 1"));
}

#[test]
fn a_paste_on_the_form_goes_to_the_words_row() {
    let mut app = showing();
    assert_eq!(
        app.on_edit(Edit::Paste("fiber\ncli".to_owned())),
        Effect::None
    );
    assert!(panel(&app).contains(&"› ✎ fiber cli".to_owned()));
    assert_eq!(app.draft(), "");
}

/// The prompt search panel's first line, its query.
fn search_query(app: &App) -> Option<String> {
    app.completions()
        .and_then(|completions| completions.lines.first().cloned())
}

#[test]
fn a_form_over_an_open_prompt_search_takes_the_paste() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached();
    app.on_key(Key::CtrlR, now);
    assert_eq!(search_query(&app).as_deref(), Some("search prompts: "));
    feed(&mut app, asked());
    app.on_edit(Edit::Paste("x".to_owned()));
    assert!(panel(&app).contains(&"› ✎ x".to_owned()));
    assert_eq!(sends(&mut app, Key::Esc)["args"]["declined"], true);
    assert_eq!(search_query(&app).as_deref(), Some("search prompts: "));
}

#[test]
fn an_approval_over_an_open_prompt_search_takes_the_paste() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached();
    app.on_key(Key::CtrlR, now);
    feed(
        &mut app,
        envelope(
            "permission_requested",
            json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
                "step": "standing_ask",
                "standing_rule": {"scope": "global", "prefix": "ls"}}),
        ),
    );
    app.on_edit(Edit::Paste("no".to_owned()));
    assert_eq!(panel(&app).last().map(String::as_str), Some("› deny · no"));
    press(&mut app, Key::Esc);
    assert_eq!(search_query(&app).as_deref(), Some("search prompts: "));
}
