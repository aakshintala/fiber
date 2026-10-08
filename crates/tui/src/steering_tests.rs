//! Tests for the steering queue and its selection.

use super::Steering;
use crate::input::Draft;
use contract::clock::Clock;
use contract::events::SteeringQueue;

/// A draft holding `text`, the cursor at its end.
fn typed(text: &str) -> Draft {
    let mut draft = Draft::default();
    draft.set(text);
    draft
}

/// A queue of `rows`: each its text and its `steer` id, `None` for
/// Fiber's own message.
fn queue(rows: &[(&str, Option<&str>)]) -> SteeringQueue {
    let messages: Vec<serde_json::Value> = rows
        .iter()
        .map(|(text, id)| {
            let mut message = serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "source": "driver",
            });
            if let (Some(id), Some(object)) = (id, message.as_object_mut()) {
                object.insert("command_id".to_owned(), serde_json::json!(id));
            }
            message
        })
        .collect();
    serde_json::from_value(serde_json::json!({ "messages": messages }))
        .unwrap_or(SteeringQueue { messages: vec![] })
}

/// A queue of three selectable rows, `one` oldest.
fn three(draft: &mut Draft) -> Steering {
    let mut steering = Steering::default();
    steering.fold(
        &queue(&[
            ("one", Some("c_1")),
            ("two", Some("c_2")),
            ("three", Some("c_3")),
        ]),
        draft,
    );
    steering
}

#[test]
fn up_selects_the_newest_then_older_and_stops_at_the_oldest() {
    let mut draft = typed("mine");
    let mut steering = three(&mut draft);
    steering.up(&mut draft);
    assert_eq!(draft.expand(), "three");
    steering.up(&mut draft);
    assert_eq!(draft.expand(), "two");
    steering.up(&mut draft);
    assert_eq!(draft.expand(), "one");
    draft.insert('!');
    // At the oldest, Up keeps the row and its edit.
    steering.up(&mut draft);
    assert_eq!(draft.expand(), "one!");
    assert_eq!(steering.lines(20), ["▸ one", "↳ two", "↳ three"]);
}

#[test]
fn down_moves_newer_and_past_the_newest_restores_the_draft() {
    let mut draft = typed("mine");
    let mut steering = three(&mut draft);
    // Nothing selected: Down does nothing.
    steering.down(&mut draft);
    assert_eq!(draft.expand(), "mine");
    assert!(!steering.is_selected());
    steering.up(&mut draft);
    steering.up(&mut draft);
    assert_eq!(draft.expand(), "two");
    steering.down(&mut draft);
    assert_eq!(draft.expand(), "three");
    steering.down(&mut draft);
    assert_eq!(draft.expand(), "mine");
    assert!(!steering.is_selected());
}

#[test]
fn fibers_own_message_shows_but_cannot_be_selected() {
    let mut draft = Draft::default();
    let mut steering = Steering::default();
    steering.fold(
        &queue(&[("first", Some("c_1")), ("from fiber", None)]),
        &mut draft,
    );
    assert_eq!(steering.lines(20), ["↳ first", "↳ from fiber"]);
    steering.up(&mut draft);
    assert_eq!(draft.expand(), "first");
    assert!(!steering.select(1, &mut draft));
    assert_eq!(draft.expand(), "first");
    assert_eq!(steering.lines(20), ["▸ first", "↳ from fiber"]);
    // Down from the only selectable row clears, skipping Fiber's.
    steering.down(&mut draft);
    assert!(!steering.is_selected());
    assert_eq!(steering.to_drop(), ["c_1"]);
    assert_eq!(steering.id_at(1), None);
    assert_eq!(steering.id_at(0).as_deref(), Some("c_1"));
}

#[test]
fn a_row_leaving_the_queue_clears_the_selection_and_restores() {
    let mut draft = typed("mine");
    let mut steering = three(&mut draft);
    steering.up(&mut draft);
    // Reordered, the selection stays on the same message.
    steering.fold(
        &queue(&[("three", Some("c_3")), ("one", Some("c_1"))]),
        &mut draft,
    );
    assert!(steering.is_selected());
    assert_eq!(steering.lines(20), ["▸ three", "↳ one"]);
    steering.fold(&queue(&[("one", Some("c_1"))]), &mut draft);
    assert!(!steering.is_selected());
    assert_eq!(draft.expand(), "mine");
    steering.fold(&queue(&[]), &mut draft);
    assert!(steering.lines(20).is_empty());
}

#[test]
fn amend_hands_back_the_id_and_the_stash() {
    let mut draft = typed("mine");
    let mut steering = three(&mut draft);
    assert!(steering.amend().is_none());
    steering.up(&mut draft);
    assert_eq!(
        steering.amend().map(|(id, stash)| (id, stash.expand())),
        Some(("c_3".to_owned(), "mine".to_owned()))
    );
    assert!(!steering.is_selected());
    // The draft is the caller's to send.
    assert_eq!(draft.expand(), "three");
}

