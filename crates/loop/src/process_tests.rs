//! `fiber_exited` from the loop's `Summary` (`docs/events.md`, "Process
//! boundary"): the exit reports what the process folded as it wrote, never
//! the log back. An extension's open asks are declined (`docs/events.md`,
//! `interaction_resolved`): a question a callback raised belongs to this
//! process's VM, so resuming never raises it again. Approvals and questions
//! without an extension stay pending instead.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::collections::BTreeMap;
use std::path::PathBuf;

use contract::events::{
    Answer, AskStep, AssistantMessageCompleted, DecidedBy, Decision, Empty, Event, InputItem,
    Interaction, InteractionRequested, InteractionResolved, MessageOutcome, PermissionRequested,
    PermissionResolved, ResolvedBy, RuleScope, StandingRule, TextCompleted, TurnCompleted,
    TurnOutcome, TurnStarted, UsageRecorded,
};
use contract::shapes::{
    ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender, Tokens, True,
};
use contract::{ActionId, CommandId, ErrorCode, GenerationId, RequestId, SessionId, TurnId};
use serde_json::Value;

use super::{Summary, fiber_exited, fiber_started};

/// A session log in a temporary directory, removed on drop. The summary is
/// folded beside the log: every appended line is observed, resetting at
/// each `fiber_started`, as the loop folds its own writes.
struct Session {
    _root: fakes::TempDir,
    dir: PathBuf,
    log: log::Log,
    summary: Summary,
}

impl Session {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-process");
        let log = log::Log::create(
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
            summary: Summary::default(),
        }
    }

    fn append(&mut self, event: &Event, action: Option<&str>) {
        let action = action.map(|action| ActionId(action.into()));
        self.log
            .append(event, Some(TurnId("t_1".into())), action.clone())
            .unwrap();
        self.summary.observe(event, action.as_ref());
    }

    /// A process starts: the summary restarts, as each process reports its
    /// own lines only (`docs/events.md`, `fiber_exited`).
    fn started(&mut self, resumed: bool) {
        fiber_started(&self.log, "1.2.3", resumed).unwrap();
        self.summary = Summary::default();
    }

    /// Writes `fiber_exited` for a one-turn run (`fiber ask`) and returns
    /// its exit code and payload.
    fn exit(&self, ran: Result<(), Failure>) -> (i32, Value) {
        self.exit_on(ran, None)
    }

    /// As [`Self::exit`], for a session the hub started.
    fn exit_served(&self, ran: Result<(), Failure>) -> (i32, Value) {
        self.exit_as(ran, false, None)
    }

    /// As [`Self::exit`], under a shutdown whose exit code is `signal`.
    fn exit_on(&self, ran: Result<(), Failure>, signal: Option<i32>) -> (i32, Value) {
        self.exit_as(ran, true, signal)
    }

    /// Writes `fiber_exited` for a run that is `one_turn` or served, under
    /// `signal`, and returns its exit code and payload.
    fn exit_as(
        &self,
        ran: Result<(), Failure>,
        one_turn: bool,
        signal: Option<i32>,
    ) -> (i32, Value) {
        let exited = fiber_exited(&self.log, &self.summary, ran, one_turn, signal).unwrap();
        let lines = log::read(&self.dir).unwrap();
        let last = lines.last().unwrap();
        assert_eq!(last.kind, "fiber_exited");
        assert!(last.turn_id.is_none());
        // The returned error is the written one, byte for byte.
        let written = last.payload.get("error").cloned().unwrap_or(Value::Null);
        let returned = match &exited.error {
            Some(error) => serde_json::to_value(error).unwrap(),
            None => Value::Null,
        };
        assert_eq!(returned, written);
        (exited.code, Value::Object(last.payload.clone()))
    }

    fn kinds(&self) -> Vec<String> {
        log::read(&self.dir)
            .unwrap()
            .into_iter()
            .map(|line| line.kind)
            .collect()
    }

    fn resolved(&self) -> Vec<contract::Envelope> {
        log::read(&self.dir)
            .unwrap()
            .into_iter()
            .filter(|line| line.kind == "interaction_resolved")
            .collect()
    }
}

