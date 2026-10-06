//! The process boundary the loop writes (`docs/events.md`, "Process
//! boundary"): `fiber_started`, and `fiber_exited` folded from the log.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use contract::events::{
    AskStep, AssistantMessageCompleted, DecidedBy, Decision, Empty, Event, InputItem, Interaction,
    InteractionRequested, MessageOutcome, PermissionRequested, PermissionResolved, RuleScope,
    StandingRule, TextCompleted, TurnCompleted, TurnOutcome, TurnStarted, UsageRecorded,
};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender, Tokens};
use contract::{ActionId, CommandId, ErrorCode, GenerationId, RequestId, SessionId, TurnId};
use log::Log;
use r#loop::{fiber_exited, fiber_started};
use serde_json::Value;

/// A session log in a temporary directory, removed on drop.
struct Session {
    _root: fakes::TempDir,
    dir: PathBuf,
    log: Log,
}

impl Session {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-process");
        let log = Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        let dir = root.path().join("s_1");
        Self {
            dir,
            _root: root,
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
        self.exit_on(ran, None)
    }

    /// As [`Self::exit`], under a shutdown whose exit code is `signal`.
    fn exit_on(&self, ran: Result<(), Failure>, signal: Option<i32>) -> (i32, Value) {
        let code = fiber_exited(&self.log, &self.dir, ran, signal).unwrap();
        let lines = log::read(&self.dir).unwrap();
        let last = lines.last().unwrap();
        assert_eq!(last.kind, "fiber_exited");
        assert!(last.turn_id.is_none());
        (code, Value::Object(last.payload.clone()))
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

fn usage(id: &str, output: u64, cost: Option<f64>, subscription: Option<bool>) -> Event {
    Event::UsageRecorded(UsageRecorded {
        generation_id: GenerationId(id.into()),
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

fn text_part(text: &str) -> Event {
    Event::TextCompleted(TextCompleted {
        text: text.into(),
        provider_item: None,
    })
}

fn message() -> Event {
    Event::AssistantMessageCompleted(AssistantMessageCompleted {
        outcome: MessageOutcome::Completed,
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
                command_id: Some(CommandId("c_1".into())),
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

    fiber_started(&session.log, "1.2.3", false).unwrap();

    let lines = log::read(&session.dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["fiber_started"]
    );
    assert_eq!(lines[0].payload["version"], "1.2.3");
    assert_eq!(lines[0].payload["resumed"], false);
}

#[test]
fn fiber_exited_carries_the_final_message_and_the_usage() {
    let session = Session::new();
    session.append(&turn_started(), None);
    session.append(&Event::StepStarted(Empty {}), None);
    session.append(&usage("g1", 3, Some(0.5), None), Some("a_1"));
    session.append(&text_part("Let me check."), Some("a_1"));
    session.append(&message(), Some("a_1"));
    session.append(&usage("g2", 5, Some(0.25), None), Some("a_2"));
    session.append(&usage("g3", 7, None, None), Some("a_2"));
    session.append(&usage("g4", 1, Some(2.0), Some(true)), Some("a_2"));
    session.append(&text_part("Hello."), Some("a_2"));
    session.append(&message(), Some("a_2"));
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
    session.append(&text_part("Earlier."), Some("a_1"));
    session.append(&message(), Some("a_1"));
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
    session.append(&text_part("Earlier."), Some("a_1"));
    session.append(&message(), Some("a_1"));
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
    session.append(&usage("g1", 1, None, None), None);

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited["usage"]["cost"], Value::Null);
}

#[test]
fn a_later_line_with_the_same_generation_replaces_the_earlier() {
    let session = Session::new();
    session.append(&usage("g1", 3, Some(0.5), None), Some("a_1"));
    session.append(&usage("g1", 9, None, None), Some("a_1"));
    session.append(&usage("g2", 1, Some(2.0), Some(true)), Some("a_2"));
    session.append(&usage("g2", 4, Some(0.1), Some(true)), Some("a_2"));
    session.append(&usage("g3", 2, Some(0.4), None), Some("a_3"));
    session.append(&usage("g3", 6, Some(1.5), None), Some("a_3"));

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    let usage = &exited["usage"];
    // Three calls remain, each the later line of its generation.
    assert_eq!(usage["tokens"]["output"], 19);
    assert_eq!(usage["tokens"]["input"], 30);
    assert_eq!(usage["tokens"]["cache_read"], 12);
    assert_eq!(usage["tokens"]["cache_write"]["1h"], 6);
    // g1's 0.5 was replaced by null and does not remain in the sum.
    assert_eq!(usage["cost"], 1.5);
    assert_eq!(usage["subscription_cost"], 0.1);
}

#[test]
fn a_resumed_fiber_started_marks_the_session_resumed() {
    let session = Session::new();

    fiber_started(&session.log, "1.2.3", true).unwrap();

    let lines = log::read(&session.dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["fiber_started"]
    );
    assert_eq!(lines[0].payload["resumed"], true);
}

#[test]
fn fiber_exited_after_a_resume_reports_only_the_resumed_process_lines() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&turn_started(), None);
    session.append(&usage("g1", 3, Some(0.5), None), Some("a_1"));
    session.append(&text_part("First."), Some("a_1"));
    session.append(&message(), Some("a_1"));
    session.append(&turn_completed(TurnOutcome::Completed, None), None);
    fiber_started(&session.log, "1.2.3", true).unwrap();
    session.append(&turn_started(), None);
    session.append(&usage("g2", 5, Some(0.25), None), Some("a_2"));
    session.append(&text_part("Second."), Some("a_2"));
    session.append(&message(), Some("a_2"));
    session.append(&turn_completed(TurnOutcome::Completed, None), None);

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    let lines = log::read(&session.dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        [
            "fiber_started",
            "turn_started",
            "usage_recorded",
            "text_completed",
            "assistant_message_completed",
            "turn_completed",
            "fiber_started",
            "turn_started",
            "usage_recorded",
            "text_completed",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(exited["text"], "Second.");
    assert_eq!(exited["final_action_id"], "a_2");
    // Only the resumed process's usage: g2, not g1.
    assert_eq!(exited["usage"]["tokens"]["output"], 5);
    assert_eq!(exited["usage"]["cost"], 0.25);
}

fn permission(id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        request_id: RequestId(id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
        },
        step: AskStep::StandingAsk {
            standing_rule: StandingRule {
                scope: RuleScope::Project,
                prefix: "npm".into(),
            },
        },
    })
}

fn resolved(id: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        request_id: Some(RequestId(id.into())),
        decision: Decision::Deny,
        decided_by: DecidedBy::Cancel,
        reason: None,
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
    })
}

#[test]
fn fiber_exited_names_the_unresolved_request() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&permission("r_old"), Some("a_1"));
    session.append(&resolved("r_old"), Some("a_1"));
    session.append(&permission("r_ask"), Some("a_2"));
    session.append(
        &Event::InteractionRequested(InteractionRequested {
            request_id: RequestId("r_question".into()),
            interaction: Interaction::Confirm {
                prompt: "go?".into(),
            },
            action_ids: None,
            extension: None,
        }),
        None,
    );

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited["suspended_on"], "r_question");
    assert_eq!(
        kinds(&session),
        [
            "fiber_started",
            "permission_requested",
            "permission_resolved",
            "permission_requested",
            "interaction_requested",
            "fiber_exited",
        ]
    );
}

