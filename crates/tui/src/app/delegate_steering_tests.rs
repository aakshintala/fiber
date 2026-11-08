//! A fiber delegate's view draws and edits its own steering queue through
//! the same card, keys and targets as the conversation (`docs/tui.md`,
//! "Swapped views" and "Steering").

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::items::testkit::{
    DELEGATE_A, SESSION, commands, complete, home, open, opened, opened_item, session_line,
    start_delegate, start_job,
};
use super::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::TargetId;

/// A `steering_queue` line for `session`: each row its text and its
/// `steer` id, `None` for Fiber's own message.
fn queue_line(session: &str, rows: &[(&str, Option<&str>)]) -> Line {
    let messages: Vec<Value> = rows
        .iter()
        .map(|(text, id)| {
            let mut message = json!({
                "content": [{"type": "text", "text": text}],
                "source": "driver",
            });
            if let (Some(id), Some(object)) = (id, message.as_object_mut()) {
                object.insert("command_id".to_owned(), json!(id));
            }
            message
        })
        .collect();
    let Line::Session(mut envelope) = session_line("steering_queue", json!({"messages": messages}))
    else {
        panic!("a session line")
    };
    envelope.session_id = contract::SessionId(session.to_owned());
    Line::Session(envelope)
}

/// Types `text` one key at a time.
fn type_text(app: &mut App, clock: &fakes::clock::FakeClock, text: &str) {
    for ch in text.chars() {
        app.on_key(Key::Char(ch), clock.now());
    }
}

/// The lines `effect` sends.
fn sent(effect: Effect) -> Vec<Value> {
    let Effect::Send(lines) = effect else {
        panic!("expected a send")
    };
    commands(lines)
}

/// The whole app drawn at 80x24, rows trimmed of trailing spaces.
fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn a_steer_typed_in_a_delegate_view_shows_in_its_steering_card() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    type_text(&mut app, &clock, "use the parser");
    let out = sent(app.on_key(Key::Enter, clock.now()));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "steer");
    assert_eq!(out[0]["session_id"], DELEGATE_A);
    let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(queue_line(DELEGATE_A, &[("use the parser", Some(&id))]));
    let shown = screen(&app);
    assert!(shown.contains("Steering"), "{shown}");
    assert!(shown.contains("↳ use the parser"), "{shown}");
    insta::assert_snapshot!("queued_delegate_steering", shown);
}

const DELEGATE_B: &str = "s_eeeeeeeeeeeeeeee";

/// An app on a running fiber delegate `j_1`, its view open and subscribed,
/// the attached session's queue holding `parent`.
fn view_with_parent(
    parent: &[(&str, Option<&str>)],
) -> (App, std::sync::Arc<fakes::clock::FakeClock>) {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    app.on_line(queue_line(SESSION, parent));
    opened_item(&mut app, "j_1");
    (app, clock)
}

/// The delegate view with an empty attached queue.
fn view() -> (App, std::sync::Arc<fakes::clock::FakeClock>) {
    view_with_parent(&[])
}

/// The click targets of the whole app drawn at 80x24.
fn targets(app: &App) -> Vec<crate::mouse::Target> {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None)
}

/// `(command, session, args)` of each line sent.
fn wire(lines: &[Value]) -> Vec<(String, String, Value)> {
    lines
        .iter()
        .map(|line| {
            (
                line["command"].as_str().unwrap_or_default().to_owned(),
                line["session_id"].as_str().unwrap_or_default().to_owned(),
                line["args"].clone(),
            )
        })
        .collect()
}

fn drop_of(session: &str, id: &str) -> (String, String, Value) {
    (
        "steer_drop".to_owned(),
        session.to_owned(),
        json!({"command_id": id}),
    )
}

#[test]
fn an_empty_delegate_queue_hides_the_attached_sessions_rows() {
    let (mut app, _clock) = view_with_parent(&[("parent row", Some("c_p1"))]);
    assert!(app.steering().is_empty());
    assert!(!screen(&app).contains("parent row"));
    app.close_item();
    assert_eq!(app.steering(), ["↳ parent row"]);
    assert!(screen(&app).contains("parent row"));
}

