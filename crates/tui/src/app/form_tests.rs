//! Tests for answering and declining a question form through the app:
//! keys and pastes to the exact command lines.

use std::path::PathBuf;

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::app::{App, Effect};
use crate::approvals::form::Spot;
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::mouse::TargetId;

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

/// A one-question interaction `kind` with request id `id`.
fn asked_one(kind: &str, id: &str) -> contract::Envelope {
    let payload = match kind {
        "confirm" => json!({"request_id": id, "kind": kind, "prompt": "Continue?"}),
        "select" => json!({"request_id": id, "kind": kind, "prompt": "Pick one?",
            "options": [{"label": "a"}, {"label": "b"}, {"label": "c"}]}),
        "multi_select" => json!({"request_id": id, "kind": kind, "prompt": "Pick some?",
            "options": [{"label": "a"}, {"label": "b"}, {"label": "c"}]}),
        "text_input" => json!({"request_id": id, "kind": kind, "prompt": "What?"}),
        _ => panic!("unknown one-question interaction: {kind}"),
    };
    from(S_A, "interaction_requested", payload)
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

/// Presses the stroke `name` through the app's bindings.
fn stroke(app: &mut App, name: &str) -> Effect {
    let stroke = crate::stroke::Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"));
    app.on_press(stroke, fakes::clock::FakeClock::new().now())
}

/// The terminal cursor's cell on a 60 by 12 screen.
fn caret(app: &App) -> Option<(u16, u16)> {
    let area = ratatui::layout::Rect::new(0, 0, 60, 12);
    crate::view::cursor(app, area).map(|at| (at.x, at.y))
}

/// The panel's tab line.
fn tab_line(app: &App) -> String {
    panel(app).get(1).cloned().unwrap_or_default()
}

#[test]
fn the_form_keys_go_through_the_keyset() {
    let mut app = showing();
    app.set_size(60, 12);
    assert_eq!(caret(&app), None);
    assert_eq!(stroke(&mut app, "space"), Effect::None);
    assert!(panel(&app).contains(&"› (•) main (Recommended) · the default".to_owned()));
    stroke(&mut app, "right");
    assert_eq!(tab_line(&app), "Base ✓  [Name]  Submit");
    // Six panel lines on the bottom rows: the words row is the fourth,
    // inset two columns with the bottom edge below.
    assert_eq!(caret(&app), Some((6, 8)));
    for ch in "ab".chars() {
        press(&mut app, Key::Char(ch));
    }
    assert_eq!(caret(&app), Some((8, 8)));
    stroke(&mut app, "left");
    assert_eq!(caret(&app), Some((7, 8)));
    assert_eq!(tab_line(&app), "Base ✓  [Name ✓]  Submit");
    stroke(&mut app, "shift+tab");
    assert_eq!(tab_line(&app), "[Base ✓]  Name ✓  Submit");
    assert_eq!(caret(&app), None);
    stroke(&mut app, "tab");
    assert_eq!(tab_line(&app), "Base ✓  [Name ✓]  Submit");
}

#[test]
fn arrows_reach_a_form_over_an_open_prompt_search() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached();
    app.on_key(Key::CtrlR, now);
    feed(&mut app, asked());
    app.on_edit(Edit::Right);
    assert_eq!(tab_line(&app), "Base  [Name]  Submit");
    press(&mut app, Key::Char('a'));
    press(&mut app, Key::Char('c'));
    app.on_edit(Edit::Left);
    press(&mut app, Key::Char('b'));
    assert!(
        panel(&app).contains(&"› ✎ abc".to_owned()),
        "{:?}",
        panel(&app)
    );
    // Once the form is declined, the search shows with its query empty.
    assert_eq!(sends(&mut app, Key::Esc)["args"]["declined"], true);
    assert_eq!(search_query(&app).as_deref(), Some("search prompts: "));
}

#[test]
fn a_global_binding_on_a_form_key_wins() {
    let mut app = showing();
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!(["f1", "right"]));
    app.set_keys(crate::KeysSetup { user });
    stroke(&mut app, "right");
    assert!(app.keymap_top().is_some());
    assert_eq!(tab_line(&app), "[Base]  Name  Submit");
}