#[test]
fn fiber_exited_has_no_suspended_on_once_the_request_is_resolved() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&permission("r_ask"), Some("a_1"));
    session.append(&resolved("r_ask"), Some("a_1"));

    let (_, exited) = session.exit(Ok(()));

    assert_eq!(exited.get("suspended_on"), None);
    assert_eq!(
        kinds(&session),
        [
            "fiber_started",
            "permission_requested",
            "permission_resolved",
            "fiber_exited",
        ]
    );
}

#[test]
fn fiber_exited_ignores_a_request_from_the_previous_process() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&permission("r_old"), Some("a_1"));
    fiber_started(&session.log, "1.2.3", true).unwrap();

    let (_, exited) = session.exit(Ok(()));

    assert_eq!(exited.get("suspended_on"), None);
    assert_eq!(
        kinds(&session),
        [
            "fiber_started",
            "permission_requested",
            "fiber_started",
            "fiber_exited",
        ]
    );
}

#[test]
fn fiber_exited_keeps_the_request_that_was_not_resolved() {
    let earlier = Session::new();
    fiber_started(&earlier.log, "1.2.3", false).unwrap();
    earlier.append(&permission("r_early"), Some("a_1"));
    earlier.append(&permission("r_late"), Some("a_2"));
    earlier.append(&resolved("r_early"), Some("a_1"));
    let (_, exited) = earlier.exit(Ok(()));
    assert_eq!(exited["suspended_on"], "r_late");
    assert_eq!(
        kinds(&earlier),
        [
            "fiber_started",
            "permission_requested",
            "permission_requested",
            "permission_resolved",
            "fiber_exited",
        ]
    );

    let later = Session::new();
    fiber_started(&later.log, "1.2.3", false).unwrap();
    later.append(&permission("r_early"), Some("a_1"));
    later.append(&permission("r_late"), Some("a_2"));
    later.append(&resolved("r_late"), Some("a_2"));
    let (_, exited) = later.exit(Ok(()));
    assert_eq!(exited["suspended_on"], "r_early");
    assert_eq!(
        kinds(&later),
        [
            "fiber_started",
            "permission_requested",
            "permission_requested",
            "permission_resolved",
            "fiber_exited",
        ]
    );
}