#[test]
fn the_attached_queue_keeps_folding_while_hidden_and_returns_latest() {
    let (mut app, _clock) = view_with_parent(&[("old parent", Some("c_p1"))]);
    app.on_line(queue_line(DELEGATE_A, &[("delegate row", Some("c_d1"))]));
    app.on_line(queue_line(SESSION, &[("new parent", Some("c_p2"))]));
    assert_eq!(app.steering(), ["↳ delegate row"]);
    app.close_item();
    assert_eq!(app.steering(), ["↳ new parent"]);
}

#[test]
fn delegate_and_attached_queues_fold_in_either_order() {
    let (mut app, _clock) = view();
    app.on_line(queue_line(SESSION, &[("parent", Some("c_p1"))]));
    app.on_line(queue_line(DELEGATE_A, &[("delegate", Some("c_d1"))]));
    assert_eq!(app.steering(), ["↳ delegate"]);
    app.close_item();
    assert_eq!(app.steering(), ["↳ parent"]);
}

#[test]
fn a_hidden_selection_removed_by_the_attached_queue_restores_into_the_parent_draft() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    type_text(&mut app, &clock, "typed before");
    app.on_line(queue_line(SESSION, &[("parent row", Some("c_p1"))]));
    app.on_key(Key::AltUp, clock.now());
    assert_eq!(app.draft(), "parent row");
    opened_item(&mut app, "j_1");
    // The view shows no edit and no selection.
    assert_eq!(app.draft(), "");
    assert!(app.steering_selected().is_none());
    type_text(&mut app, &clock, "to the delegate");
    app.on_line(queue_line(SESSION, &[]));
    assert_eq!(app.draft(), "to the delegate");
    assert!(app.steering().is_empty());
    app.close_item();
    assert_eq!(app.draft(), "typed before");
    assert!(app.steering().is_empty());
}

#[test]
fn a_parent_selection_survives_a_view_and_comes_back_selected() {
    let (mut app, clock) = {
        let clock = fakes::clock::FakeClock::new();
        let mut app = home();
        opened(&mut app);
        start_delegate(&mut app, "j_1", DELEGATE_A);
        app.on_line(queue_line(SESSION, &[("parent row", Some("c_p1"))]));
        app.on_key(Key::AltUp, clock.now());
        opened_item(&mut app, "j_1");
        (app, clock)
    };
    assert!(app.steering_selected().is_none());
    let _ = clock;
    app.close_item();
    assert_eq!(app.steering(), ["▸ parent row"]);
    assert_eq!(app.draft(), "parent row");
}

#[test]
fn another_sessions_queue_is_ignored_and_switching_views_shows_no_old_rows() {
    let (mut app, _clock) = view();
    start_delegate(&mut app, "j_2", DELEGATE_B);
    app.on_line(queue_line(DELEGATE_B, &[("other", Some("c_x"))]));
    assert!(app.steering().is_empty());
    app.on_line(queue_line(DELEGATE_A, &[("a row", Some("c_a1"))]));
    assert_eq!(app.steering(), ["↳ a row"]);
    opened_item(&mut app, "j_2");
    assert!(app.steering().is_empty());
    app.on_line(queue_line(DELEGATE_B, &[("b row", Some("c_b1"))]));
    assert_eq!(app.steering(), ["↳ b row"]);
    app.close_item();
    assert!(app.steering().is_empty());
}

#[test]
fn closing_and_reopening_replays_the_delegate_queue() {
    let (mut app, _clock) = view();
    app.on_line(queue_line(DELEGATE_A, &[("one", Some("c_d1"))]));
    app.close_item();
    let out = open(&mut app, "j_1");
    super::items::testkit::ack_all(&mut app, &out);
    assert!(app.steering().is_empty());
    app.on_line(queue_line(DELEGATE_A, &[("one", Some("c_d1"))]));
    assert_eq!(app.steering(), ["↳ one"]);
}

