//! The `model` driver command (`docs/invocation.md`, "Driver commands"):
//! switching model or thinking level at the next turn boundary, writing
//! `model_changed` then `preamble_built`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::clock::Clock as _;
use contract::events::{CacheLifetime, Notice, TurnOutcome};
use contract::inbox::{Ack, Answer, Delivery, Rejection};
use contract::provider::Provider;
use contract::{Envelope, ErrorCode, ThinkingLevel};
use fakes::{Scripted, ScriptedProvider};
use r#loop::{HandoffSettings, Model, NO_SWITCH, Prepare, Prepared, Reviewer, Switchable};

use support::{DEADLINE, MODEL, Session, delivery, kinds, model};

const NEW_MODEL: &str = "fake/model-2";

fn new_provider(script: Vec<Scripted>) -> Arc<ScriptedProvider> {
    Arc::new(ScriptedProvider::new(script))
}

fn model_of(reference: &str) -> Model {
    Model {
        reference: reference.into(),
        cost: None,
        subscription: false,
    }
}

fn no_reviewer() -> Result<Reviewer, contract::shapes::Failure> {
    Err(contract::shapes::Failure {
        code: ErrorCode::NoModel,
        message: r#loop::NO_MODEL_MESSAGE.into(),
        retry_after_ms: None,
        provider: None,
    })
}

/// A `Prepare` that switches to `reference` on `provider`, keeping the
/// session's choice unless `thinking` names one. Records every `chosen`
/// it was given, in order.
fn prepare_to(
    provider: Arc<ScriptedProvider>,
    reference: &str,
    recorded: Arc<Mutex<Vec<Option<ThinkingLevel>>>>,
) -> Prepare {
    let reference = reference.to_owned();
    Arc::new(
        move |args: &contract::commands::ModelArgs, chosen: Option<ThinkingLevel>| {
            recorded.lock().unwrap().push(chosen);
            let thinking = match &args.thinking {
                Some(level) => Some(level.parse::<ThinkingLevel>().map_err(|_| Rejection {
                    code: ErrorCode::InvalidArguments,
                    message: format!("unknown thinking level `{level}`"),
                })?),
                None => chosen,
            };
            let kept = thinking.or(chosen);
            Ok(Prepared {
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: model_of(&reference),
                thinking: kept,
                chosen: kept,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: None,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: None,
                notice: None,
            })
        },
    )
}

fn switchable() -> Switchable {
    Switchable {
        chosen: None,
        web_search: None,
    }
}

fn with_switch(session: &mut Session, prepare: Prepare, switchable: Switchable) {
    let looped = session.looped.take().unwrap().switcher(prepare, switchable);
    session.looped = Some(looped);
}

fn run(session: &mut Session, prompt: &str) -> (Option<TurnOutcome>, Vec<Envelope>) {
    session.inbox.send(delivery(prompt)).unwrap();
    let outcome = session.turn();
    (outcome, session.lines())
}

fn of_kind<'a>(lines: &'a [Envelope], kind: &str) -> Vec<&'a Envelope> {
    lines.iter().filter(|line| line.kind == kind).collect()
}

fn assert_kinds(lines: &[Envelope], parts: &[&[&str]]) {
    assert_eq!(kinds(lines), parts.concat());
}

const OPENING: &[&str] = &[
    "session_started",
    "preamble_built",
    "opening_message",
    "turn_started",
];
const STEP: &[&str] = &["step_started"];
const REPLY: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
];
const REASONING_REPLY: &[&str] = &[
    "assistant_message_started",
    "reasoning_started",
    "reasoning_delta",
    "assistant_message_delta",
    "reasoning_completed",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
];
const ENDED: &[&str] = &["turn_completed"];
const SWITCHED_OPENING: &[&str] = &["model_changed", "preamble_built", "turn_started"];