/// Sends the key sequence that answers one interaction kind.
fn answer_one(app: &mut App, kind: &str) -> (String, Value) {
    match kind {
        "confirm" => sent(app, Key::Enter),
        "select" => {
            press(app, Key::Down);
            sent(app, Key::Enter)
        }
        "multi_select" => {
            press(app, Key::Char(' '));
            press(app, Key::Down);
            press(app, Key::Down);
            press(app, Key::Char(' '));
            sent(app, Key::Enter)
        }
        "text_input" => {
            press(app, Key::Char('h'));
            press(app, Key::Char('i'));
            sent(app, Key::Enter)
        }
        _ => panic!("unknown one-question interaction: {kind}"),
    }
}

#[test]
fn each_one_question_kind_sends_its_exact_reply_args() {
    for (kind, request_id, expected) in [
        (
            "confirm",
            "r_c",
            json!({"request_id": "r_c", "confirmed": true}),
        ),
        (
            "select",
            "r_s",
            json!({"request_id": "r_s", "labels": ["b"]}),
        ),
        (
            "multi_select",
            "r_m",
            json!({"request_id": "r_m", "labels": ["a", "c"]}),
        ),
        (
            "text_input",
            "r_t",
            json!({"request_id": "r_t", "text": "hi"}),
        ),
    ] {
        let mut app = attached();
        feed(&mut app, asked_one(kind, request_id));
        let (_, reply) = answer_one(&mut app, kind);
        assert_eq!(reply["command"], "reply", "{kind}");
        assert_eq!(reply["session_id"], S_A, "{kind}");
        assert_eq!(reply["args"], expected, "{kind}");
        assert!(app.panel().is_none(), "{kind}");
    }
}

#[test]
fn esc_on_each_one_question_declines_then_cancels_once() {
    for (kind, request_id) in [
        ("confirm", "r_c"),
        ("select", "r_s"),
        ("multi_select", "r_m"),
        ("text_input", "r_t"),
    ] {
        let mut app = attached();
        feed(&mut app, asked_one(kind, request_id));
        let (reply_id, reply) = sent(&mut app, Key::Esc);
        assert_eq!(reply["command"], "reply", "{kind}");
        assert_eq!(reply["session_id"], S_A, "{kind}");
        assert_eq!(
            reply["args"],
            json!({"request_id": request_id, "declined": true}),
            "{kind}"
        );
        let cancel_id = cancels(&mut app, accepted(S_A, &reply_id), S_A);
        assert!(
            fold(&mut app, accepted(S_A, &reply_id)).is_empty(),
            "{kind}"
        );
        assert!(cancel_id.starts_with("c_"));
    }
}

#[test]
fn an_enter_reply_accept_does_not_cancel_any_one_question_turn() {
    for (kind, request_id) in [
        ("confirm", "r_c"),
        ("select", "r_s"),
        ("multi_select", "r_m"),
        ("text_input", "r_t"),
    ] {
        let mut app = attached();
        feed(&mut app, asked_one(kind, request_id));
        let (reply_id, _) = answer_one(&mut app, kind);
        assert!(
            fold(&mut app, accepted(S_A, &reply_id)).is_empty(),
            "{kind}"
        );
    }
}

#[test]
fn a_rejected_text_input_reply_restores_its_words() {
    let mut app = attached();
    feed(&mut app, asked_one("text_input", "r_t"));
    press(&mut app, Key::Char('h'));
    press(&mut app, Key::Char('i'));
    let (reply_id, _) = sent(&mut app, Key::Enter);
    assert!(app.panel().is_none());
    assert!(fold(&mut app, rejected(S_A, &reply_id)).is_empty());
    assert_eq!(header(&app), format!("question · {S_A} · 1 of 1"));
    assert!(panel(&app).contains(&"› ✎ hi".to_owned()));
}

#[test]
fn clicking_chat_about_this_declines_a_one_question() {
    let mut app = attached();
    feed(&mut app, asked_one("confirm", "r_c"));
    let (reply_id, value) = without_id_from_effect(app.on_click(TargetId::Form(Spot::Chat)));
    assert_eq!(value["command"], "reply");
    assert_eq!(value["session_id"], S_A);
    assert_eq!(
        value["args"],
        json!({"request_id": "r_c", "declined": true})
    );
    cancels(&mut app, accepted(S_A, &reply_id), S_A);
}

/// Parses the one command sent by a click, taking out its id.
fn without_id_from_effect(effect: Effect) -> (String, Value) {
    let Effect::Send(lines) = effect else {
        panic!("expected one line, got {effect:?}");
    };
    assert_eq!(lines.len(), 1, "{lines:?}");
    without_id(lines.first().map(String::as_str).unwrap_or_default())
}