fn permission(id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        always_reviewed: false,
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

fn asked(id: &str) -> Event {
    Event::InteractionRequested(InteractionRequested {
        request_id: RequestId(id.into()),
        interaction: Interaction::Confirm {
            prompt: "go?".into(),
        },
        action_ids: None,
        extension: Some("fiber.test/notes".into()),
        resumes: false,
    })
}

fn answered_by_person(id: &str) -> Event {
    Event::InteractionResolved(InteractionResolved {
        request_id: RequestId(id.into()),
        by: ResolvedBy::Person,
        answer: Answer::Confirmed { confirmed: true },
    })
}

#[test]
fn an_open_extension_ask_is_declined_at_exit() {
    let mut session = Session::new();
    session.started(false);
    session.append(&asked("r_1"), None);
    let (code, exited) = session.exit_served(Ok(()));
    assert_eq!(code, 0);
    let resolved = session.resolved();
    assert_eq!(resolved.len(), 1, "exactly one resolution is written");
    assert_eq!(resolved[0].payload["request_id"], "r_1");
    assert_eq!(resolved[0].payload["by"], "fiber");
    assert_eq!(resolved[0].payload["declined"], true);
    assert!(
        exited.get("suspended_on").is_none(),
        "no status waits on a question no callback holds"
    );
    assert_eq!(
        session.kinds(),
        [
            "fiber_started",
            "interaction_requested",
            "interaction_resolved",
            "fiber_exited",
        ]
    );
}

#[test]
fn a_resolved_extension_ask_is_left_alone() {
    let mut session = Session::new();
    session.started(false);
    session.append(&asked("r_1"), None);
    session.append(&answered_by_person("r_1"), None);
    let (code, exited) = session.exit_served(Ok(()));
    assert_eq!(code, 0);
    let resolved = session.resolved();
    assert_eq!(resolved.len(), 1, "no second resolution is written");
    assert_eq!(resolved[0].payload["by"], "person");
    assert!(exited.get("suspended_on").is_none());
}

#[test]
fn a_resolved_ask_from_an_earlier_process_is_not_declined_again() {
    let mut session = Session::new();
    session.started(false);
    session.append(&asked("r_old"), None);
    session.append(&answered_by_person("r_old"), None);
    session.started(true);
    let (code, _) = session.exit_served(Ok(()));
    assert_eq!(code, 0);
    assert_eq!(session.resolved().len(), 1, "the earlier resolution stands");
}

#[test]
fn two_open_extension_asks_are_declined_in_log_order() {
    let mut session = Session::new();
    session.started(false);
    session.append(&asked("r_1"), None);
    session.append(&asked("r_2"), None);
    let (code, exited) = session.exit_served(Ok(()));
    assert_eq!(code, 0);
    let resolved = session.resolved();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].payload["request_id"], "r_1");
    assert_eq!(resolved[1].payload["request_id"], "r_2");
    assert!(exited.get("suspended_on").is_none());
    assert_eq!(
        session.kinds(),
        [
            "fiber_started",
            "interaction_requested",
            "interaction_requested",
            "interaction_resolved",
            "interaction_resolved",
            "fiber_exited",
        ]
    );
}

#[test]
fn an_open_extension_ask_beside_an_open_approval_suspends_on_the_approval() {
    let mut session = Session::new();
    session.started(false);
    session.append(&asked("r_ask"), None);
    session.append(&permission("r_approval"), None);
    let (code, exited) = session.exit_served(Ok(()));
    assert_eq!(code, 0);
    assert_eq!(session.resolved().len(), 1, "the ask is declined");
    assert_eq!(exited["suspended_on"], "r_approval");
}

fn offered(id: &str) -> Event {
    Event::RepositoryCodeOffered(contract::events::RepositoryCodeOffered {
        request_id: RequestId(id.into()),
        items: Vec::new(),
    })
}

fn offer_resolved(id: &str) -> Event {
    Event::RepositoryCodeResolved(contract::events::RepositoryCodeResolved {
        request_id: RequestId(id.into()),
        decisions: Vec::new(),
    })
}

