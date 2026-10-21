//! Tests for `/copy` (#3013): the newest reply in the conversation on
//! screen goes out through the copy path, as the model wrote it.

use super::commands_testkit::{attached, connected, enter, session_line, turn_starts};
use crate::app::copy::{LastReply, last_reply};
use crate::app::{App, Effect};
use crate::link::Line;
use crate::turn::Turn;
use serde_json::{Value, json};

/// A line of message `action` on the session.
fn message_line(kind: &str, action: &str, payload: Value) -> Line {
    let mut line = session_line(kind, payload);
    if let Line::Session(envelope) = &mut line {
        envelope.action_id = Some(contract::ActionId(action.to_owned()));
    }
    line
}

/// Folds message `action` whose text parts are `parts`, in order.
fn message(app: &mut App, action: &str, parts: &[&str]) {
    app.on_line(message_line("assistant_message_started", action, json!({})));
    for part in parts {
        app.on_line(message_line(
            "text_completed",
            action,
            json!({"text": part}),
        ));
    }
    app.on_line(message_line(
        "assistant_message_completed",
        action,
        json!({"outcome": "completed"}),
    ));
}

/// Ends the running turn.
fn turn_ends(app: &mut App) {
    app.on_line(session_line(
        "turn_completed",
        json!({"outcome": "completed"}),
    ));
}

#[test]
fn copy_copies_the_newest_reply_as_written_and_shows_copied() {
    let mut app = attached();
    turn_starts(&mut app);
    message(&mut app, "a_1", &["the older reply"]);
    turn_ends(&mut app);
    turn_starts(&mut app);
    message(&mut app, "a_2", &["The **newest** reply:\n\n- one\n- two"]);
    turn_ends(&mut app);
    assert!(!app.copied());
    assert_eq!(
        enter(&mut app, "/copy"),
        Effect::Copy("The **newest** reply:\n\n- one\n- two".to_owned())
    );
    assert!(app.copied());
    assert_eq!(app.draft(), "");
}

#[test]
fn copy_joins_the_newest_messages_text_parts_and_leaves_older_messages() {
    let mut app = attached();
    turn_starts(&mut app);
    message(&mut app, "a_1", &["an earlier step"]);
    message(&mut app, "a_2", &["first part", "second part"]);
    assert_eq!(
        enter(&mut app, "/copy"),
        Effect::Copy("first part\n\nsecond part".to_owned())
    );
}

#[test]
fn copy_with_no_reply_says_so_and_copies_nothing() {
    let mut app = attached();
    turn_starts(&mut app);
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    assert_eq!(app.notice(), Some("No reply to copy yet."));
    assert!(!app.copied());
    assert_eq!(app.draft(), "");
    // Home holds no conversation either.
    let mut app = connected();
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    assert_eq!(app.notice(), Some("No reply to copy yet."));
}

#[test]
fn a_dropped_page_before_any_reply_leaves_the_reply_paged_out() {
    let empty: &[Turn] = &[];
    assert_eq!(
        last_reply([Some(empty), None].into_iter()),
        LastReply::PagedOut
    );
    assert_eq!(last_reply([None].into_iter()), LastReply::PagedOut);
    assert_eq!(
        last_reply([Some(empty), Some(empty)].into_iter()),
        LastReply::NoReply
    );
    assert_eq!(last_reply(std::iter::empty()), LastReply::NoReply);
}
