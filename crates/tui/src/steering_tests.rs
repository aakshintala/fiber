//! Tests for the steering queue and its selection.

use super::Steering;
use crate::input::Draft;
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