fn preamble() -> Event {
    Event::PreambleBuilt(
        serde_json::from_value(serde_json::json!({
            "reason": "start", "model": "fake/m", "context_window": 1000,
            "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": []
        }))
        .unwrap(),
    )
}

#[test]
fn a_process_that_exits_on_its_offer_names_it() {
    let mut session = Session::new();
    session.started(false);
    session.append(&offered("r_o"), None);
    let (_, exited) = session.exit_served(Ok(()));
    assert_eq!(exited["suspended_on"], "r_o");
}

#[test]
fn a_resolved_offer_is_not_suspended_on() {
    let mut session = Session::new();
    session.started(false);
    session.append(&offered("r_o"), None);
    session.append(&offer_resolved("r_o"), None);
    let (_, exited) = session.exit_served(Ok(()));
    assert!(exited.get("suspended_on").is_none());
}

#[test]
fn an_offer_left_behind_at_the_preamble_is_not_suspended_on() {
    let mut session = Session::new();
    session.started(false);
    session.append(&offered("r_o"), None);
    session.append(&preamble(), None);
    let (_, exited) = session.exit_served(Ok(()));
    assert!(exited.get("suspended_on").is_none());
}

#[test]
fn an_approval_beside_an_offer_is_suspended_on() {
    let mut session = Session::new();
    session.started(false);
    session.append(&offered("r_o"), None);
    session.append(&permission("r_a"), None);
    let (_, exited) = session.exit_served(Ok(()));
    assert_eq!(exited["suspended_on"], "r_a");
}

fn rewound() -> Event {
    Event::Rewound(contract::events::Rewound {
        new_session_id: SessionId("s_9e2b0000000000b2".into()),
        seq: contract::Seq(1),
        from_session_id: None,
        summary: None,
    })
}

#[test]
fn a_log_ending_in_rewound_writes_no_fiber_exited() {
    let mut session = Session::new();
    session.started(false);
    session.append(&asked("r_1"), None);
    session.append(&rewound(), None);
    let before = session.kinds();
    let exited = fiber_exited(&session.log, &session.summary, Ok(()), false, None).unwrap();
    assert_eq!(exited.code, 0);
    assert!(exited.error.is_none());
    // Nothing is written: no `fiber_exited`, and the open extension ask is
    // left as it is, not declined.
    assert_eq!(session.kinds(), before);
    assert_eq!(session.kinds().last().map(String::as_str), Some("rewound"));
    assert!(session.resolved().is_empty());
}

#[test]
fn a_log_ending_in_rewound_reports_the_loops_error_without_writing() {
    let mut session = Session::new();
    session.started(false);
    session.append(&rewound(), None);
    let failure = contract::shapes::Failure {
        code: contract::ErrorCode::IoFailed,
        message: "the disk is gone".into(),
        retry_after_ms: None,
        provider: None,
    };
    let exited = fiber_exited(
        &session.log,
        &session.summary,
        Err(failure.clone()),
        false,
        None,
    )
    .unwrap();
    assert_eq!(exited.code, 1);
    assert_eq!(exited.error, Some(failure));
    assert_eq!(session.kinds().last().map(String::as_str), Some("rewound"));
}

fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.into(),
        retry_after_ms: None,
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
        reviewer: None,
        input_bytes: 1,
        input_media: None,
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
fn fiber_exited_returns_the_error_it_wrote() {
    let mut failed = Session::new();
    let cause = failure(ErrorCode::ProviderUnavailable, "down");
    failed.append(&turn_started(), None);
    failed.append(
        &turn_completed(TurnOutcome::Failed, Some(cause.clone())),
        None,
    );
    let written = fiber_exited(&failed.log, &failed.summary, Ok(()), true, None).unwrap();
    assert_eq!(written.code, 1);
    assert_eq!(written.error, Some(cause));

    let mut ok = Session::new();
    ok.append(&turn_started(), None);
    ok.append(&turn_completed(TurnOutcome::Completed, None), None);
    let written = fiber_exited(&ok.log, &ok.summary, Ok(()), true, None).unwrap();
    assert_eq!(written.code, 0);
    assert_eq!(written.error, None);

    let mut signalled = Session::new();
    signalled.append(&turn_started(), None);
    signalled.append(
        &turn_completed(
            TurnOutcome::Failed,
            Some(failure(ErrorCode::ProviderUnavailable, "down")),
        ),
        None,
    );
    let written =
        fiber_exited(&signalled.log, &signalled.summary, Ok(()), true, Some(143)).unwrap();
    assert_eq!(written.code, 143);
    assert_eq!(written.error, None);
}

