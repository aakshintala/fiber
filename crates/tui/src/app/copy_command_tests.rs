//! Tests for `/copy` (#3013): the newest reply in the conversation on
//! screen goes out through the copy path, as the model wrote it.

use super::commands_testkit::{attached, connected, enter, now, session_line, turn_starts};
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

#[test]
fn copy_waits_for_the_opening_pass_and_copies_once_it_ends() {
    let mut app = attached();
    turn_starts(&mut app);
    message(&mut app, "a_1", &["the reply"]);
    app.reopen_pending(contract::SessionId(
        super::commands_testkit::SESSION.to_owned(),
    ));
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    assert_eq!(app.notice(), None);
    assert!(!app.copied());
    // The frames during the pass copy nothing.
    assert_eq!(app.take_owed_copy(), None);
    app.passes.clear();
    assert_eq!(app.take_owed_copy(), Some("the reply".to_owned()));
    assert!(app.copied());
    // It copies once.
    assert_eq!(app.take_owed_copy(), None);
}

#[test]
fn leaving_the_session_drops_a_copy_waiting_on_its_pass() {
    let mut app = attached();
    turn_starts(&mut app);
    message(&mut app, "a_1", &["the reply"]);
    app.reopen_pending(contract::SessionId(
        super::commands_testkit::SESSION.to_owned(),
    ));
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    assert_eq!(enter(&mut app, "/home"), Effect::None);
    app.passes.clear();
    assert_eq!(app.take_owed_copy(), None);
    assert!(!app.copied());
    assert_eq!(app.notice(), None);
}

#[test]
fn a_delegates_transcript_copies_its_own_reply_never_the_parents() {
    use crate::app::items::testkit::{DELEGATE_A, home, opened, opened_item, start_delegate};
    let mut app = home();
    opened(&mut app);
    turn_starts(&mut app);
    message(&mut app, "a_1", &["the parent's reply"]);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    assert_eq!(app.notice(), Some("No reply to copy yet."));
    assert!(!app.copied());
}

#[test]
fn a_view_with_no_transcript_has_no_reply_to_copy() {
    use crate::app::items::testkit::{home, opened, opened_item, start_job};
    let mut app = home();
    opened(&mut app);
    turn_starts(&mut app);
    message(&mut app, "a_1", &["the parent's reply"]);
    start_job(&mut app, "j_1");
    opened_item(&mut app, "j_1");
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    assert_eq!(app.notice(), Some("No reply to copy in this view."));
    assert!(!app.copied());
}

#[test]
fn closing_or_opening_a_view_drops_a_copy_waiting_on_a_pass() {
    use crate::app::items::testkit::{
        DELEGATE_A, home, opened, opened_item, start_delegate, start_job,
    };
    let mut app = home();
    opened(&mut app);
    turn_starts(&mut app);
    message(&mut app, "a_1", &["the parent's reply"]);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_job(&mut app, "j_2");
    // Waiting in a delegate's transcript, then back to the parent.
    opened_item(&mut app, "j_1");
    app.reopen_pending(contract::SessionId(DELEGATE_A.to_owned()));
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    app.close_item();
    app.passes.clear();
    assert_eq!(app.take_owed_copy(), None);
    assert_eq!(app.notice(), None);
    // Waiting on the parent, then into a job's view.
    app.reopen_pending(contract::SessionId(
        super::commands_testkit::SESSION.to_owned(),
    ));
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    opened_item(&mut app, "j_2");
    app.passes.clear();
    assert_eq!(app.take_owed_copy(), None);
    assert!(!app.copied());
    assert_eq!(app.notice(), None);
}

#[test]
fn a_key_a_click_or_a_lost_connection_drops_a_copy_waiting_on_a_pass() {
    let session = contract::SessionId(super::commands_testkit::SESSION.to_owned());
    // A key.
    let mut app = attached();
    turn_starts(&mut app);
    message(&mut app, "a_1", &["the reply"]);
    app.reopen_pending(session.clone());
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    assert_eq!(app.on_key(crate::keys::Key::End, now()), Effect::None);
    app.passes.clear();
    assert_eq!(app.take_owed_copy(), None);
    assert_eq!(app.notice(), None);
    // A click: the loop hides "Copied" on every left press.
    app.reopen_pending(session.clone());
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    app.clear_copied();
    app.passes.clear();
    assert_eq!(app.take_owed_copy(), None);
    // An edit.
    app.reopen_pending(session.clone());
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    app.on_edit(crate::keys::Edit::Paste("x".to_owned()));
    app.passes.clear();
    assert_eq!(app.take_owed_copy(), None);
    // A lost connection.
    app.draft.clear();
    app.reopen_pending(session);
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    app.disconnected();
    assert_eq!(app.take_owed_copy(), None);
    assert!(!app.copied());
    // Only the connection's own notice shows.
    assert_eq!(app.notice(), Some("Connection lost."));
}

#[test]
fn a_session_attaching_drops_a_copy_waiting_on_a_pass() {
    // On home, with an earlier session's pass still running.
    let mut app = connected();
    app.reopen_pending(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    assert_eq!(enter(&mut app, "/copy"), Effect::None);
    app.attach(contract::SessionId(
        super::commands_testkit::SESSION.to_owned(),
    ));
    turn_starts(&mut app);
    message(&mut app, "a_1", &["the new session's reply"]);
    app.passes.clear();
    assert_eq!(app.take_owed_copy(), None);
    assert!(!app.copied());
    assert_eq!(app.notice(), None);
}