#[test]
fn between_turns_switches_before_the_next_turn_started() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );

    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);

    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);

    let changed = of_kind(&lines, "model_changed");
    assert_eq!(changed.len(), 1);
    let changed = &changed[0];
    assert_eq!(changed.payload["before"]["model"], MODEL);
    assert_eq!(changed.payload["after"]["model"], NEW_MODEL);
    assert_eq!(changed.payload["source"], "driver");
    assert!(changed.turn_id.is_none());

    let built = of_kind(&lines, "preamble_built");
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].payload["reason"], "switch");
    assert_eq!(built[0].payload["model"], NEW_MODEL);

    assert_eq!(
        session.requests().len(),
        1,
        "the old provider keeps one call"
    );
    assert_eq!(
        next.requests().len(),
        1,
        "the next turn goes to the new provider"
    );
    assert_eq!(*recorded.lock().unwrap(), vec![None]);
}

#[test]
fn model_then_prompt_in_one_batch_runs_the_prompt_on_the_new_model() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    // A switch admitted while idle applies at once, so the prompt that
    // follows in the same drain starts its turn on the new model.
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    session.inbox.send(delivery("hi")).unwrap();
    let outcome = session.turn();
    let lines = session.lines();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            &[
                "session_started",
                "model_changed",
                "preamble_built",
                "opening_message",
                "turn_started",
            ] as &[&str],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert_eq!(next.requests().len(), 1);
    assert!(session.requests().is_empty());
}

#[test]
fn during_a_turn_applies_after_turn_completed() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::with_tools_injecting(
        vec![Scripted::text("Old."), Scripted::text("New.")],
        vec![model(NEW_MODEL, None)],
        Vec::new(),
    );
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    // The first turn runs on the old provider; the switch it took waits
    // in `pending`.
    session.inbox.send(delivery("hi")).unwrap();
    let outcome = session.turn();
    let first = session.lines();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    assert_eq!(session.requests().len(), 1);
    assert!(next.requests().is_empty());

    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
    assert_eq!(next.requests().len(), 1);
}

#[test]
fn rejected_by_prepare_changes_nothing() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let mut session = Session::new(
        vec![Scripted::text("Old."), Scripted::text("Old again.")],
        None,
    );
    let prepare: Prepare = Arc::new(|_, _| {
        Err(Rejection {
            code: ErrorCode::InvalidArguments,
            message: "no such model".into(),
        })
    });
    with_switch(&mut session, prepare, switchable());

    let (tx, rx) = mpsc::channel();
    session
        .inbox
        .send(support::model_reported(NEW_MODEL, None, tx))
        .unwrap();
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    let answer = rx.recv_timeout(DEADLINE).expect("the model is answered");
    let rejection = answer.unwrap_err();
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);

    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[&["turn_started"] as &[&str], STEP, REPLY, ENDED]);
    assert!(of_kind(&lines, "model_changed").is_empty());
    assert_eq!(session.requests().len(), 2);
    assert!(next.requests().is_empty());
}

#[test]
fn no_switcher_is_invalid_arguments() {
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    let (tx, rx) = mpsc::channel();
    session
        .inbox
        .send(support::model_reported(NEW_MODEL, None, tx))
        .unwrap();
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[OPENING, STEP, REPLY, ENDED]);
    let answer = rx.recv_timeout(DEADLINE).expect("the model is answered");
    let rejection = answer.unwrap_err();
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(rejection.message, NO_SWITCH);
    assert!(of_kind(&lines, "model_changed").is_empty());
}

#[test]
fn after_close_prepare_is_not_called() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let called = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&called);
    let prepare: Prepare = Arc::new(move |_, _| {
        *flag.lock().unwrap() = true;
        Ok(Prepared {
            provider: Arc::clone(&next) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: None,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: no_reviewer(),
            web_search: None,
            notice: None,
        })
    });
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    with_switch(&mut session, prepare, switchable());
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);

    let (close_tx, close_rx) = mpsc::channel::<Answer>();
    let (model_tx, model_rx) = mpsc::channel::<Answer>();
    session
        .inbox
        .send(Delivery::Close(Ack(Box::new(move |answer| {
            let _sent = close_tx.send(answer);
        }))))
        .unwrap();
    session
        .inbox
        .send(Delivery::Model(
            contract::commands::ModelArgs {
                model: NEW_MODEL.into(),
                thinking: None,
            },
            Ack(Box::new(move |answer| {
                let _sent = model_tx.send(answer);
            })),
        ))
        .unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, None);
    assert!(
        close_rx
            .recv_timeout(DEADLINE)
            .expect("close answered")
            .is_ok()
    );
    let rejected = model_rx
        .recv_timeout(DEADLINE)
        .expect("model answered")
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::Closing);
    assert!(
        !*called.lock().unwrap(),
        "prepare runs only before `closing`"
    );
    let all = log::read(&session.dir).unwrap();
    // The log holds the turn's durable lines only: no deltas, and the
    // refused `model` and `close` write nothing.
    assert_kinds(
        &all,
        &[&[
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ] as &[&str]],
    );
}

