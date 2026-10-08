//! Tests for answering and declining a question form through the app:
//! keys and pastes to the exact command lines.

use std::path::PathBuf;

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::app::{App, Effect};
use crate::keys::{Edit, Key};
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";

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
    from(S_A, kind, payload)
}

/// One envelope from `session` without an action.
fn from(session: &str, kind: &str, payload: Value) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
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

/// The form `r_4f` from `S_A`.
fn asked() -> contract::Envelope {
    asked_by(S_A)
}

/// The form `r_4f` from `session`: single-choice `Base`, then free-text
/// `Name`.
fn asked_by(session: &str) -> contract::Envelope {
    from(
        session,
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
    sent(app, key).1
}

/// The single line `key` sends: its `id`, and the line parsed without it.
fn sent(app: &mut App, key: Key) -> (String, Value) {
    let now = fakes::clock::FakeClock::new().now();
    let effect = app.on_key(key, now);
    let Effect::Send(lines) = effect else {
        panic!("expected one line, got {effect:?}");
    };
    assert_eq!(lines.len(), 1, "{lines:?}");
    without_id(lines.first().map(String::as_str).unwrap_or_default())
}

/// A command line parsed: its `id`, and the rest.
fn without_id(line: &str) -> (String, Value) {
    let mut value: Value = serde_json::from_str(line).unwrap_or_default();
    let id = value
        .as_object_mut()
        .and_then(|line| line.remove("id"))
        .and_then(|id| id.as_str().map(str::to_owned))
        .unwrap_or_default();
    assert!(id.starts_with("c_"), "{id}");
    (id, value)
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

/// `command_accepted` for `id` from `session`.
fn accepted(session: &str, id: &str) -> contract::Envelope {
    from(session, "command_accepted", json!({"command_id": id}))
}

/// `command_rejected` `stale_request` for `id` from `session`.
fn rejected(session: &str, id: &str) -> contract::Envelope {
    from(
        session,
        "command_rejected",
        json!({"command_id": id, "code": "stale_request", "message": "gone"}),
    )
}

/// `interaction_resolved` declining `r_4f`, from `S_A`.
fn resolved_declined() -> contract::Envelope {
    envelope(
        "interaction_resolved",
        json!({"request_id": "r_4f", "by": "person", "declined": true}),
    )
}

/// The lines folding `line` sends.
fn fold(app: &mut App, line: contract::Envelope) -> Vec<String> {
    app.on_line(Line::Session(line))
}

/// The single `cancel` line folding `line` sends: its command id, after
/// checking it names `session`.
fn cancels(app: &mut App, line: contract::Envelope, session: &str) -> String {
    let lines = fold(app, line);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let (id, value) = without_id(lines.first().map(String::as_str).unwrap_or_default());
    assert_eq!(value, json!({"command": "cancel", "session_id": session}));
    id
}

#[test]
fn esc_sends_declined_then_cancel_after_the_accept() {
    let mut app = showing();
    let (id, _) = sent(&mut app, Key::Esc);
    assert!(fold(&mut app, resolved_declined()).is_empty());
    cancels(&mut app, accepted(S_A, &id), S_A);
    // A second accept of the same decline sends nothing.
    assert!(fold(&mut app, accepted(S_A, &id)).is_empty());
}

#[test]
fn an_accept_before_the_resolution_also_cancels() {
    let mut app = showing();
    let (id, _) = sent(&mut app, Key::Esc);
    cancels(&mut app, accepted(S_A, &id), S_A);
    assert!(fold(&mut app, resolved_declined()).is_empty());
    assert!(app.panel().is_none());
}

#[test]
fn chat_about_this_declines_like_esc() {
    let mut app = showing();
    for _ in 0..4 {
        press(&mut app, Key::Down);
    }
    let (id, value) = sent(&mut app, Key::Enter);
    assert_eq!(
        value,
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_4f", "declined": true}})
    );
    cancels(&mut app, accepted(S_A, &id), S_A);
}

#[test]
fn a_rejected_decline_restores_the_form_and_never_cancels() {
    let mut app = showing();
    press(&mut app, Key::Char('x'));
    let (id, _) = sent(&mut app, Key::Esc);
    assert!(app.panel().is_none());
    assert!(fold(&mut app, rejected(S_A, &id)).is_empty());
    assert_eq!(app.notice(), Some("gone"));
    assert_eq!(header(&app), format!("question · {S_A} · 1 of 1"));
    assert!(panel(&app).contains(&"› ✎ x".to_owned()));
    assert!(fold(&mut app, accepted(S_A, &id)).is_empty());
}

#[test]
fn a_submit_accept_sends_no_cancel() {
    let mut app = showing();
    press(&mut app, Key::Tab);
    press(&mut app, Key::Tab);
    let (id, value) = sent(&mut app, Key::Enter);
    assert_eq!(
        value["args"]["answers"],
        json!([{"skipped": true}, {"skipped": true}])
    );
    assert!(fold(&mut app, accepted(S_A, &id)).is_empty());
}

/// A standing-ask approval `r_1` from `S_A`.
fn approval() -> contract::Envelope {
    envelope(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "ls"}}),
    )
}