/// A delegate view whose queue holds `rows`.
fn view_queued(rows: &[(&str, Option<&str>)]) -> (App, std::sync::Arc<fakes::clock::FakeClock>) {
    let (mut app, clock) =
        view_with_parent(&[("parent one", Some("c_p1")), ("parent two", Some("c_p2"))]);
    app.on_line(queue_line(DELEGATE_A, rows));
    (app, clock)
}

#[test]
fn alt_up_and_alt_down_walk_the_delegates_queue() {
    let (mut app, clock) = view_queued(&[
        ("one", Some("c_d1")),
        ("two", Some("c_d2")),
        ("three", Some("c_d3")),
    ]);
    type_text(&mut app, &clock, "mine");
    app.on_key(Key::AltUp, clock.now());
    assert_eq!(app.steering(), ["↳ one", "↳ two", "▸ three"]);
    assert_eq!(app.draft(), "three");
    app.on_key(Key::AltUp, clock.now());
    assert_eq!(app.steering(), ["↳ one", "▸ two", "↳ three"]);
    app.on_key(Key::AltUp, clock.now());
    app.on_key(Key::AltUp, clock.now());
    assert_eq!(app.steering(), ["▸ one", "↳ two", "↳ three"]);
    assert_eq!(app.draft(), "one");
    app.on_key(Key::AltDown, clock.now());
    assert_eq!(app.steering(), ["↳ one", "▸ two", "↳ three"]);
    app.on_key(Key::AltDown, clock.now());
    app.on_key(Key::AltDown, clock.now());
    assert_eq!(app.steering(), ["↳ one", "↳ two", "↳ three"]);
    assert_eq!(app.draft(), "mine");
}

#[test]
fn alt_x_drops_the_delegates_selected_row_or_every_droppable_row() {
    let (mut app, clock) = view_queued(&[
        ("one", Some("c_d1")),
        ("fibers", None),
        ("two", Some("c_d2")),
    ]);
    let drops = wire(&sent(app.on_key(Key::AltX, clock.now())));
    assert_eq!(
        drops,
        [drop_of(DELEGATE_A, "c_d1"), drop_of(DELEGATE_A, "c_d2")]
    );
    app.on_key(Key::AltUp, clock.now());
    let drops = wire(&sent(app.on_key(Key::AltX, clock.now())));
    assert_eq!(drops, [drop_of(DELEGATE_A, "c_d2")]);
    app.on_line(queue_line(DELEGATE_A, &[]));
    assert_eq!(app.on_key(Key::AltX, clock.now()), Effect::None);
}

#[test]
fn an_image_row_drops_but_cannot_be_selected() {
    let (mut app, clock) = view();
    let Line::Session(mut envelope) = queue_line(DELEGATE_A, &[]) else {
        panic!("a session line")
    };
    envelope.payload = json!({"messages": [
        {"content": [{"type": "image", "path": "artifacts/a.png",
            "mime_type": "image/png", "width": 1, "height": 1}],
         "source": "driver", "command_id": "c_img"},
    ]})
    .as_object()
    .cloned()
    .unwrap_or_default();
    app.on_line(Line::Session(envelope));
    app.on_key(Key::AltUp, clock.now());
    assert!(app.steering_selected().is_none());
    let drops = wire(&sent(app.drop_steering(0)));
    assert_eq!(drops, [drop_of(DELEGATE_A, "c_img")]);
}