use contract::RequestId;
use contract::events::Decision;
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::Effect;
use support::{TestTool, calls_reply, on_request, reasoning_reply};

fn executes_tool() -> Arc<TestTool> {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = Some("npm publish".into());
    Arc::new(tool)
}

fn ask_rule() -> StandingRules {
    StandingRules {
        global: vec![Rule {
            decision: RuleDecision::Ask,
            tool: "shell".into(),
            prefix: "npm publish".into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    }
}

fn allow_answer() -> contract::commands::ReplyAnswer {
    contract::commands::ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: None,
    }
}

#[test]
fn during_an_approval_wait_is_accepted_and_applied_after_the_turn() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let tool = executes_tool();
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", serde_json::json!({"city": "Paris"}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn contract::tool::Tool>],
    );
    session.rules.set(ask_rule());
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    let (ack_tx, ack_rx) = mpsc::channel::<Answer>();
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| {
            inbox
                .send(support::model_reported(NEW_MODEL, None, ack_tx.clone()))
                .unwrap();
            // The switch is accepted while the approval waits.
            inbox
                .send(Delivery::Reply(
                    contract::commands::Reply {
                        request_id: id,
                        answer: allow_answer(),
                    },
                    support::ignore(),
                ))
                .unwrap();
        }
    });
    session.inbox.send(delivery("go")).unwrap();
    let outcome = session.turn();
    answered.join().unwrap();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = session.lines();
    // Accepted during the wait, applied after the turn: no `model_changed` yet.
    assert_kinds(
        &lines,
        &[
            &[
                "session_started",
                "preamble_built",
                "opening_message",
                "turn_started",
            ] as &[&str],
            &["step_started"],
            &[
                "assistant_message_started",
                "assistant_message_delta",
                "tool_call_arguments_delta",
                "tool_call_requested",
                "usage_recorded",
                "assistant_message_completed",
                "permission_requested",
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed",
            ],
            &["step_started"],
            REPLY,
            ENDED,
        ],
    );
    assert!(of_kind(&lines, "model_changed").is_empty());
    let answer = ack_rx
        .recv_timeout(DEADLINE)
        .expect("the model is answered");
    assert!(answer.is_ok(), "accepted before `turn_completed`");

    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
    assert_eq!(next.requests().len(), 1);
}

#[test]
fn an_idle_deadline_in_an_approval_wait_writes_no_model_changed() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let tool = executes_tool();
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", serde_json::json!({"city": "Paris"}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn contract::tool::Tool>],
    );
    session.rules.set(ask_rule());
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.idle_exit(Some(Duration::from_secs(60))));
    let clock = Arc::clone(&session.clock);
    session.inbox.send(delivery("go")).unwrap();
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    let start = clock.now();
    // Wait for the approval wait to park on the idle deadline.
    assert!(
        clock.await_parked(start + Duration::from_secs(60), DEADLINE),
        "the approval wait parks on the idle deadline"
    );
    // A switch accepted during the wait.
    let (ack_tx, ack_rx) = mpsc::channel::<Answer>();
    session
        .inbox
        .send(support::model_reported(NEW_MODEL, None, ack_tx))
        .unwrap();
    // Let the loop take it, then end the wait at the deadline.
    let wake = || {
        session
            .inbox
            .send(Delivery::Reply(
                contract::commands::Reply {
                    request_id: RequestId("r_absent".into()),
                    answer: allow_answer(),
                },
                support::ignore(),
            ))
            .unwrap();
    };
    // The ack arrives before the deadline ends the wait.
    let answer = ack_rx
        .recv_timeout(DEADLINE)
        .expect("the model is answered");
    assert!(answer.is_ok());
    clock.advance(Duration::from_secs(60));
    wake();
    let (looped, outcome) = finished.recv_timeout(DEADLINE).expect("the turn ended");
    session.looped = Some(looped);
    assert_eq!(
        outcome, None,
        "the idle deadline ends the turn with no `turn_completed`"
    );
    assert!(next.requests().is_empty());
    let idle_wait = log::read(&session.dir).unwrap();
    // The idle deadline ends the wait with no `turn_completed`, and the
    // switch queued during it is dropped: no `model_changed`.
    assert_kinds(
        &idle_wait,
        &[&[
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
        ] as &[&str]],
    );
}

