//! Tests for interrupting the turn on screen (`docs/tui.md`,
//! "Interrupts"): ⌥I and the two click targets cancel the turn of the
//! conversation on screen, a delegate's own turn in its view, and Esc in
//! that view only closes it.

use contract::SessionId;
use contract::events::EventKind;
use serde_json::{Value, json};

use super::commands::commands_testkit::{now, sent};
use super::items::testkit::{
    DELEGATE_A, SESSION, complete, home, opened, opened_item, session_line, start_delegate,
    start_job,
};
use super::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::TargetId;
use crate::stroke::Stroke;

/// One envelope of `session`.
fn line_of(session: &str, kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::from_name(kind),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `turn_started` payload.
fn turn() -> Value {
    json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]})
}

/// The attached session starts a turn.
fn parent_turn(app: &mut App) {
    app.on_line(session_line("turn_started", turn()));
}

/// The delegate starts a turn.
fn delegate_turn(app: &mut App) {
    app.on_line(line_of(DELEGATE_A, "turn_started", turn()));
}

/// An attached app with the delegate's view open, its parent idle.
fn delegate_view() -> App {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app
}

/// Presses the stroke `name` names, returning the lines it sends.
fn press(app: &mut App, name: &str) -> Vec<Value> {
    let stroke = Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"));
    sent(app.on_press(stroke, now()))
}

/// The `cancel` commands among `out`, as `(session, id)`.
fn cancels(out: &[Value]) -> Vec<String> {
    out.iter()
        .filter(|line| line["command"] == "cancel")
        .map(|line| line["session_id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Loads `entries` as the person's `keys`.
fn keyed(app: &mut App, entries: &[(&str, Value)]) {
    let mut user = serde_json::Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), value.clone());
    }
    app.set_keys(crate::KeysSetup { user });
}

#[test]
fn alt_i_interrupts_a_busy_main_turn_and_nothing_when_idle() {
    let mut app = home();
    opened(&mut app);
    // Idle: no turn to interrupt.
    assert!(press(&mut app, "alt+i").is_empty());
    // The positive control: the same stroke on a running turn cancels.
    parent_turn(&mut app);
    let out = press(&mut app, "alt+i");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(cancels(&out), [SESSION]);
}

#[test]
fn alt_i_sends_nothing_while_disconnected() {
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    app.disconnected();
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn alt_i_in_a_delegate_view_cancels_the_delegate_with_an_idle_parent() {
    let mut app = delegate_view();
    delegate_turn(&mut app);
    let out = press(&mut app, "alt+i");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(cancels(&out), [DELEGATE_A]);
    // The view stays open.
    assert!(app.item_open());
}

#[test]
fn alt_i_in_a_delegate_view_cancels_the_delegate_not_a_busy_parent() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    delegate_turn(&mut app);
    let out = press(&mut app, "alt+i");
    assert_eq!(cancels(&out), [DELEGATE_A], "{out:?}");
    assert_eq!(out.len(), 1);
}

#[test]
fn alt_i_in_an_idle_delegate_view_never_cancels_the_busy_parent() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn alt_i_in_a_finished_delegate_view_never_cancels_the_busy_parent() {
    let mut app = delegate_view();
    delegate_turn(&mut app);
    parent_turn(&mut app);
    complete(&mut app, "j_1");
    assert!(app.item_open());
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn alt_i_after_the_delegate_turn_completes_sends_nothing() {
    let mut app = delegate_view();
    delegate_turn(&mut app);
    assert_eq!(cancels(&press(&mut app, "alt+i")), [DELEGATE_A]);
    app.on_line(line_of(
        DELEGATE_A,
        "turn_completed",
        json!({"outcome": "interrupted"}),
    ));
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn alt_i_in_a_job_view_interrupts_the_busy_main_turn() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_2");
    opened_item(&mut app, "j_2");
    assert!(press(&mut app, "alt+i").is_empty());
    parent_turn(&mut app);
    assert_eq!(cancels(&press(&mut app, "alt+i")), [SESSION]);
}

#[test]
fn clicking_the_working_line_target_in_a_delegate_view_cancels_the_delegate() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    delegate_turn(&mut app);
    let out = sent(app.on_click(TargetId::Interrupt));
    assert_eq!(cancels(&out), [DELEGATE_A], "{out:?}");
    assert_eq!(out.len(), 1);
    // Idle delegate: the target is gone and the click is a no-op.
    let mut idle = delegate_view();
    parent_turn(&mut idle);
    assert_eq!(idle.on_click(TargetId::Interrupt), Effect::None);
}

#[test]
fn esc_in_a_delegate_view_closes_it_and_sends_no_cancel() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    delegate_turn(&mut app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.item_open());
}

#[test]
fn a_cancel_leaves_the_draft_and_the_steering_queue_alone() {
    let mut app = delegate_view();
    delegate_turn(&mut app);
    app.draft.set("half typed");
    let out = press(&mut app, "alt+i");
    assert_eq!(cancels(&out), [DELEGATE_A]);
    assert_eq!(app.draft(), "half typed");
}

#[test]
fn a_rebound_key_interrupts_and_the_old_key_stops() {
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    keyed(&mut app, &[("interrupt_turn", json!("f5"))]);
    assert!(press(&mut app, "alt+i").is_empty());
    assert_eq!(cancels(&press(&mut app, "f5")), [SESSION]);
}

#[test]
fn an_unbound_action_interrupts_from_no_key() {
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    keyed(&mut app, &[("interrupt_turn", json!([]))]);
    assert!(press(&mut app, "alt+i").is_empty());
    // The click target still works.
    assert_eq!(cancels(&sent(app.on_click(TargetId::Interrupt))), [SESSION]);
}