#[test]
fn clicking_a_drawn_row_selects_it_and_its_cross_drops_it_in_the_delegate() {
    let (mut app, _clock) = view_queued(&[("one", Some("c_d1")), ("two", Some("c_d2"))]);
    let drawn = targets(&app);
    let row = drawn
        .iter()
        .find(|target| target.id == TargetId::Steering(1))
        .map(|target| target.id)
        .unwrap_or_else(|| panic!("a row target in {drawn:?}"));
    app.on_click(row);
    assert_eq!(app.steering(), ["↳ one", "▸ two"]);
    assert_eq!(app.draft(), "two");
    let cross = drawn
        .iter()
        .find(|target| target.id == TargetId::DropSteering(0))
        .map(|target| target.id)
        .unwrap_or_else(|| panic!("a cross target in {drawn:?}"));
    let drops = wire(&sent(app.on_click(cross)));
    assert_eq!(drops, [drop_of(DELEGATE_A, "c_d1")]);
}

#[test]
fn enter_on_a_selected_delegate_row_drops_it_then_steers_the_edit() {
    let (mut app, clock) = view_queued(&[("one", Some("c_d1")), ("two", Some("c_d2"))]);
    type_text(&mut app, &clock, "mine");
    app.on_key(Key::AltUp, clock.now());
    app.on_key(Key::Backspace, clock.now());
    app.on_key(Key::Char('O'), clock.now());
    let lines = sent(app.on_key(Key::Enter, clock.now()));
    assert_eq!(
        wire(&lines),
        [
            drop_of(DELEGATE_A, "c_d2"),
            (
                "steer".to_owned(),
                DELEGATE_A.to_owned(),
                json!({"content": [{"type": "text", "text": "twO"}]})
            ),
        ]
    );
    assert_ne!(lines[0]["id"], lines[1]["id"]);
    assert_eq!(app.draft(), "mine");
    assert_eq!(app.steering(), ["↳ one", "↳ two"]);
    // A stale drop changes nothing and shows nothing.
    let rejected = |line: &Value| {
        let Line::Session(mut envelope) = session_line(
            "command_rejected",
            json!({"command_id": line["id"], "code": "stale_request", "message": "gone"}),
        ) else {
            panic!("a session line")
        };
        envelope.session_id = contract::SessionId(DELEGATE_A.to_owned());
        Line::Session(envelope)
    };
    app.on_line(rejected(&lines[0]));
    assert_eq!(app.draft(), "mine");
    assert!(app.notice().is_none());
    // A rejected steer restores its text only into an empty draft.
    app.on_key(Key::CtrlC, clock.now());
    app.on_line(rejected(&lines[1]));
    assert_eq!(app.draft(), "twO");
}

#[test]
fn a_rejected_amend_steer_never_overwrites_newer_input() {
    let (mut app, clock) = view_queued(&[("one", Some("c_d1"))]);
    app.on_key(Key::AltUp, clock.now());
    let lines = sent(app.on_key(Key::Enter, clock.now()));
    type_text(&mut app, &clock, "newer");
    let Line::Session(mut envelope) = session_line(
        "command_rejected",
        json!({"command_id": lines[1]["id"], "code": "stale_request", "message": "gone"}),
    ) else {
        panic!("a session line")
    };
    envelope.session_id = contract::SessionId(DELEGATE_A.to_owned());
    app.on_line(Line::Session(envelope));
    assert_eq!(app.draft(), "newer");
}

#[test]
fn esc_clears_a_delegate_selection_before_it_closes_the_view() {
    let (mut app, clock) = view_queued(&[("one", Some("c_d1"))]);
    app.on_key(Key::AltUp, clock.now());
    assert_eq!(app.on_key(Key::Esc, clock.now()), Effect::None);
    assert!(app.item_open());
    assert_eq!(app.draft(), "");
    assert_eq!(app.on_key(Key::Esc, clock.now()), Effect::None);
    assert!(!app.item_open());
}