#[test]
fn a_shutdown_after_turn_completed_writes_model_changed_and_starts_no_turn() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::with_tools_injecting(
        vec![Scripted::text("Old.")],
        vec![model(NEW_MODEL, None)],
        Vec::new(),
    );
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    session.inbox.send(delivery("hi")).unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let first = session.lines();
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    // A shutdown requested with the switch pending.
    session.cancel.shutdown(143);
    let outcome = session.turn();
    assert_eq!(outcome, None, "no next turn starts");
    let shutdown = log::read(&session.dir).unwrap();
    assert_kinds(
        &shutdown,
        &[&[
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "model_changed",
        ] as &[&str]],
    );
    assert_eq!(
        shutdown.iter().filter(|l| l.kind == "turn_started").count(),
        1
    );
}

#[test]
fn close_applies_after_the_closing_turn() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::with_tools_injecting(
        vec![Scripted::text("Old.")],
        vec![model(NEW_MODEL, None)],
        Vec::new(),
    );
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    session.inbox.send(delivery("hi")).unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let first = session.lines();
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    let (close_tx, close_rx) = mpsc::channel::<Answer>();
    session
        .inbox
        .send(Delivery::Close(Ack(Box::new(move |answer| {
            let _sent = close_tx.send(answer);
        }))))
        .unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, None);
    assert!(
        close_rx
            .recv_timeout(DEADLINE)
            .expect("close answered")
            .is_ok()
    );
    let closed = log::read(&session.dir).unwrap();
    assert_kinds(
        &closed,
        &[&[
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "model_changed",
        ] as &[&str]],
    );
}

#[test]
fn thinking_only_switches_the_level_with_the_same_events() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::new(
        vec![
            support::reasoning_reply("Hmm.", "Old."),
            Scripted::text("New."),
        ],
        None,
    );
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REASONING_REPLY, ENDED]);

    session.inbox.send(model(MODEL, Some("high"))).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
    let changed = of_kind(&lines, "model_changed");
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].payload["before"]["model"], MODEL);
    assert_eq!(changed[0].payload["after"]["model"], MODEL);
    assert_eq!(changed[0].payload["after"]["thinking"], "high");
    let requests = next.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].thinking, Some(ThinkingLevel::High));
    // The same model keeps its reasoning: it is still sent.
    assert!(
        requests[0]
            .conversation
            .iter()
            .any(|input| matches!(input, contract::provider::Input::Reasoning { .. }))
    );
}

#[test]
fn another_model_leaves_out_earlier_reasoning() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::new(
        vec![reasoning_reply("Hmm.", "Old."), Scripted::text("New.")],
        None,
    );
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REASONING_REPLY, ENDED]);

    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
    // The request goes to the new provider; the provider layer leaves out
    // reasoning another model reference produced. The conversation still
    // holds it under the old stamp.
    let requests = next.requests();
    assert_eq!(requests.len(), 1);
    let live: Vec<String> = log::read(&session.dir)
        .unwrap()
        .into_iter()
        .map(|l| l.kind)
        .collect();
    assert!(live.contains(&"model_changed".to_owned()));
}