#[test]
fn fiber_exited_carries_the_final_message_and_the_usage() {
    let mut session = Session::new();
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
    let mut session = Session::new();
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
fn a_served_session_whose_last_turn_failed_exits_0_with_no_error() {
    let mut session = Session::new();
    let cause = failure(ErrorCode::ProviderUnavailable, "down");
    session.append(&turn_started(), None);
    session.append(&text_part("Earlier."), Some("a_1"));
    session.append(&message(), Some("a_1"));
    session.append(&turn_completed(TurnOutcome::Failed, Some(cause)), None);

    let (code, exited) = session.exit_served(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited.get("error"), None);
    // The failed turn still leaves no final message.
    assert_eq!(exited.get("text"), None);
    assert_eq!(exited.get("final_action_id"), None);
}

#[test]
fn a_served_session_whose_run_failed_exits_1_with_the_runs_error() {
    let mut session = Session::new();
    session.append(&turn_started(), None);
    session.append(
        &turn_completed(
            TurnOutcome::Failed,
            Some(failure(ErrorCode::ProviderUnavailable, "down")),
        ),
        None,
    );
    let broke = failure(ErrorCode::IoFailed, "disk full");

    let (code, exited) = session.exit_served(Err(broke.clone()));

    assert_eq!(code, 1);
    assert_eq!(exited["exit_code"], 1);
    assert_eq!(exited["error"], serde_json::to_value(&broke).unwrap());
}

#[test]
fn a_served_session_whose_earlier_turn_failed_exits_0_with_the_last_message() {
    let mut session = Session::new();
    session.append(&turn_started(), None);
    session.append(
        &turn_completed(
            TurnOutcome::Failed,
            Some(failure(ErrorCode::ProviderUnavailable, "down")),
        ),
        None,
    );
    session.append(&turn_started(), None);
    session.append(&text_part("Hello."), Some("a_2"));
    session.append(&message(), Some("a_2"));
    session.append(&turn_completed(TurnOutcome::Completed, None), None);

    let (code, exited) = session.exit_served(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited.get("error"), None);
    assert_eq!(exited["text"], "Hello.");
    assert_eq!(exited["final_action_id"], "a_2");
}

#[test]
fn a_failed_run_exits_1_with_its_error() {
    let mut session = Session::new();
    session.append(&turn_started(), None);
    let broke = failure(ErrorCode::IoFailed, "disk full");

    let (code, exited) = session.exit(Err(broke.clone()));

    assert_eq!(code, 1);
    assert_eq!(exited["error"], serde_json::to_value(&broke).unwrap());
}

#[test]
fn a_new_turn_forgets_the_last_ones_error_and_message() {
    let mut session = Session::new();
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
    let mut session = Session::new();
    session.append(&turn_started(), None);
    session.append(&usage("g1", 1, None, None), None);

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited["usage"]["cost"], Value::Null);
}

#[test]
fn a_later_line_with_the_same_generation_replaces_the_earlier() {
    let mut session = Session::new();
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
fn fiber_exited_after_a_resume_reports_only_the_resumed_process_lines() {
    let mut session = Session::new();
    session.started(false);
    session.append(&turn_started(), None);
    session.append(&usage("g1", 3, Some(0.5), None), Some("a_1"));
    session.append(&text_part("First."), Some("a_1"));
    session.append(&message(), Some("a_1"));
    session.append(&turn_completed(TurnOutcome::Completed, None), None);
    session.started(true);
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

fn resolved(id: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: None,
        request_id: Some(RequestId(id.into())),
        decision: Decision::Deny,
        decided_by: DecidedBy::Cancel,
        reason: None,
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
        hosts: None,
    })
}

