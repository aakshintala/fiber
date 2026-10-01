//! The process boundary the loop writes (`docs/events.md`, "Process
//! boundary"): `fiber_started`, and `fiber_exited` folded from the log.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use contract::events::{
    AssistantMessageCompleted, Empty, Event, InputItem, MessageOutcome, TurnCompleted, TurnOutcome,
    TurnStarted, UsageRecorded,
};
use contract::shapes::{ContentPart, Failure, Origin, Sender, Tokens};
use contract::{ActionId, CommandId, ErrorCode, GenerationId, SessionId, TurnId};
use log::Log;
use r#loop::{fiber_exited, fiber_started};
use serde_json::Value;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A session log in a temporary directory, removed on drop.
struct Session {
    root: PathBuf,
    dir: PathBuf,
    log: Log,
}

impl Session {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fiber-process-{}-{n}", std::process::id()));
        fs::remove_dir_all(&root).unwrap_or(());
        let log = Log::create(&root, SessionId("s_1".into())).unwrap();
        Self {
            dir: root.join("s_1"),
            root,
            log,
        }
    }

    fn append(&self, event: &Event, action: Option<&str>) {
        let action = action.map(|a| ActionId(a.into()));
        self.log
            .append(event, Some(TurnId("t_1".into())), action)
            .unwrap();
    }

    /// Writes `fiber_exited` and returns its exit code and payload.
    fn exit(&self, ran: Result<(), Failure>) -> (i32, Value) {
        let code = fiber_exited(&self.log, &self.dir, ran).unwrap();
        let lines = log::read(&self.dir).unwrap();
        let last = lines.last().unwrap();
        assert_eq!(last.kind, "fiber_exited");
        assert!(last.turn_id.is_none());
        (code, Value::Object(last.payload.clone()))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or(());
    }
}

fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.into(),
        retry_after: None,
        provider: None,
    }
}

fn usage(output: u64, cost: Option<f64>, subscription: Option<bool>) -> Event {
    Event::UsageRecorded(UsageRecorded {
        generation_id: GenerationId("g".into()),
        model: "fake/m".into(),
        tokens: Tokens {
            input: 10,
            cache_read: 4,
            cache_write: BTreeMap::from([("1h".to_owned(), 2)]),
            output,
        },
        web_searches: None,
        cost,
        subscription,
        extension: None,
        origin_session_id: None,
    })
}

fn message(text: &str) -> Event {
    Event::AssistantMessageCompleted(AssistantMessageCompleted {
        outcome: MessageOutcome::Completed,
        text: text.into(),
        error: None,
        attempt: None,
    })
}

fn turn_started() -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: "hi".into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: CommandId("c_1".into()),
            },
            changed_by: None,
        }],
    })
}

fn turn_completed(outcome: TurnOutcome, error: Option<Failure>) -> Event {
    Event::TurnCompleted(TurnCompleted {
        outcome,
        error,
        questions: None,
    })
}

#[test]
fn fiber_started_is_the_first_line_and_names_the_version() {
    let session = Session::new();

    fiber_started(&session.log, "1.2.3").unwrap();

    let lines = log::read(&session.dir).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].kind, "fiber_started");
    assert_eq!(lines[0].payload["version"], "1.2.3");
    assert_eq!(lines[0].payload["resumed"], false);
}

#[test]
fn fiber_exited_carries_the_final_message_and_the_usage() {
    let session = Session::new();
    session.append(&turn_started(), None);
    session.append(&Event::StepStarted(Empty {}), None);
    session.append(&usage(3, Some(0.5), None), Some("a_1"));
    session.append(&message("Let me check."), Some("a_1"));
    session.append(&usage(5, Some(0.25), None), Some("a_2"));
    session.append(&usage(7, None, None), Some("a_2"));
    session.append(&usage(1, Some(2.0), Some(true)), Some("a_2"));
    session.append(&message("Hello."), Some("a_2"));
    session.append(&turn_completed(TurnOutcome::Completed, None), None);

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["text"], "Hello.");
    assert_eq!(exited["final_action_id"], "a_2");
    assert_eq!(exited.get("error"), None);
    let usage = &exited["usage"];
    assert_eq!(usage["tokens"]["output"], 16);
    assert_eq!(usage["tokens"]["input"], 40);
    assert_eq!(usage["tokens"]["cache_read"], 16);
    assert_eq!(usage["tokens"]["cache_write"]["1h"], 8);
    assert_eq!(usage["cost"], 0.75);
    assert_eq!(usage["subscription_cost"], 2.0);
}

#[test]
fn a_failed_turn_exits_1_with_its_error_and_no_final_message() {
    let session = Session::new();
    let cause = failure(ErrorCode::ProviderUnavailable, "down");
    session.append(&turn_started(), None);
    session.append(&message("Earlier."), Some("a_1"));
    session.append(
        &turn_completed(TurnOutcome::Failed, Some(cause.clone())),
        None,
    );

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 1);
    assert_eq!(exited["exit_code"], 1);
    assert_eq!(exited["error"], serde_json::to_value(&cause).unwrap());
    assert_eq!(exited.get("text"), None);
    // No billed call: the cost is 0, not null.
    assert_eq!(exited["usage"]["cost"], 0.0);
}

#[test]
fn a_failed_run_exits_1_with_its_error() {
    let session = Session::new();
    session.append(&turn_started(), None);
    let broke = failure(ErrorCode::IoFailed, "disk full");

    let (code, exited) = session.exit(Err(broke.clone()));

    assert_eq!(code, 1);
    assert_eq!(exited["error"], serde_json::to_value(&broke).unwrap());
}

#[test]
fn a_new_turn_forgets_the_last_ones_error_and_message() {
    let session = Session::new();
    let cause = failure(ErrorCode::ProviderUnavailable, "down");
    session.append(&turn_started(), None);
    session.append(&message("Earlier."), Some("a_1"));
    session.append(&turn_completed(TurnOutcome::Failed, Some(cause)), None);
    session.append(&turn_started(), None);

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited.get("error"), None);
    assert_eq!(exited.get("text"), None);
}

#[test]
fn a_billed_call_with_no_known_cost_makes_the_cost_null() {
    let session = Session::new();
    session.append(&turn_started(), None);
    session.append(&usage(1, None, None), None);

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited["usage"]["cost"], Value::Null);
}