#[test]
fn chosen_persists_to_a_model_only_switch_and_queued_switches_see_each_other() {
    let next = new_provider(vec![Scripted::text("A."), Scripted::text("B.")]);
    let recorded: Arc<Mutex<Vec<Option<ThinkingLevel>>>> = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);

    // Thinking `high` chosen, then a model-only switch: the new model
    // keeps `high`.
    session.inbox.send(model(NEW_MODEL, Some("high"))).unwrap();
    let (outcome, second) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&second, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, third) = run(&mut session, "third");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&third, &[&["turn_started"] as &[&str], STEP, REPLY, ENDED]);
    let requests = next.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].thinking, Some(ThinkingLevel::High));
    assert_eq!(requests[1].thinking, Some(ThinkingLevel::High));

    // Two queued switches see each other: the second is given the first's choice.
    recorded.lock().unwrap().clear();
    let third = new_provider(vec![Scripted::text("C.")]);
    let recorded2 = Arc::clone(&recorded);
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&third), NEW_MODEL, recorded2),
        switchable(),
    );
    session.inbox.send(model(NEW_MODEL, Some("low"))).unwrap();
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    session.inbox.send(delivery("fourth")).unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let fourth = session.lines();
    assert_kinds(
        &fourth,
        &[
            &["model_changed", "preamble_built", "turn_started"] as &[&str],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    let seen = recorded.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1], Some(ThinkingLevel::Low));
}

#[test]
fn a_new_model_lacking_the_chosen_level_is_invalid_arguments() {
    let next = new_provider(vec![]);
    let prepare: Prepare = Arc::new(
        move |args: &contract::commands::ModelArgs, chosen: Option<ThinkingLevel>| {
            let _ = chosen;
            let thinking = match &args.thinking {
                Some(level) => Some(level.parse::<ThinkingLevel>().map_err(|_| Rejection {
                    code: ErrorCode::InvalidArguments,
                    message: format!("unknown thinking level `{level}`"),
                })?),
                None => None,
            };
            // The new model declares no levels: a chosen level fails.
            if thinking.is_some() {
                return Err(Rejection {
                    code: ErrorCode::InvalidArguments,
                    message: "`high` is not a thinking level of `fake/model-2`".into(),
                });
            }
            Ok(Prepared {
                provider: Arc::clone(&next) as Arc<dyn Provider>,
                model: model_of(NEW_MODEL),
                thinking: None,
                chosen: None,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: None,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: None,
                notice: None,
            })
        },
    );
    let mut session = Session::new(
        vec![Scripted::text("Old."), Scripted::text("Old again.")],
        None,
    );
    with_switch(&mut session, prepare, switchable());
    let (tx, rx) = mpsc::channel::<Answer>();
    session
        .inbox
        .send(support::model_reported(NEW_MODEL, Some("high"), tx))
        .unwrap();
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    // Admitted while idle ahead of the prompt: rejected, so the turn runs on.
    let rejected = rx
        .recv_timeout(DEADLINE)
        .expect("the model is answered")
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::InvalidArguments);
}

#[test]
fn reviewer_collision_is_invalid_arguments_and_changes_nothing() {
    let provider = new_provider(vec![]);
    let review_provider = new_provider(vec![]);
    let prepare: Prepare = Arc::new(move |_, _| {
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: None,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: Ok(Reviewer {
                provider: Arc::clone(&review_provider) as Arc<dyn Provider>,
                model: model_of(NEW_MODEL),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: None,
            }),
            web_search: None,
            notice: None,
        })
    });
    let mut session = Session::new(
        vec![Scripted::text("Old."), Scripted::text("Old again.")],
        None,
    );
    with_switch(&mut session, prepare, switchable());
    let (tx, rx) = mpsc::channel::<Answer>();
    session
        .inbox
        .send(support::model_reported(NEW_MODEL, None, tx))
        .unwrap();
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    let rejected = rx
        .recv_timeout(DEADLINE)
        .expect("the model is answered")
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::InvalidArguments);
    assert!(rejected.message.contains(NEW_MODEL));
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[&["turn_started"] as &[&str], STEP, REPLY, ENDED]);
    assert!(of_kind(&lines, "model_changed").is_empty());
}