fn interaction(id: &str) -> Event {
    Event::InteractionRequested(InteractionRequested {
        request_id: RequestId(id.into()),
        interaction: Interaction::Confirm {
            prompt: "go?".into(),
        },
        action_ids: None,
        extension: None,
    })
}

fn answered(id: &str) -> Event {
    Event::InteractionResolved(contract::events::InteractionResolved {
        request_id: RequestId(id.into()),
        by: contract::events::ResolvedBy::Fiber,
        answer: contract::events::Answer::Declined {
            declined: contract::shapes::True,
        },
    })
}

#[test]
fn fiber_exited_keeps_the_question_that_was_not_resolved() {
    for (resolve, left) in [("q_early", "q_late"), ("q_late", "q_early")] {
        let session = Session::new();
        fiber_started(&session.log, "1.2.3", false).unwrap();
        session.append(&interaction("q_early"), None);
        session.append(&interaction("q_late"), None);
        session.append(&answered(resolve), None);
        let (_, exited) = session.exit(Ok(()));
        assert_eq!(exited["suspended_on"], left);
        assert_eq!(
            kinds(&session),
            [
                "fiber_started",
                "interaction_requested",
                "interaction_requested",
                "interaction_resolved",
                "fiber_exited",
            ]
        );
    }
}

fn kinds(session: &Session) -> Vec<String> {
    log::read(&session.dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect()
}

#[test]
fn a_signal_exits_with_its_code_and_no_final_message() {
    let session = Session::new();
    session.append(&turn_started(), None);
    session.append(&usage("g1", 3, Some(0.5), None), Some("a_1"));
    session.append(&text_part("Hello."), Some("a_1"));
    session.append(&message(), Some("a_1"));
    session.append(&turn_completed(TurnOutcome::Interrupted, None), None);

    let (code, exited) = session.exit_on(Ok(()), Some(130));

    assert_eq!(code, 130);
    assert_eq!(exited["exit_code"], 130);
    assert_eq!(exited.get("text"), None);
    assert_eq!(exited.get("final_action_id"), None);
    assert_eq!(exited.get("error"), None);
    assert_eq!(exited["usage"]["tokens"]["output"], 3);
}

#[test]
fn a_signal_drops_the_error_the_loop_returned() {
    let session = Session::new();
    session.append(&turn_started(), None);
    session.append(
        &turn_completed(
            TurnOutcome::Failed,
            Some(failure(ErrorCode::ProviderUnavailable, "the turn failed")),
        ),
        None,
    );

    let (code, exited) = session.exit_on(
        Err(failure(ErrorCode::IoFailed, "the loop failed")),
        Some(143),
    );

    assert_eq!(code, 143);
    assert_eq!(exited["exit_code"], 143);
    assert_eq!(exited.get("error"), None);
}

#[test]
fn a_signal_keeps_the_request_this_process_left_pending() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&permission("r_1"), Some("a_1"));

    let (code, exited) = session.exit_on(Ok(()), Some(129));

    assert_eq!(code, 129);
    assert_eq!(exited["suspended_on"], "r_1");
}

#[test]
fn a_signal_names_a_request_from_the_previous_process() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&permission("r_old"), Some("a_1"));
    fiber_started(&session.log, "1.2.3", true).unwrap();

    let (_, exited) = session.exit_on(Ok(()), Some(143));

    assert_eq!(exited["suspended_on"], "r_old");
}

#[test]
fn a_signal_does_not_name_a_request_resolved_since() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&permission("r_old"), Some("a_1"));
    fiber_started(&session.log, "1.2.3", true).unwrap();
    session.append(&permission("r_old"), Some("a_1"));
    session.append(&resolved("r_old"), Some("a_1"));

    let (_, exited) = session.exit_on(Ok(()), Some(143));

    assert_eq!(exited.get("suspended_on"), None);
}

#[test]
fn a_signal_reports_only_this_process_usage() {
    let session = Session::new();
    fiber_started(&session.log, "1.2.3", false).unwrap();
    session.append(&usage("g1", 3, Some(0.5), None), Some("a_1"));
    fiber_started(&session.log, "1.2.3", true).unwrap();
    session.append(&usage("g2", 5, Some(0.25), None), Some("a_2"));

    let (_, exited) = session.exit_on(Ok(()), Some(143));

    assert_eq!(exited["usage"]["tokens"]["output"], 5);
}