#[test]
fn fiber_exited_names_the_unresolved_request() {
    let mut session = Session::new();
    session.started(false);
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
            resumes: false,
        }),
        None,
    );

    let (code, exited) = session.exit(Ok(()));

    assert_eq!(code, 0);
    assert_eq!(exited["suspended_on"], "r_question");
    assert_eq!(
        session.kinds(),
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
    let mut session = Session::new();
    session.started(false);
    session.append(&permission("r_ask"), Some("a_1"));
    session.append(&resolved("r_ask"), Some("a_1"));

    let (_, exited) = session.exit(Ok(()));

    assert_eq!(exited.get("suspended_on"), None);
    assert_eq!(
        session.kinds(),
        [
            "fiber_started",
            "permission_requested",
            "permission_resolved",
            "fiber_exited",
        ]
    );
}

#[test]
fn fiber_exited_keeps_the_request_that_was_not_resolved() {
    let mut earlier = Session::new();
    earlier.started(false);
    earlier.append(&permission("r_early"), Some("a_1"));
    earlier.append(&permission("r_late"), Some("a_2"));
    earlier.append(&resolved("r_early"), Some("a_1"));
    let (_, exited) = earlier.exit(Ok(()));
    assert_eq!(exited["suspended_on"], "r_late");
    assert_eq!(
        earlier.kinds(),
        [
            "fiber_started",
            "permission_requested",
            "permission_requested",
            "permission_resolved",
            "fiber_exited",
        ]
    );

    let mut later = Session::new();
    later.started(false);
    later.append(&permission("r_early"), Some("a_1"));
    later.append(&permission("r_late"), Some("a_2"));
    later.append(&resolved("r_late"), Some("a_2"));
    let (_, exited) = later.exit(Ok(()));
    assert_eq!(exited["suspended_on"], "r_early");
    assert_eq!(
        later.kinds(),
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
        resumes: false,
    })
}

fn answered(id: &str) -> Event {
    Event::InteractionResolved(contract::events::InteractionResolved {
        request_id: RequestId(id.into()),
        by: contract::events::ResolvedBy::Fiber,
        answer: contract::events::Answer::Declined { declined: True },
    })
}

#[test]
fn fiber_exited_keeps_the_question_that_was_not_resolved() {
    for (resolve, left) in [("q_early", "q_late"), ("q_late", "q_early")] {
        let mut session = Session::new();
        session.started(false);
        session.append(&interaction("q_early"), None);
        session.append(&interaction("q_late"), None);
        session.append(&answered(resolve), None);
        let (_, exited) = session.exit(Ok(()));
        assert_eq!(exited["suspended_on"], left);
        assert_eq!(
            session.kinds(),
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

#[test]
fn a_signal_exits_with_its_code_and_no_final_message() {
    let mut session = Session::new();
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
    let mut session = Session::new();
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
    let mut session = Session::new();
    session.started(false);
    session.append(&permission("r_1"), Some("a_1"));

    let (code, exited) = session.exit_on(Ok(()), Some(129));

    assert_eq!(code, 129);
    assert_eq!(exited["suspended_on"], "r_1");
}

#[test]
fn a_signal_does_not_name_a_request_resolved_since() {
    let mut session = Session::new();
    session.started(false);
    session.append(&permission("r_old"), Some("a_1"));
    session.started(true);
    session.append(&permission("r_old"), Some("a_1"));
    session.append(&resolved("r_old"), Some("a_1"));

    let (_, exited) = session.exit_on(Ok(()), Some(143));

    assert_eq!(exited.get("suspended_on"), None);
}

#[test]
fn a_signal_reports_only_this_process_usage() {
    let mut session = Session::new();
    session.started(false);
    session.append(&usage("g1", 3, Some(0.5), None), Some("a_1"));
    session.started(true);
    session.append(&usage("g2", 5, Some(0.25), None), Some("a_2"));

    let (_, exited) = session.exit_on(Ok(()), Some(143));

    assert_eq!(exited["usage"]["tokens"]["output"], 5);
}