#[test]
fn hosted_search_mismatch_is_invalid_arguments_in_both_directions() {
    for prepared_search in [None, Some("other_search".to_owned())] {
        let provider = new_provider(vec![]);
        let prepare: Prepare = Arc::new(move |_, _| {
            Ok(Prepared {
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: model_of(NEW_MODEL),
                thinking: None,
                chosen: None,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: None,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: prepared_search.clone(),
                notice: None,
            })
        });
        let mut session = Session::new(
            vec![Scripted::text("Old."), Scripted::text("Old again.")],
            None,
        );
        with_switch(
            &mut session,
            prepare,
            Switchable {
                chosen: None,
                web_search: Some("web_search_20250305".into()),
            },
        );
        let (tx, rx) = mpsc::channel::<Answer>();
        session
            .inbox
            .send(support::model_reported(NEW_MODEL, None, tx))
            .unwrap();
        let (outcome, first) = run(&mut session, "hi");
        assert_eq!(outcome, Some(TurnOutcome::Completed));
        assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
        let rejected = rx
            .recv_timeout(DEADLINE)
            .expect("the model is answered")
            .unwrap_err();
        assert_eq!(rejected.code, ErrorCode::InvalidArguments);
    }
}

#[test]
fn two_switches_apply_in_order_with_one_rebuild() {
    let second = new_provider(vec![Scripted::text("Second.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let first_provider = new_provider(vec![]);
    // The first prepare switches to an intermediate model; the second to the final one.
    let mid = Arc::clone(&first_provider);
    let fin = Arc::clone(&second);
    let prepare: Prepare = Arc::new(
        move |args: &contract::commands::ModelArgs, chosen: Option<ThinkingLevel>| {
            recorded.lock().unwrap().push(chosen);
            let (provider, reference) = if args.model == "fake/mid" {
                (Arc::clone(&mid), "fake/mid")
            } else {
                (Arc::clone(&fin), NEW_MODEL)
            };
            Ok(Prepared {
                provider: provider as Arc<dyn Provider>,
                model: model_of(reference),
                thinking: None,
                chosen,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: None,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: None,
                notice: None,
            })
        },
    );
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    with_switch(&mut session, prepare, switchable());
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    session.inbox.send(model("fake/mid", None)).unwrap();
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            &[
                "model_changed",
                "model_changed",
                "preamble_built",
                "turn_started",
            ] as &[&str],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    let changed = of_kind(&lines, "model_changed");
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[0].payload["after"]["model"], "fake/mid");
    assert_eq!(changed[1].payload["before"]["model"], "fake/mid");
    assert_eq!(changed[1].payload["after"]["model"], NEW_MODEL);
    assert_eq!(of_kind(&lines, "preamble_built").len(), 1);
    assert_eq!(second.requests().len(), 1);
}

#[test]
fn a_noop_switch_writes_nothing_but_keeps_the_choice() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::new(
        vec![Scripted::text("Old."), Scripted::text("Old again.")],
        None,
    );
    with_switch(
        &mut session,
        prepare_to(Arc::clone(&next), MODEL, Arc::clone(&recorded)),
        switchable(),
    );
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    // Same model, same thinking, same credential and lifetime: a no-op.
    session.inbox.send(model(MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[&["turn_started"] as &[&str], STEP, REPLY, ENDED]);
    assert!(of_kind(&lines, "model_changed").is_empty());
    assert!(
        of_kind(&lines, "preamble_built").is_empty(),
        "a no-op rebuilds nothing"
    );
}

#[test]
fn a_notice_is_written_after_model_changed() {
    let provider = new_provider(vec![Scripted::text("New.")]);
    let prepare: Prepare = Arc::new(move |_, _| {
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: None,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: no_reviewer(),
            web_search: None,
            notice: Some(Notice {
                code: ErrorCode::ConfigKeyIgnored,
                message: "Thinking `high` is not a level of `fake/model-2`.".into(),
                extension: None,
            }),
        })
    });
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    with_switch(&mut session, prepare, switchable());
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            &["model_changed", "notice", "preamble_built", "turn_started"] as &[&str],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    let kinds_now = kinds(&lines);
    let changed_at = kinds_now
        .iter()
        .position(|k| *k == "model_changed")
        .unwrap();
    let notice_at = kinds_now.iter().position(|k| *k == "notice").unwrap();
    let built_at = kinds_now
        .iter()
        .position(|k| *k == "preamble_built")
        .unwrap();
    assert!(changed_at < notice_at && notice_at < built_at);
}