#[test]
fn a_delegate_finishing_clears_its_queue_and_selection_but_not_the_parents() {
    let (mut app, clock) = view_queued(&[("one", Some("c_d1"))]);
    type_text(&mut app, &clock, "mine");
    app.on_key(Key::AltUp, clock.now());
    complete(&mut app, "j_1");
    assert!(app.steering().is_empty());
    assert_eq!(app.draft(), "mine");
    // A late replayed queue does not bring the rows back, and keys send
    // nothing.
    app.on_line(queue_line(DELEGATE_A, &[("one", Some("c_d1"))]));
    assert!(app.steering().is_empty());
    assert_eq!(app.on_key(Key::AltX, clock.now()), Effect::None);
    app.close_item();
    assert_eq!(app.steering(), ["↳ parent one", "↳ parent two"]);
}

#[test]
fn a_resumed_delegate_takes_fresh_steering() {
    let (mut app, clock) = view_queued(&[("one", Some("c_d1"))]);
    complete(&mut app, "j_1");
    start_job(&mut app, "j_1");
    assert!(app.steering().is_empty());
    app.on_line(queue_line(DELEGATE_A, &[("again", Some("c_d9"))]));
    assert_eq!(app.steering(), ["↳ again"]);
    let drops = wire(&sent(app.on_key(Key::AltX, clock.now())));
    assert_eq!(drops, [drop_of(DELEGATE_A, "c_d9")]);
}

#[test]
fn a_finished_delegate_opens_read_only_with_no_queue() {
    let (mut app, clock) = view_with_parent(&[("parent one", Some("c_p1"))]);
    app.close_item();
    complete(&mut app, "j_1");
    opened_item(&mut app, "j_1");
    app.on_line(queue_line(DELEGATE_A, &[("stale", Some("c_d1"))]));
    assert!(app.steering().is_empty());
    assert_eq!(app.on_key(Key::AltX, clock.now()), Effect::None);
    assert!(!screen(&app).contains("parent one"));
}

#[test]
fn another_harness_delegate_and_a_job_view_hide_the_attached_queue() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_o", "description": "other", "output_path": "/tmp/o"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_o", "delegate_session_id": DELEGATE_B,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    start_job(&mut app, "j_p");
    app.on_line(queue_line(SESSION, &[("parent one", Some("c_p1"))]));
    for job in ["j_o", "j_p"] {
        opened_item(&mut app, job);
        assert!(app.steering().is_empty(), "{job}");
        assert!(!screen(&app).contains("parent one"), "{job}");
        assert_eq!(app.on_key(Key::AltX, clock.now()), Effect::None, "{job}");
        app.on_line(queue_line(DELEGATE_B, &[("x", Some("c_x"))]));
        assert!(app.steering().is_empty(), "{job}");
    }
    app.close_item();
    assert_eq!(app.steering(), ["↳ parent one"]);
}

#[test]
fn going_home_or_losing_the_link_gives_the_attached_queue_back() {
    let (mut app, _clock) = view_with_parent(&[("parent one", Some("c_p1"))]);
    app.drop_items();
    assert_eq!(app.steering(), ["↳ parent one"]);
    let (mut app, _clock) = view_with_parent(&[("parent one", Some("c_p1"))]);
    app.leave_item();
    assert_eq!(app.steering(), ["↳ parent one"]);
}

#[test]
fn a_delegates_applied_steer_joins_its_turn_and_empties_the_queue_echo() {
    let (mut app, _clock) = view();
    app.on_line(queue_line(DELEGATE_A, &[("use the parser", Some("c_d1"))]));
    let line = |kind: &str, payload: Value| {
        let Line::Session(mut envelope) = session_line(kind, payload) else {
            panic!("a session line")
        };
        envelope.session_id = contract::SessionId(DELEGATE_A.to_owned());
        Line::Session(envelope)
    };
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]}),
    ));
    app.on_line(line(
        "steering_applied",
        json!({"content": [{"type": "text", "text": "use the parser"}], "source": "driver"}),
    ));
    app.on_line(queue_line(DELEGATE_A, &[]));
    assert!(app.steering().is_empty());
    let shown = screen(&app);
    assert!(shown.contains("steer"), "{shown}");
    assert!(shown.contains("use the parser"), "{shown}");
    assert!(!shown.contains("Steering"), "{shown}");
}