#[test]
fn an_approval_reply_accept_sends_no_cancel() {
    let mut app = attached();
    feed(&mut app, approval());
    let (id, value) = sent(&mut app, Key::Enter);
    assert_eq!(value["args"]["decision"], "allow");
    assert!(fold(&mut app, accepted(S_A, &id)).is_empty());
}

#[test]
fn a_form_from_another_session_is_declined_and_cancelled_there() {
    let mut app = attached();
    feed(&mut app, asked_by(S_B));
    assert_eq!(header(&app), format!("question · {S_B} · 1 of 1"));
    let (id, value) = sent(&mut app, Key::Esc);
    assert_eq!(value["session_id"], S_B);
    cancels(&mut app, accepted(S_B, &id), S_B);
}

#[test]
fn esc_on_an_approval_then_esc_on_the_form_behind_it_declines_the_form() {
    let mut app = attached();
    feed(&mut app, approval());
    feed(&mut app, asked());
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 2"));
    press(&mut app, Key::Esc);
    assert_eq!(header(&app), format!("question · {S_A} · 2 of 2"));
    assert_eq!(
        sends(&mut app, Key::Esc),
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_4f", "declined": true}})
    );
}

#[test]
fn a_rejected_submit_from_another_session_restores_its_form() {
    let mut app = attached();
    feed(&mut app, asked_by(S_B));
    press(&mut app, Key::Enter);
    press(&mut app, Key::Tab);
    let (id, _) = sent(&mut app, Key::Enter);
    assert!(app.panel().is_none());
    assert!(fold(&mut app, rejected(S_B, &id)).is_empty());
    assert_eq!(app.notices().len(), 1);
    assert_eq!(header(&app), format!("question · {S_B} · 1 of 1"));
    assert!(panel(&app).contains(&"Base: main (Recommended)".to_owned()));
}

#[test]
fn an_attached_rejection_is_handled_once() {
    let mut app = showing();
    let (id, _) = sent(&mut app, Key::Esc);
    assert!(fold(&mut app, rejected(S_A, &id)).is_empty());
    assert_eq!(app.notices().len(), 1);
    assert_eq!(header(&app), format!("question · {S_A} · 1 of 1"));
}

#[test]
fn a_rejected_cancel_shows_no_notice() {
    let mut app = attached();
    feed(&mut app, asked_by(S_B));
    let (id, _) = sent(&mut app, Key::Esc);
    let cancel = cancels(&mut app, accepted(S_B, &id), S_B);
    assert!(fold(&mut app, rejected(S_B, &cancel)).is_empty());
    assert!(app.notices().is_empty());
    let mut app = showing();
    let (id, _) = sent(&mut app, Key::Esc);
    let cancel = cancels(&mut app, accepted(S_A, &id), S_A);
    assert!(fold(&mut app, rejected(S_A, &cancel)).is_empty());
    assert!(app.notices().is_empty());
}
