//! The in-process driver (`docs/extensions.md`, "Host calls"): one driver
//! command each, answered through the host call's acknowledgement, with a
//! fake inbox standing in for the loop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::io;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{CommandResult, ToolInfo, ToolSource, ToolState};
use contract::extension::Drive;
use contract::inbox::{Ack, Answer, Delivery, Rejection};
use contract::shapes::Origin;
use contract::{ErrorCode, SessionId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::{Map, Value};

use crate::Session;

/// A hang bound for one answer or delivery.
const DEADLINE: Duration = Duration::from_secs(5);

struct Opened {
    _temp: fakes::TempDir,
    log: Arc<Log>,
    session: Session,
}

fn open(tools: Vec<ToolInfo>) -> Opened {
    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = SessionId(crate::mint("s_"));
    let dir = sessions.join(&id.0);
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let session = Session::open(&home, &dir, &log, timed, tools, Box::new(io::sink())).unwrap();
    Opened {
        _temp: temp,
        log,
        session,
    }
}

#[track_caller]
fn close_within(session: Session, log: Arc<Log>) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = tx.send(()) {}
    });
    Deadline::after(DEADLINE).recv(&rx).expect("close returned");
}

fn tool() -> ToolInfo {
    ToolInfo {
        name: "read".into(),
        source: ToolSource::Builtin,
        state: ToolState::Full,
        bytes: 12,
        tokens: None,
    }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn text_args(text: &str) -> Value {
    serde_json::json!({"content": [{"type": "text", "text": text}]})
}

/// Drives one command; the host call's answer arrives on the returned channel.
fn drive(driver: &Arc<dyn Drive>, command: &str, args: Value) -> mpsc::Receiver<Answer> {
    let (tx, rx) = mpsc::channel();
    driver.drive(
        "fiber.test/a",
        command,
        object(args),
        Ack(Box::new(move |answer| tx.send(answer).unwrap())),
    );
    rx
}

fn rejected(answer: Answer) -> Rejection {
    match answer {
        Err(rejection) => rejection,
        Ok(_) => panic!("the drive was accepted: {answer:?}"),
    }
}

#[test]
fn drive_steer_is_accepted_with_an_extension_sender() {
    let opened = open(vec![]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let outcome = drive(&driver, "steer", text_args("use the other file"));
            let Delivery::Steer(message, ack) = Deadline::after(DEADLINE).recv(&inbox).expect("the steer is delivered") else {
                panic!("a steer is delivered");
            };
            // drive_steer_carries_extension_sender: the message carries
            // `source: extension` and the extension's name, not `driver`.
            assert!(matches!(&message.sender.origin, Origin::Extension { extension } if extension == "fiber.test/a"), "{:?}", message.sender);
            assert!(
                message.sender.command_id.as_ref().is_some_and(|id| id.0.starts_with("c_")),
                "{:?}",
                message.sender
            );
            ack.0(Ok(None));
            let answered = Deadline::after(DEADLINE).recv(&outcome);
            assert!(
                matches!(answered, Ok(Ok(None))),
                "an accepted steer answers no result: {answered:?}"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn drive_prompt_answered_busy_rejects_busy() {
    let opened = open(vec![]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let outcome = drive(&driver, "prompt", text_args("hi"));
            let Delivery::Prompt(message, ack) = Deadline::after(DEADLINE).recv(&inbox).expect("the prompt is delivered") else {
                panic!("a prompt is delivered");
            };
            assert!(matches!(&message.sender.origin, Origin::Extension { extension } if extension == "fiber.test/a"), "{:?}", message.sender);
            ack.0(Err(Rejection {
                code: ErrorCode::Busy,
                message: "A turn is running; send `steer` to add to it.".into(),
            }));
            let rejection = rejected(Deadline::after(DEADLINE).recv(&outcome).expect("the drive is answered"));
            assert_eq!(rejection.code, ErrorCode::Busy);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn drive_tools_answers_with_its_result() {
    let opened = open(vec![tool()]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let outcome = drive(&driver, "tools", Value::Object(Map::new()));
            match Deadline::after(DEADLINE)
                .recv(&outcome)
                .expect("the drive is answered")
            {
                Ok(Some(CommandResult::Tools { tools })) => {
                    assert_eq!(tools.len(), 1);
                    assert_eq!(tools[0].name, "read");
                }
                other => panic!("tools answers with its result: {other:?}"),
            }
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn drive_approval_reply_is_rejected_before_the_inbox() {
    let opened = open(vec![]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let outcome = drive(
                &driver,
                "reply",
                serde_json::json!({"request_id": "r_1", "decision": "allow"}),
            );
            let rejection = rejected(
                Deadline::after(DEADLINE)
                    .recv(&outcome)
                    .expect("the drive is answered"),
            );
            // drive_approval_reply_is_rejected: an extension never answers an approval.
            assert_eq!(rejection.code, ErrorCode::InvalidArguments);
            assert_eq!(rejection.message, "An extension never answers an approval.");
            assert!(
                Deadline::after(Duration::from_millis(100))
                    .recv(&inbox)
                    .is_err(),
                "the refused reply never reaches the inbox"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn drive_subscribe_is_rejected_as_a_second_one() {
    let opened = open(vec![]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let outcome = drive(&driver, "subscribe", serde_json::json!({"level": "full"}));
            let rejection = rejected(
                Deadline::after(DEADLINE)
                    .recv(&outcome)
                    .expect("the drive is answered"),
            );
            assert_eq!(rejection.code, ErrorCode::InvalidArguments);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn drive_unknown_command_is_rejected() {
    let opened = open(vec![]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let outcome = drive(&driver, "frobnicate", Value::Object(Map::new()));
            let rejection = rejected(
                Deadline::after(DEADLINE)
                    .recv(&outcome)
                    .expect("the drive is answered"),
            );
            // drive_unknown_command_is_rejected: no `reply` to an unknown name.
            assert_eq!(rejection.code, ErrorCode::UnknownCommand);
            assert_eq!(
                rejection.message,
                "`frobnicate` is not built in this Fiber yet."
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn drive_after_close_answers_closing() {
    let opened = open(vec![tool()]);
    let driver = opened.session.driver();
    close_within(opened.session, opened.log);
    let (tx, rx) = mpsc::channel();
    driver.drive(
        "fiber.test/a",
        "tools",
        Map::new(),
        Ack(Box::new(move |answer| tx.send(answer).unwrap())),
    );
    let rejection = rejected(
        Deadline::after(DEADLINE)
            .recv(&rx)
            .expect("the drive is answered"),
    );
    assert_eq!(rejection.code, ErrorCode::Closing);
}

#[test]
fn drive_after_close_answers_closing_with_a_retained_handle() {
    let opened = open(vec![tool()]);
    let driver = opened.session.driver();
    // A retained handle keeps the gate alive past `Session::close`: the
    // stopped flag, not the upgrade, answers `closing`.
    let _stopper = opened.session.stopper();
    close_within(opened.session, opened.log);
    let (tx, rx) = mpsc::channel();
    driver.drive(
        "fiber.test/a",
        "tools",
        Map::new(),
        Ack(Box::new(move |answer| tx.send(answer).unwrap())),
    );
    let rejection = rejected(
        Deadline::after(DEADLINE)
            .recv(&rx)
            .expect("the drive is answered"),
    );
    assert_eq!(rejection.code, ErrorCode::Closing);
}

/// A fake door for `reply`: takes the ask `r_held` and answers its ack
/// itself; hands every other reply back for the loop.
struct AskDoor;

impl contract::extension::ExtensionDoor for AskDoor {
    fn command(&self, name: &str, _text: &str) -> Result<Box<dyn FnOnce() + Send>, Rejection> {
        Err(Rejection {
            code: ErrorCode::UnknownCommand,
            message: format!("`{name}` names no extension command."),
        })
    }

    fn seal(&self) {}

    fn reply(
        &self,
        reply: contract::commands::Reply,
        ack: Ack,
    ) -> Option<(contract::commands::Reply, Ack)> {
        if reply.request_id.0 == "r_held" {
            // reply_for_a_held_ask_never_reaches_the_inbox: the answer is
            // what the holder does with the ack.
            ack.0(Ok(None));
            None
        } else {
            Some((reply, ack))
        }
    }
}

fn reply_args(request: &str) -> Value {
    serde_json::json!({"request_id": request, "confirmed": true})
}

#[test]
fn reply_for_a_held_ask_never_reaches_the_inbox() {
    let opened = open(vec![]);
    opened.session.extensions(Arc::new(AskDoor));
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let outcome = drive(&driver, "reply", reply_args("r_held"));
            let answered = Deadline::after(DEADLINE)
                .recv(&outcome)
                .expect("the reply is answered");
            assert!(
                matches!(answered, Ok(None)),
                "the answer is what the holder does with the ack: {answered:?}"
            );
            assert!(
                inbox.try_recv().is_err(),
                "a held reply never reaches the inbox"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn reply_handed_back_reaches_the_inbox() {
    let opened = open(vec![]);
    opened.session.extensions(Arc::new(AskDoor));
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let outcome = drive(&driver, "reply", reply_args("r_other"));
            let Delivery::Reply(reply, ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the reply is delivered")
            else {
                panic!("a handed-back reply is delivered");
            };
            assert_eq!(reply.request_id.0, "r_other");
            ack.0(Ok(None));
            let answered = Deadline::after(DEADLINE)
                .recv(&outcome)
                .expect("the reply is answered");
            assert!(
                matches!(answered, Ok(None)),
                "the loop accepts the handed-back reply: {answered:?}"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn reply_with_no_door_reaches_the_inbox() {
    let opened = open(vec![]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let outcome = drive(&driver, "reply", reply_args("r_other"));
            let Delivery::Reply(reply, ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the reply is delivered")
            else {
                panic!("a reply with no door is delivered");
            };
            assert_eq!(reply.request_id.0, "r_other");
            ack.0(Ok(None));
            let answered = Deadline::after(DEADLINE)
                .recv(&outcome)
                .expect("the reply is answered");
            assert!(
                matches!(answered, Ok(None)),
                "the loop accepts the reply: {answered:?}"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn drive_offer_reply_is_rejected_before_the_inbox() {
    let opened = open(vec![]);
    let driver = opened.session.driver();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let outcome = drive(
                &driver,
                "reply",
                serde_json::json!({"request_id": "r_1", "decisions": ["approve"]}),
            );
            let rejection = rejected(
                Deadline::after(DEADLINE)
                    .recv(&outcome)
                    .expect("the drive is answered"),
            );
            assert_eq!(rejection.code, ErrorCode::InvalidArguments);
            assert_eq!(
                rejection.message,
                "An extension never answers an offer of a repository's code."
            );
            assert!(
                Deadline::after(Duration::from_millis(100))
                    .recv(&inbox)
                    .is_err(),
                "the refused reply never reaches the inbox"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}
