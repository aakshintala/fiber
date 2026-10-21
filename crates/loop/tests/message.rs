//! A session message from another session (`docs/tools.md`, "Messaging other
//! sessions"): between turns it starts a turn, during a turn it applies as
//! steering at the next step boundary, framed with the sender's id and name.
//! A `steer_drop` never removes one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::mpsc;

use contract::inbox::{Ack, Answer, Delivery, SessionMessage};
use contract::provider::Input;
use contract::{CommandId, Envelope, ErrorCode, SessionId};
use fakes::Scripted;
use serde_json::json;

use support::{Session, kinds, rejected};

const FROM: &str = "s_0123456789abcdef";
const COMMAND: &str = "c_m1";
const TEXT: &str = "Tests pass on main.";
const NAMED: &str = "Fiber: a message from session s_0123456789abcdef, named \"builder\". Another session wrote it, not the person.\n\nTests pass on main.";
const UNNAMED: &str = "Fiber: a message from session s_0123456789abcdef. Another session wrote it, not the person.\n\nTests pass on main.";
const STALE_STEER: &str = "That steering message was already applied, or was never queued.";

/// A session message from `FROM`, sent by `command`, saying `text`, with
/// `name` as the sender's latest `session_named` name, `parent` as the
/// sender's parent, and answered on `ack`.
fn session_message(
    command: &str,
    parent: Option<&str>,
    name: Option<&str>,
    text: &str,
    ack: Ack,
) -> Delivery {
    Delivery::Message(
        SessionMessage {
            from_session_id: SessionId(FROM.into()),
            parent_session_id: parent.map(|parent| SessionId(parent.into())),
            command_id: CommandId(command.into()),
            text: text.into(),
            name: name.map(str::to_owned),
        },
        ack,
    )
}

fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (Ack(Box::new(move |answer| tx.send(answer).unwrap())), rx)
}

fn take(rx: &mpsc::Receiver<Answer>) -> Answer {
    rx.try_recv().expect("the command was answered")
}

/// A `steer_drop` naming `command`, answering on the returned channel.
fn drop_of(command: &str) -> (Delivery, mpsc::Receiver<Answer>) {
    let (ack, answers) = capture();
    (Delivery::SteerDrop(CommandId(command.into()), ack), answers)
}

fn plain() -> Vec<&'static str> {
    vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

/// The user messages `request` sends, in order.
fn users(request: &contract::provider::ModelRequest) -> Vec<String> {
    request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.clone()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect()
}

#[test]
fn a_session_message_to_an_idle_loop_starts_a_turn_framed_with_its_senders_name() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (ack, answers) = capture();
    session
        .inbox
        .send(session_message(COMMAND, None, Some("builder"), TEXT, ack))
        .unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    // The take answered the message once it joined the turn's input.
    assert_eq!(take(&answers), Ok(None));
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(
        lines[3].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": NAMED}],
        }])
    );
    let requests = session.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(users(&requests[0]).last(), Some(&NAMED.to_owned()));
}

#[test]
fn a_session_message_from_an_unnamed_sender_is_framed_by_its_id_alone() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (ack, answers) = capture();
    session
        .inbox
        .send(session_message(COMMAND, None, None, TEXT, ack))
        .unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    assert_eq!(take(&answers), Ok(None));
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(
        lines[3].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": UNNAMED}],
        }])
    );
    let requests = session.requests();
    assert_eq!(users(&requests[0]).last(), Some(&UNNAMED.to_owned()));
}

#[test]
fn a_session_message_mid_turn_is_applied_as_steering_at_the_next_step() {
    let (ack, answers) = capture();
    let mut session = Session::injecting(
        vec![Scripted::text("Hi."), Scripted::text("Steered.")],
        vec![session_message(COMMAND, None, Some("builder"), TEXT, ack)],
    );
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    // The running take answered the message once it queued.
    assert_eq!(take(&answers), Ok(None));
    let lines = session.lines();
    let expected = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "steering_queue",
        "step_started",
        "steering_queue",
        "steering_applied",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ];
    assert_eq!(kinds(&lines), expected);
    let queued: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "steering_queue")
        .collect();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].payload["messages"][0]["source"], "session");
    assert_eq!(queued[0].payload["messages"][0]["from_session_id"], FROM);
    assert_eq!(queued[0].payload["messages"][0]["command_id"], COMMAND);
    assert_eq!(queued[1].payload["messages"], json!([]));
    let applied = lines
        .iter()
        .find(|line| line.kind == "steering_applied")
        .expect("the message applies");
    assert_eq!(applied.payload["source"], "session");
    assert_eq!(applied.payload["from_session_id"], FROM);
    assert_eq!(applied.payload["command_id"], COMMAND);
    assert_eq!(applied.payload["content"][0]["text"], json!(NAMED));
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[1]).last(), Some(&NAMED.to_owned()));
}

#[test]
fn a_name_holding_a_placeholder_is_not_expanded() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (ack, answers) = capture();
    session
        .inbox
        .send(session_message(COMMAND, None, Some("{text}"), TEXT, ack))
        .unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    assert_eq!(take(&answers), Ok(None));
    let lines = session.lines();
    assert_eq!(
        lines[3].payload["input"][0]["content"][0]["text"],
        "Fiber: a message from session s_0123456789abcdef, named \"{text}\". Another session wrote it, not the person.\n\nTests pass on main."
    );
}

#[test]
fn a_steer_drop_naming_a_session_message_while_idle_is_stale_and_the_message_runs() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (ack, message_answers) = capture();
    session
        .inbox
        .send(session_message(COMMAND, None, Some("builder"), TEXT, ack))
        .unwrap();
    let (drop, answers) = drop_of(COMMAND);
    session.inbox.send(drop).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    assert_eq!(take(&message_answers), Ok(None));
    assert_eq!(
        take(&answers),
        rejected(ErrorCode::StaleRequest, STALE_STEER)
    );
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(
        lines[3].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": NAMED}],
        }])
    );
}

#[test]
fn a_steer_drop_naming_a_queued_session_message_mid_turn_is_stale_and_the_message_applies() {
    let (drop_ack, answers) = capture();
    let (message_ack, message_answers) = capture();
    let mut session = Session::injecting(
        vec![Scripted::text("Hi."), Scripted::text("Steered.")],
        vec![
            session_message(COMMAND, None, Some("builder"), TEXT, message_ack),
            Delivery::SteerDrop(CommandId(COMMAND.into()), drop_ack),
        ],
    );
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    assert_eq!(take(&message_answers), Ok(None));
    assert_eq!(
        take(&answers),
        rejected(ErrorCode::StaleRequest, STALE_STEER)
    );
    let lines = session.lines();
    let applied = lines
        .iter()
        .find(|line| line.kind == "steering_applied")
        .expect("the message applies");
    assert_eq!(applied.payload["command_id"], COMMAND);
    assert_eq!(applied.payload["content"][0]["text"], json!(NAMED));
}