#[test]
fn drop_takes_the_selected_row_or_every_row() {
    let mut draft = Draft::default();
    let mut steering = three(&mut draft);
    assert_eq!(steering.to_drop(), ["c_1", "c_2", "c_3"]);
    steering.up(&mut draft);
    assert_eq!(steering.to_drop(), ["c_3"]);
    assert!(Steering::default().to_drop().is_empty());
}

#[test]
fn clear_restores_only_with_a_selection() {
    let mut draft = typed("mine");
    let mut steering = three(&mut draft);
    steering.clear(&mut draft);
    assert_eq!(draft.expand(), "mine");
    assert!(steering.select(1, &mut draft));
    assert_eq!(draft.expand(), "two");
    // Selecting another row keeps the first stash.
    assert!(steering.select(0, &mut draft));
    assert!(!steering.select(7, &mut draft));
    steering.clear(&mut draft);
    assert_eq!(draft.expand(), "mine");
}

#[test]
fn rows_are_cut_to_the_width_on_one_line() {
    let mut draft = Draft::default();
    let mut steering = Steering::default();
    steering.fold(
        &queue(&[("a long message\nover lines", Some("c_1"))]),
        &mut draft,
    );
    // The last column is the ✕'s.
    assert_eq!(steering.lines(10), ["↳ a long "]);
    assert_eq!(steering.lines(40), ["↳ a long message over lines"]);
}

/// A queue whose middle row holds an image: `one`, a picture, `three`.
fn with_image(draft: &mut Draft) -> Steering {
    let messages = vec![
        serde_json::json!({"content": [{"type": "text", "text": "one"}],
            "source": "driver", "command_id": "c_1"}),
        serde_json::json!({"content": [{"type": "image", "path": "artifacts/a.png",
            "mime_type": "image/png", "width": 1, "height": 1}],
            "source": "driver", "command_id": "c_2"}),
        serde_json::json!({"content": [{"type": "text", "text": "three"}],
            "source": "driver", "command_id": "c_3"}),
    ];
    let queue: SteeringQueue =
        serde_json::from_value(serde_json::json!({ "messages": messages }))
            .unwrap_or(SteeringQueue { messages: vec![] });
    let mut steering = Steering::default();
    steering.fold(&queue, draft);
    steering
}

#[test]
fn alt_up_and_alt_down_skip_a_queued_row_with_an_image() {
    // T1: the image row is never selected, stepping either way.
    let mut draft = typed("mine");
    let mut steering = with_image(&mut draft);
    steering.up(&mut draft);
    assert_eq!(draft.expand(), "three");
    steering.up(&mut draft);
    assert_eq!(draft.expand(), "one");
    steering.down(&mut draft);
    assert_eq!(draft.expand(), "three");
    steering.down(&mut draft);
    assert_eq!(draft.expand(), "mine");
    assert!(!steering.is_selected());
}

#[test]
fn a_click_does_not_select_a_row_with_an_image() {
    // T2: selecting the image row fails, and ⌥X still drops it with the
    // rest.
    let mut draft = typed("mine");
    let mut steering = with_image(&mut draft);
    assert!(!steering.select(1, &mut draft));
    assert_eq!(draft.expand(), "mine");
    assert!(!steering.is_selected());
    assert_eq!(steering.to_drop(), ["c_1", "c_2", "c_3"]);
    assert_eq!(steering.id_at(1).as_deref(), Some("c_2"));
    assert_eq!(steering.droppable(), [true, true, true]);
    assert!(steering.select(2, &mut draft));
    assert_eq!(steering.to_drop(), ["c_3"]);
}

/// An app connected to the hub and attached to a session.
fn attached() -> crate::app::App {
    let mut app = crate::app::App::new(std::path::PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    assert!(app.on_line(crate::link::Line::Hub(hello)).is_empty());
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app
}

/// The steering queue of `queue` on the attached session.
fn feed(app: &mut crate::app::App, queue: &SteeringQueue) {
    let payload =
        serde_json::to_value(queue).unwrap_or(serde_json::json!({"messages": []}));
    let line = crate::link::Line::Session(contract::Envelope {
        kind: "steering_queue".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    });
    assert!(app.on_line(line).is_empty());
}

#[test]
fn an_amend_sends_the_drafts_images() {
    // T3: an image pasted while amending goes out with the row's text,
    // and the stashed draft comes back.
    use crate::app::Effect;
    use crate::keys::Key;
    let mut app = attached();
    feed(&mut app, &queue(&[("one", Some("c_1")), ("two", Some("c_2"))]));
    app.select_steering(0);
    assert_eq!(app.input().expand(), "one");
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlV, now), Effect::ReadImage(0));
    app.on_image(0, Ok("AAA".to_owned()));
    let lines: Vec<serde_json::Value> = match app.on_key(Key::Enter, now) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => panic!("Enter amends"),
    };
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["command"], "steer_drop");
    assert_eq!(lines[1]["command"], "steer");
    assert_eq!(
        lines[1]["args"],
        serde_json::json!({"content": [{"type": "text", "text": "one"},
            {"type": "image", "data": "AAA", "mime_type": "image/png"}]})
    );
    assert_eq!(app.input().expand(), "");
}