#[test]
fn a_switch_replaces_the_reviewer() {
    let provider = new_provider(vec![Scripted::text("New.")]);
    let new_review = new_provider(vec![Scripted::text("allow")]);
    let review_handle = Arc::clone(&new_review);
    let prepare: Prepare = Arc::new(move |_, _| {
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: None,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: Ok(Reviewer {
                provider: Arc::clone(&review_handle) as Arc<dyn Provider>,
                model: model_of("fake/reviewer-2"),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: None,
            }),
            web_search: None,
            notice: None,
        })
    });
    let tool = executes_tool();
    let mut session = Session::with_tools(
        vec![Scripted::text("Old.")],
        None,
        vec![tool as Arc<dyn contract::tool::Tool>],
    );
    with_switch(&mut session, prepare, switchable());
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
    assert_eq!(of_kind(&lines, "model_changed").len(), 1);
    assert_eq!(new_review.requests().len(), 0, "no review ran yet");
}

#[test]
fn resume_interleaving_holds_arrival_order() {
    use contract::events::RuleScope;
    use contract::events::{
        AskStep, Empty, PermissionRequested, SessionStarted, StandingRule, TurnStarted as TurnBegin,
    };
    use contract::shapes::{ContentPart as Part, DeclaredEffects, Origin, Sender as From};
    use contract::{ActionId as Aid, CommandId as Cid, SessionId as Sid, TurnId as Tid};

    let root = fakes::TempDir::new("fiber-switch-resume");
    let workspace_dir = root.path().join("w");
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let credentials = root.path().join("credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let log = Arc::new(
        log::Log::create(
            root.path(),
            Sid("s_1".into()),
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        )
        .unwrap(),
    );
    let dir = root.path().join("s_1");
    let tid = Tid("t_1".into());
    let append = |event: contract::events::Event, turn: Option<Tid>, action: Option<Aid>| {
        log.append(&event, turn, action).unwrap();
    };
    append(
        contract::events::Event::SessionStarted(SessionStarted {
            workspace: workspace_dir.display().to_string(),
            variables: contract::events::Variables {
                path: String::new(),
                names: Vec::new(),
                source: contract::events::VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
        }),
        None,
        None,
    );
    append(
        contract::events::Event::TurnStarted(TurnBegin {
            input: vec![contract::events::InputItem::Message {
                content: vec![Part::Text { text: "one".into() }],
                sender: From {
                    origin: Origin::Driver,
                    command_id: Some(Cid("c_1".into())),
                },
                changed_by: None,
            }],
        }),
        Some(tid.clone()),
        None,
    );
    append(
        contract::events::Event::AssistantMessageStarted(Empty {}),
        Some(tid.clone()),
        Some(Aid("a_0".into())),
    );
    append(
        contract::events::Event::ToolCallRequested(contract::events::ToolCallRequested {
            name: "read".into(),
            arguments: serde_json::json!({"city": "Paris"}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        }),
        Some(tid.clone()),
        Some(Aid("a_1".into())),
    );
    append(
        contract::events::Event::PermissionRequested(PermissionRequested {
            request_id: RequestId("r_9".into()),
            declared: DeclaredEffects {
                effects: vec![contract::shapes::Effect::Executes],
                reversible: true,
                paths: None,
            },
            step: AskStep::StandingAsk {
                standing_rule: StandingRule {
                    scope: RuleScope::Project,
                    prefix: "x".into(),
                },
            },
        }),
        Some(tid.clone()),
        Some(Aid("a_1".into())),
    );
    append(
        contract::events::Event::FiberStarted(contract::events::FiberStarted {
            version: "test".into(),
            resumed: false,
        }),
        None,
        None,
    );
    append(
        contract::events::Event::FiberExited(contract::events::FiberExited {
            exit_code: 0,
            usage: contract::shapes::Usage {
                tokens: contract::shapes::Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: std::collections::BTreeMap::new(),
                    output: 0,
                },
                cost: Some(0.0),
                subscription_cost: 0.0,
            },
            final_message: None,
            error: None,
            suspended_on: Some(RequestId("r_9".into())),
            questions: None,
        }),
        None,
        None,
    );

    let provider_n = new_provider(vec![Scripted::text("N.")]);
    let provider_p = new_provider(vec![Scripted::text("P.")]);
    let pn = Arc::clone(&provider_n);
    let pp = Arc::clone(&provider_p);
    let prepare: Prepare = Arc::new(
        move |args: &contract::commands::ModelArgs, chosen: Option<ThinkingLevel>| {
            let (provider, reference) = if args.model == "fake/n" {
                (Arc::clone(&pn), "fake/n")
            } else {
                (Arc::clone(&pp), "fake/p")
            };
            Ok(Prepared {
                provider: provider as Arc<dyn Provider>,
                model: model_of(reference),
                thinking: None,
                chosen,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: None,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: None,
                notice: None,
            })
        },
    );

    let (inbox_tx, inbox_rx) = mpsc::channel::<Delivery>();
    let home = root.path().to_path_buf();
    let session_log = dir.join("events.jsonl").display().to_string();
    let prompt_clock = Arc::clone(&clock) as Arc<dyn contract::clock::Clock>;
    let mut prompt =
        r#loop::PromptInputs::new(home.clone(), "/bin/sh".into(), session_log, prompt_clock);
    prompt.credential = Some("work".into());
    let rules = Arc::new(support::FakeRules::empty());
    let tool = Arc::new(support::TestTool::reads("read", "ok"));
    let mut looped = r#loop::Loop::resume(
        Arc::clone(&log),
        r#loop::resumed(&dir).unwrap(),
        Arc::new(ScriptedProvider::new(vec![Scripted::text("Fin.")])) as Arc<dyn Provider>,
        model_of(MODEL),
        prompt,
        inbox_rx,
        vec![("builtin".into(), tool as Arc<dyn contract::tool::Tool>)],
        r#loop::Permissions {
            workspace: workspace_dir.display().to_string(),
            credentials,
            credential_files: Vec::new(),
            rules,
        },
    )
    .unwrap()
    .switcher(prepare, switchable());

    // A `model(n)` held aside before the finishing turn starts.
    inbox_tx.send(model("fake/n", None)).unwrap();
    // The finishing turn runs on its own thread; a live `model(p)` arrives
    // during its approval wait and holds behind `n`.
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    // Wait for the re-raised request, then send the live switch and the reply.
    let mut watcher = log.watch();
    let request_id = loop {
        let line = watcher
            .recv_timeout(DEADLINE)
            .expect("a line in time")
            .expect("log")
            .expect("line");
        if line.kind == "permission_requested" {
            break RequestId(line.payload["request_id"].as_str().unwrap().into());
        }
    };
    assert_eq!(request_id.0, "r_9");
    inbox_tx.send(model("fake/p", None)).unwrap();
    inbox_tx
        .send(Delivery::Reply(
            contract::commands::Reply {
                request_id: RequestId("r_9".into()),
                answer: allow_answer(),
            },
            support::ignore(),
        ))
        .unwrap();
    let (mut looped, outcome) = finished
        .recv_timeout(DEADLINE)
        .expect("the finishing turn ended");
    assert_eq!(outcome, Some(TurnOutcome::Completed));

    // The next turn admits both in arrival order: `n` before `p`.
    inbox_tx.send(support::delivery("next")).unwrap();
    let outcome = looped.turn().unwrap();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = log::read(&dir).unwrap();
    assert_kinds(
        &lines,
        &[&[
            "session_started",
            "turn_started",
            "assistant_message_started",
            "tool_call_requested",
            "permission_requested",
            "fiber_started",
            "fiber_exited",
            "preamble_built",
            "opening_message",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "model_changed",
            "model_changed",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ] as &[&str]],
    );
    let changed: Vec<&contract::Envelope> =
        lines.iter().filter(|l| l.kind == "model_changed").collect();
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[0].payload["after"]["model"], "fake/n");
    assert_eq!(changed[1].payload["before"]["model"], "fake/n");
    assert_eq!(changed[1].payload["after"]["model"], "fake/p");
    assert_eq!(
        provider_p.requests().len(),
        1,
        "the next request goes to `p`"
    );
    assert!(provider_n.requests().is_empty());
}
