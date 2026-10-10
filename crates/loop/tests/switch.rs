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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::clock::Clock as _;
use contract::events::{CacheLifetime, Notice, TurnOutcome};
use contract::inbox::{Ack, Answer, Delivery, Rejection};
use contract::provider::Provider;
use contract::{Envelope, ErrorCode, ThinkingLevel};
use fakes::{Scripted, ScriptedProvider};
use r#loop::{HandoffSettings, Hosted, Model, NO_SWITCH, Prepare, Prepared, Reviewer, Switchable};

use support::{
    DEADLINE, ENDED, MODEL, OPENING, REPLY, STEP, Session, allow, assert_kinds, delivery, kinds,
    model, of_kind,
};

const NEW_MODEL: &str = "fake/model-2";

/// The window a prepared switch declares: the switched preamble is built
/// for it, not for the start model's window.
const NEW_WINDOW: u64 = 200_000;

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
        move |args: &contract::commands::ModelArgs,
              _label: Option<&str>,
              chosen: Option<ThinkingLevel>| {
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
                context_window: NEW_WINDOW,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
            })
        },
    )
}

fn switchable() -> Switchable {
    Switchable { chosen: None }
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
const SWITCHED_OPENING: &[&str] = &["model_changed", "preamble_built", "turn_started"];
/// Whether the case switches the model or the credential label: the two
/// halves of every switch pair in the table below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Switch {
    Model,
    Credential,
}

/// The shared flow a pair of tests runs. Both halves run the same turns;
/// only the prepare function and the switch delivery swap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    /// One turn, the switch, another turn.
    BetweenTurns,
    /// The switch arrives while the first turn runs and applies after it.
    DuringTurn,
    /// The switch's prepare rejects, so nothing changes.
    Rejected,
    /// No switcher is installed, so the switch is invalid arguments.
    NoSwitcher,
    /// The switch arrives after close, so it is closing.
    AfterClose,
    /// The switch arrives during an approval wait and applies after the turn.
    ApprovalWait,
    /// The switch names what is already set, so nothing changes.
    Noop,
}

/// One row of the switch-pair table: `name` names the case in every
/// failure message, `switch` picks its half, `shape` picks the shared
/// flow. Each `#[test]` below is one row's one-line call.
struct Case {
    name: &'static str,
    switch: Switch,
    shape: Shape,
}

/// Asserts the complete, ordered event kinds of `lines` for the `name`
/// case, like support's `assert_kinds` but naming the case on failure.
fn assert_case_kinds(name: &str, lines: &[Envelope], parts: &[&[&str]]) {
    assert_eq!(kinds(lines), parts.concat(), "case {name}");
}

/// The switch delivery a `Switch` half sends carrying `value`: a `model`
/// command or a `credential` command.
fn switch_delivery(switch: Switch, value: &str) -> Delivery {
    match switch {
        Switch::Model => model(value, None),
        Switch::Credential => support::credential(value),
    }
}

/// What a `Shape::BetweenTurns` case's prepare recorded: the model
/// prepare's thinking levels, or the credential prepare's `(model, label)`
/// pairs.
enum Recorder {
    Model(Arc<Mutex<Vec<Option<ThinkingLevel>>>>),
    Credential(Seen),
}

/// Rejects the switch answer `answer`, naming the `name` case when the
/// switch was wrongly accepted.
fn rejected_answer(name: &str, answer: Answer) -> Rejection {
    match answer {
        Err(rejection) => rejection,
        Ok(ok) => panic!("case {name}: the switch was accepted: {ok:?}"),
    }
}

/// Runs one switch-pair case: the shared `shape` flow with the `switch`
/// half's prepare and delivery, then the case-specific assertions. Every
/// failure message names the case.
fn run_switch_case(case: &Case) {
    let name = case.name;
    match case.shape {
        Shape::BetweenTurns => {
            let next = new_provider(vec![Scripted::text("New.")]);
            let mut session = Session::new(vec![Scripted::text("Old.")], None);
            let recorder = match case.switch {
                Switch::Model => {
                    let recorded = Arc::new(Mutex::new(Vec::new()));
                    with_switch(
                        &mut session,
                        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
                        switchable(),
                    );
                    Recorder::Model(recorded)
                }
                Switch::Credential => {
                    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
                    with_switch(
                        &mut session,
                        prepare_keeping_model(Arc::clone(&next), Arc::clone(&seen)),
                        switchable(),
                    );
                    Recorder::Credential(seen)
                }
            };

            let (outcome, first) = run(&mut session, "hi");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(name, &first, &[OPENING, STEP, REPLY, ENDED]);

            let ack_rx = match case.switch {
                Switch::Model => {
                    session.inbox.send(model(NEW_MODEL, None)).unwrap();
                    None
                }
                Switch::Credential => {
                    let (tx, rx) = mpsc::channel();
                    session
                        .inbox
                        .send(support::credential_reported("home", tx))
                        .unwrap();
                    Some(rx)
                }
            };
            let (outcome, lines) = run(&mut session, "again");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            if let Some(rx) = ack_rx {
                let answer = rx
                    .recv_timeout(DEADLINE)
                    .unwrap_or_else(|_| panic!("case {name}: the credential is answered"));
                assert!(answer.is_ok(), "case {name}: accepted");
            }
            assert_case_kinds(name, &lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);

            let changed = of_kind(&lines, "model_changed");
            assert_eq!(changed.len(), 1, "case {name}");
            match case.switch {
                Switch::Model => {
                    let changed = &changed[0];
                    assert_eq!(changed.payload["before"]["model"], MODEL, "case {name}");
                    assert_eq!(changed.payload["after"]["model"], NEW_MODEL, "case {name}");
                    assert_eq!(changed.payload["source"], "driver", "case {name}");
                    assert!(changed.turn_id.is_none(), "case {name}");
                }
                Switch::Credential => {
                    assert_eq!(changed[0].payload["before"]["model"], MODEL, "case {name}");
                    assert_eq!(changed[0].payload["after"]["model"], MODEL, "case {name}");
                    assert_eq!(
                        changed[0].payload["before"]["credential"], "work",
                        "case {name}"
                    );
                    assert_eq!(
                        changed[0].payload["after"]["credential"], "home",
                        "case {name}"
                    );
                    assert_eq!(changed[0].payload["source"], "driver", "case {name}");
                }
            }

            let built = of_kind(&lines, "preamble_built");
            assert_eq!(built.len(), 1, "case {name}");
            assert_eq!(built[0].payload["reason"], "switch", "case {name}");
            match case.switch {
                Switch::Model => {
                    assert_eq!(built[0].payload["model"], NEW_MODEL, "case {name}");
                    assert_eq!(
                        built[0].payload["context_window"], NEW_WINDOW,
                        "case {name}"
                    );
                }
                Switch::Credential => {
                    assert_eq!(built[0].payload["model"], MODEL, "case {name}");
                    assert_eq!(built[0].payload["credential"], "home", "case {name}");
                }
            }

            match recorder {
                Recorder::Model(recorded) => {
                    assert_eq!(*recorded.lock().unwrap(), vec![None], "case {name}");
                }
                Recorder::Credential(seen) => {
                    assert_eq!(
                        seen.lock().unwrap().as_slice(),
                        [(MODEL.to_owned(), Some("home".to_owned()))],
                        "case {name}"
                    );
                }
            }
            assert_eq!(
                session.requests().len(),
                1,
                "case {name}: the old provider keeps one call"
            );
            let after = match case.switch {
                Switch::Model => "new",
                Switch::Credential => "prepared",
            };
            assert_eq!(
                next.requests().len(),
                1,
                "case {name}: the next turn goes to the {after} provider"
            );
        }
        Shape::DuringTurn => {
            let script = match case.switch {
                Switch::Model => vec![Scripted::text("Old."), Scripted::text("New.")],
                Switch::Credential => vec![Scripted::text("Old."), Scripted::text("Spare.")],
            };
            let next = new_provider(vec![Scripted::text("New.")]);
            let injected = match case.switch {
                Switch::Model => vec![model(NEW_MODEL, None)],
                Switch::Credential => vec![support::credential("home")],
            };
            let mut session = Session::with_tools_injecting(script, injected, Vec::new());
            match case.switch {
                Switch::Model => {
                    let recorded = Arc::new(Mutex::new(Vec::new()));
                    with_switch(
                        &mut session,
                        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
                        switchable(),
                    );
                    // The first turn runs on the old provider; the switch it took waits
                    // in `pending`.
                }
                Switch::Credential => {
                    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
                    with_switch(
                        &mut session,
                        prepare_keeping_model(Arc::clone(&next), Arc::clone(&seen)),
                        switchable(),
                    );
                    // The first turn runs on the old label; the switch it took waits in
                    // `pending`.
                }
            }
            session.inbox.send(delivery("hi")).unwrap();
            let outcome = session.turn();
            let first = session.lines();
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(name, &first, &[OPENING, STEP, REPLY, ENDED]);
            assert_eq!(session.requests().len(), 1, "case {name}");
            assert!(next.requests().is_empty(), "case {name}");

            let (outcome, lines) = run(&mut session, "again");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(name, &lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
            if case.switch == Switch::Credential {
                let changed = of_kind(&lines, "model_changed");
                assert_eq!(changed.len(), 1, "case {name}");
                assert_eq!(
                    changed[0].payload["before"]["credential"], "work",
                    "case {name}"
                );
                assert_eq!(
                    changed[0].payload["after"]["credential"], "home",
                    "case {name}"
                );
            }
            assert_eq!(next.requests().len(), 1, "case {name}");
        }
        Shape::Rejected => {
            let next = new_provider(vec![Scripted::text("New.")]);
            let mut session = Session::new(
                vec![Scripted::text("Old."), Scripted::text("Old again.")],
                None,
            );
            let (tx, rx) = mpsc::channel();
            let send = match case.switch {
                Switch::Model => support::model_reported(NEW_MODEL, None, tx),
                Switch::Credential => support::credential_reported("nope", tx),
            };
            match case.switch {
                Switch::Model => {
                    let prepare: Prepare = Arc::new(|_, _, _| {
                        Err(Rejection {
                            code: ErrorCode::InvalidArguments,
                            message: "no such model".into(),
                        })
                    });
                    with_switch(&mut session, prepare, switchable());
                }
                Switch::Credential => {
                    let prepare: Prepare = Arc::new(|_, _, _| {
                        Err(Rejection {
                            code: ErrorCode::CredentialMissing,
                            message:
                                "`fake` has no credential label `nope`. The labels for `fake` are: work"
                                    .into(),
                        })
                    });
                    with_switch(&mut session, prepare, switchable());
                }
            }
            session.inbox.send(send).unwrap();
            let (outcome, first) = run(&mut session, "hi");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(name, &first, &[OPENING, STEP, REPLY, ENDED]);
            let what = match case.switch {
                Switch::Model => "the model is answered",
                Switch::Credential => "the credential is answered",
            };
            let answer = rx
                .recv_timeout(DEADLINE)
                .unwrap_or_else(|_| panic!("case {name}: {what}"));
            let rejection = rejected_answer(name, answer);
            match case.switch {
                Switch::Model => {
                    assert_eq!(rejection.code, ErrorCode::InvalidArguments, "case {name}");
                }
                Switch::Credential => {
                    assert_eq!(rejection.code, ErrorCode::CredentialMissing, "case {name}");
                }
            }

            let (outcome, lines) = run(&mut session, "again");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(
                name,
                &lines,
                &[&["turn_started"] as &[&str], STEP, REPLY, ENDED],
            );
            assert!(of_kind(&lines, "model_changed").is_empty(), "case {name}");
            assert_eq!(session.requests().len(), 2, "case {name}");
            assert!(next.requests().is_empty(), "case {name}");
        }
        Shape::NoSwitcher => {
            let mut session = Session::new(vec![Scripted::text("Old.")], None);
            let (tx, rx) = mpsc::channel();
            let send = match case.switch {
                Switch::Model => support::model_reported(NEW_MODEL, None, tx),
                Switch::Credential => support::credential_reported("home", tx),
            };
            session.inbox.send(send).unwrap();
            let (outcome, lines) = run(&mut session, "hi");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(name, &lines, &[OPENING, STEP, REPLY, ENDED]);
            let what = match case.switch {
                Switch::Model => "the model is answered",
                Switch::Credential => "the credential is answered",
            };
            let answer = rx
                .recv_timeout(DEADLINE)
                .unwrap_or_else(|_| panic!("case {name}: {what}"));
            let rejection = rejected_answer(name, answer);
            assert_eq!(rejection.code, ErrorCode::InvalidArguments, "case {name}");
            assert_eq!(rejection.message, NO_SWITCH, "case {name}");
            if case.switch == Switch::Model {
                assert!(of_kind(&lines, "model_changed").is_empty(), "case {name}");
            }
        }
        Shape::AfterClose => {
            let next = new_provider(vec![Scripted::text("New.")]);
            let called = Arc::new(Mutex::new(false));
            let flag = Arc::clone(&called);
            let prepare: Prepare = Arc::new(move |_, _, _| {
                *flag.lock().unwrap() = true;
                Ok(Prepared {
                    provider: Arc::clone(&next) as Arc<dyn Provider>,
                    model: model_of(NEW_MODEL),
                    thinking: None,
                    chosen: None,
                    credential: Some("home".into()),
                    cache_lifetime: CacheLifetime::OneHour,
                    context_window: fakes::CONTEXT_WINDOW,
                    addendum: None,
                    handoff: HandoffSettings::default(),
                    reviewer: no_reviewer(),
                    web_search: Hosted::Keep,
                    notice: None,
                    applied: None,
                    credential_files: Vec::new(),
                })
            });
            let mut session = Session::new(vec![Scripted::text("Old.")], None);
            with_switch(&mut session, prepare, switchable());
            let (outcome, first) = run(&mut session, "hi");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(name, &first, &[OPENING, STEP, REPLY, ENDED]);

            let (close_tx, close_rx) = mpsc::channel::<Answer>();
            session
                .inbox
                .send(Delivery::Close(Ack(Box::new(move |answer| {
                    let _sent = close_tx.send(answer);
                }))))
                .unwrap();
            let switch_rx = match case.switch {
                Switch::Model => {
                    let (model_tx, model_rx) = mpsc::channel::<Answer>();
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
                    model_rx
                }
                Switch::Credential => {
                    let (cred_tx, cred_rx) = mpsc::channel::<Answer>();
                    session
                        .inbox
                        .send(support::credential_reported("home", cred_tx))
                        .unwrap();
                    cred_rx
                }
            };
            let outcome = session.turn();
            assert_eq!(outcome, None, "case {name}");
            assert!(
                close_rx
                    .recv_timeout(DEADLINE)
                    .unwrap_or_else(|_| panic!("case {name}: close answered"))
                    .is_ok(),
                "case {name}"
            );
            let what = match case.switch {
                Switch::Model => "model answered",
                Switch::Credential => "credential answered",
            };
            let answer = switch_rx
                .recv_timeout(DEADLINE)
                .unwrap_or_else(|_| panic!("case {name}: {what}"));
            let rejected = rejected_answer(name, answer);
            assert_eq!(rejected.code, ErrorCode::Closing, "case {name}");
            assert!(
                !*called.lock().unwrap(),
                "case {name}: prepare runs only before `closing`"
            );
            if case.switch == Switch::Model {
                let all = log::read(&session.dir).unwrap();
                // The log holds the turn's durable lines only: no deltas, and the
                // refused `model` and `close` write nothing.
                assert_case_kinds(
                    name,
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
        }
        Shape::ApprovalWait => {
            let next = new_provider(vec![Scripted::text("New.")]);
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
            let switch = case.switch;
            match switch {
                Switch::Model => {
                    let recorded = Arc::new(Mutex::new(Vec::new()));
                    with_switch(
                        &mut session,
                        prepare_to(Arc::clone(&next), NEW_MODEL, Arc::clone(&recorded)),
                        switchable(),
                    );
                }
                Switch::Credential => {
                    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
                    with_switch(
                        &mut session,
                        prepare_keeping_model(Arc::clone(&next), Arc::clone(&seen)),
                        switchable(),
                    );
                }
            }
            let (ack_tx, ack_rx) = mpsc::channel::<Answer>();
            let answered = on_request(&session, {
                let inbox = session.inbox.clone();
                move |id| {
                    match switch {
                        Switch::Model => {
                            inbox
                                .send(support::model_reported(NEW_MODEL, None, ack_tx.clone()))
                                .unwrap();
                        }
                        Switch::Credential => {
                            inbox
                                .send(support::credential_reported("home", ack_tx.clone()))
                                .unwrap();
                        }
                    }
                    // The switch is accepted while the approval waits.
                    inbox
                        .send(Delivery::Reply(
                            contract::commands::Reply {
                                request_id: id,
                                answer: allow(),
                            },
                            support::ignore(),
                        ))
                        .unwrap();
                }
            });
            session.inbox.send(delivery("go")).unwrap();
            let outcome = session.turn();
            answered.join().unwrap();
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            let lines = session.lines();
            // Accepted during the wait, applied after the turn: no `model_changed` yet.
            assert_case_kinds(
                name,
                &lines,
                &[
                    OPENING,
                    STEP,
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
                    STEP,
                    REPLY,
                    ENDED,
                ],
            );
            assert!(of_kind(&lines, "model_changed").is_empty(), "case {name}");
            let what = match switch {
                Switch::Model => "the model is answered",
                Switch::Credential => "the credential is answered",
            };
            let answer = ack_rx
                .recv_timeout(DEADLINE)
                .unwrap_or_else(|_| panic!("case {name}: {what}"));
            assert!(
                answer.is_ok(),
                "case {name}: accepted before `turn_completed`"
            );

            let (outcome, lines) = run(&mut session, "again");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(name, &lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
            if switch == Switch::Credential {
                let changed = of_kind(&lines, "model_changed");
                assert_eq!(changed.len(), 1, "case {name}");
                assert_eq!(changed[0].payload["before"]["model"], MODEL, "case {name}");
                assert_eq!(changed[0].payload["after"]["model"], MODEL, "case {name}");
                assert_eq!(
                    changed[0].payload["before"]["credential"], "work",
                    "case {name}"
                );
                assert_eq!(
                    changed[0].payload["after"]["credential"], "home",
                    "case {name}"
                );
            }
            assert_eq!(next.requests().len(), 1, "case {name}");
        }
        Shape::Noop => {
            let next = new_provider(vec![Scripted::text("New.")]);
            let mut session = Session::new(
                vec![Scripted::text("Old."), Scripted::text("Old again.")],
                None,
            );
            match case.switch {
                Switch::Model => {
                    let recorded = Arc::new(Mutex::new(Vec::new()));
                    with_switch(
                        &mut session,
                        prepare_to(Arc::clone(&next), MODEL, Arc::clone(&recorded)),
                        switchable(),
                    );
                }
                Switch::Credential => {
                    with_switch(
                        &mut session,
                        prepare_keeping_model(Arc::clone(&next), Arc::new(Mutex::new(Vec::new()))),
                        switchable(),
                    );
                }
            }
            let (outcome, first) = run(&mut session, "hi");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            assert_case_kinds(name, &first, &[OPENING, STEP, REPLY, ENDED]);
            let ack_rx = match case.switch {
                Switch::Model => {
                    // Same model, same thinking, same credential and lifetime: a no-op.
                    session.inbox.send(model(MODEL, None)).unwrap();
                    None
                }
                Switch::Credential => {
                    // The command is still accepted.
                    let (tx, rx) = mpsc::channel();
                    session
                        .inbox
                        .send(support::credential_reported("work", tx))
                        .unwrap();
                    Some(rx)
                }
            };
            let (outcome, lines) = run(&mut session, "again");
            assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
            if let Some(rx) = ack_rx {
                let answer = rx
                    .recv_timeout(DEADLINE)
                    .unwrap_or_else(|_| panic!("case {name}: the credential is answered"));
                assert!(answer.is_ok(), "case {name}");
            }
            assert_case_kinds(
                name,
                &lines,
                &[&["turn_started"] as &[&str], STEP, REPLY, ENDED],
            );
            assert!(of_kind(&lines, "model_changed").is_empty(), "case {name}");
            match case.switch {
                Switch::Model => assert!(
                    of_kind(&lines, "preamble_built").is_empty(),
                    "case {name}: a no-op rebuilds nothing"
                ),
                Switch::Credential => {
                    assert!(of_kind(&lines, "preamble_built").is_empty(), "case {name}");
                    assert!(next.requests().is_empty(), "case {name}");
                }
            }
        }
    }
}

const BETWEEN_TURNS_MODEL: Case = Case {
    name: "between_turns_switches_before_the_next_turn_started",
    switch: Switch::Model,
    shape: Shape::BetweenTurns,
};

#[test]
fn between_turns_switches_before_the_next_turn_started() {
    run_switch_case(&BETWEEN_TURNS_MODEL);
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

const DURING_TURN_MODEL: Case = Case {
    name: "during_a_turn_applies_after_turn_completed",
    switch: Switch::Model,
    shape: Shape::DuringTurn,
};

#[test]
fn during_a_turn_applies_after_turn_completed() {
    run_switch_case(&DURING_TURN_MODEL);
}

const REJECTED_MODEL: Case = Case {
    name: "rejected_by_prepare_changes_nothing",
    switch: Switch::Model,
    shape: Shape::Rejected,
};

#[test]
fn rejected_by_prepare_changes_nothing() {
    run_switch_case(&REJECTED_MODEL);
}

const NO_SWITCHER_MODEL: Case = Case {
    name: "no_switcher_is_invalid_arguments",
    switch: Switch::Model,
    shape: Shape::NoSwitcher,
};

#[test]
fn no_switcher_is_invalid_arguments() {
    run_switch_case(&NO_SWITCHER_MODEL);
}

const AFTER_CLOSE_MODEL: Case = Case {
    name: "after_close_prepare_is_not_called",
    switch: Switch::Model,
    shape: Shape::AfterClose,
};

#[test]
fn after_close_prepare_is_not_called() {
    run_switch_case(&AFTER_CLOSE_MODEL);
}

use contract::RequestId;
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

const APPROVAL_WAIT_MODEL: Case = Case {
    name: "during_an_approval_wait_is_accepted_and_applied_after_the_turn",
    switch: Switch::Model,
    shape: Shape::ApprovalWait,
};

#[test]
fn during_an_approval_wait_is_accepted_and_applied_after_the_turn() {
    run_switch_case(&APPROVAL_WAIT_MODEL);
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
                    answer: allow(),
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
        move |args: &contract::commands::ModelArgs,
              _label: Option<&str>,
              chosen: Option<ThinkingLevel>| {
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
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
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
    let prepare: Prepare = Arc::new(move |_, _, _| {
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: Ok(Reviewer {
                provider: Arc::clone(&review_provider) as Arc<dyn Provider>,
                model: model_of(NEW_MODEL),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                thinking_levels: Vec::new(),
            }),
            web_search: Hosted::Keep,
            notice: None,
            applied: None,
            credential_files: Vec::new(),
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

/// A tool the provider hosts, of the vendor type `kind`.
struct HostedFake(&'static str);

impl contract::tool::Tool for HostedFake {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "web_search".into(),
            description: String::new(),
            input_schema: serde_json::json!({}),
            deferred: false,
            hosted: Some(self.0.into()),
        }
    }

    fn effects(
        &self,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Ok(contract::tool::Effects {
            declared: contract::shapes::DeclaredEffects {
                effects: vec![Effect::Network],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(
        &self,
        _arguments: &serde_json::Map<String, serde_json::Value>,
        _cancel: &dyn contract::tool::Cancel,
        _emit: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        contract::tool::Output::default()
    }
}

/// A `Prepare` that switches to `NEW_MODEL` on `provider` with the hosted
/// search `hosted` makes, counting each `applied` call in `applied`.
fn prepare_hosted(
    provider: Arc<ScriptedProvider>,
    hosted: impl Fn() -> Hosted + Send + Sync + 'static,
    applied: Arc<AtomicUsize>,
) -> Prepare {
    Arc::new(move |_, _, chosen| {
        let count = Arc::clone(&applied);
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: no_reviewer(),
            web_search: hosted(),
            notice: None,
            applied: Some(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            })),
            credential_files: Vec::new(),
        })
    })
}

/// The hosted type of the `web_search` a request declares, if it declares one.
fn hosted_in(request: &contract::provider::ModelRequest) -> Option<Option<String>> {
    request
        .tools
        .iter()
        .find(|tool| tool.name == "web_search")
        .map(|tool| tool.hosted.clone())
}

/// One session started with the hosted search of type `type_a`, switched
/// between turns with `hosted`: the start's request, the switched
/// request, and the switched turn's lines.
fn switch_hosted(
    hosted: impl Fn() -> Hosted + Send + Sync + 'static,
) -> (
    contract::provider::ModelRequest,
    contract::provider::ModelRequest,
    Vec<Envelope>,
) {
    let next = new_provider(vec![Scripted::text("New.")]);
    let mut session = Session::with_tools(
        vec![Scripted::text("Old.")],
        None,
        vec![Arc::new(HostedFake("type_a")) as Arc<dyn contract::tool::Tool>],
    );
    with_switch(
        &mut session,
        prepare_hosted(Arc::clone(&next), hosted, Arc::default()),
        switchable(),
    );
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
    let started = session.requests().remove(0);
    let switched = next.requests().remove(0);
    (started, switched, lines)
}

/// The names `preamble_built` declares in `lines`, with each one's
/// `registered_by`.
fn declared(lines: &[Envelope]) -> Vec<(String, String)> {
    let built = of_kind(lines, "preamble_built");
    built[0].payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().unwrap().to_owned(),
                tool["registered_by"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn a_declared_hosted_search_replaces_the_one_the_session_started_with() {
    let (started, switched, lines) =
        switch_hosted(|| Hosted::Declare(Arc::new(HostedFake("type_b"))));
    assert_eq!(hosted_in(&started), Some(Some("type_a".into())));
    assert_eq!(hosted_in(&switched), Some(Some("type_b".into())));
    assert_eq!(
        declared(&lines),
        vec![("web_search".to_owned(), "builtin".to_owned())]
    );
}

#[test]
fn a_withdrawn_hosted_search_is_not_declared() {
    let (started, switched, lines) = switch_hosted(|| Hosted::Withdraw("web_search".into()));
    assert_eq!(hosted_in(&started), Some(Some("type_a".into())));
    assert_eq!(hosted_in(&switched), None);
    assert!(declared(&lines).is_empty());
}

#[test]
fn a_kept_hosted_search_stays() {
    let (_, switched, lines) = switch_hosted(|| Hosted::Keep);
    assert_eq!(hosted_in(&switched), Some(Some("type_a".into())));
    assert_eq!(
        declared(&lines),
        vec![("web_search".to_owned(), "builtin".to_owned())]
    );
}

#[test]
fn applied_runs_once_when_the_switch_applies() {
    // Idle: the switch applies at once.
    let applied = Arc::new(AtomicUsize::new(0));
    let next = new_provider(vec![Scripted::text("New.")]);
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    with_switch(
        &mut session,
        prepare_hosted(Arc::clone(&next), || Hosted::Keep, Arc::clone(&applied)),
        switchable(),
    );
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, _) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(applied.load(Ordering::SeqCst), 1);

    // Admitted during a turn: it runs after `turn_completed`, when the
    // next turn applies the switch.
    let applied = Arc::new(AtomicUsize::new(0));
    let next = new_provider(vec![Scripted::text("New.")]);
    let mut session = Session::with_tools_injecting(
        vec![Scripted::text("Old.")],
        vec![model(NEW_MODEL, None)],
        Vec::new(),
    );
    with_switch(
        &mut session,
        prepare_hosted(Arc::clone(&next), || Hosted::Keep, Arc::clone(&applied)),
        switchable(),
    );
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    assert_eq!(applied.load(Ordering::SeqCst), 0, "not before the boundary");
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[SWITCHED_OPENING, STEP, REPLY, ENDED]);
    assert_eq!(applied.load(Ordering::SeqCst), 1);
}

#[test]
fn applied_never_runs_for_a_rejected_or_noop_switch() {
    // Rejected by the loop's own check: the new reviewer is the new model.
    let applied = Arc::new(AtomicUsize::new(0));
    let provider = new_provider(vec![]);
    let count = Arc::clone(&applied);
    let prepare: Prepare = Arc::new(move |_, _, _| {
        let count = Arc::clone(&count);
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: Ok(Reviewer {
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: model_of(NEW_MODEL),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                thinking_levels: Vec::new(),
            }),
            web_search: Hosted::Keep,
            notice: None,
            applied: Some(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            })),
            credential_files: Vec::new(),
        })
    });
    let mut session = Session::new(vec![Scripted::text("Old.")], None);
    with_switch(&mut session, prepare, switchable());
    let (tx, rx) = mpsc::channel::<Answer>();
    session
        .inbox
        .send(support::model_reported(NEW_MODEL, None, tx))
        .unwrap();
    let (outcome, lines) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[OPENING, STEP, REPLY, ENDED]);
    let rejected = rx
        .recv_timeout(DEADLINE)
        .expect("the model is answered")
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::InvalidArguments);
    assert_eq!(applied.load(Ordering::SeqCst), 0);

    // A no-op: the same model, thinking, credential and lifetime.
    let applied = Arc::new(AtomicUsize::new(0));
    let same = new_provider(vec![]);
    let count = Arc::clone(&applied);
    let noop: Prepare = Arc::new(move |_, _, chosen| {
        let count = Arc::clone(&count);
        Ok(Prepared {
            provider: Arc::clone(&same) as Arc<dyn Provider>,
            model: model_of(MODEL),
            thinking: None,
            chosen,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: no_reviewer(),
            web_search: Hosted::Withdraw("web_search".into()),
            notice: None,
            applied: Some(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            })),
            credential_files: Vec::new(),
        })
    });
    let mut session = Session::new(
        vec![Scripted::text("Old."), Scripted::text("Old again.")],
        None,
    );
    with_switch(&mut session, noop, switchable());
    let (outcome, first) = run(&mut session, "hi");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&first, &[OPENING, STEP, REPLY, ENDED]);
    session.inbox.send(model(MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&lines, &[&["turn_started"] as &[&str], STEP, REPLY, ENDED]);
    assert_eq!(applied.load(Ordering::SeqCst), 0);
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
        move |args: &contract::commands::ModelArgs,
              _label: Option<&str>,
              chosen: Option<ThinkingLevel>| {
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
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
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

const NOOP_MODEL: Case = Case {
    name: "a_noop_switch_writes_nothing_but_keeps_the_choice",
    switch: Switch::Model,
    shape: Shape::Noop,
};

#[test]
fn a_noop_switch_writes_nothing_but_keeps_the_choice() {
    run_switch_case(&NOOP_MODEL);
}

#[test]
fn a_notice_is_written_after_model_changed() {
    let provider = new_provider(vec![Scripted::text("New.")]);
    let prepare: Prepare = Arc::new(move |_, _, _| {
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: no_reviewer(),
            web_search: Hosted::Keep,
            notice: Some(Notice {
                code: ErrorCode::ConfigKeyIgnored,
                message: "Thinking `high` is not a level of `fake/model-2`.".into(),
                extension: None,
            }),
            applied: None,
            credential_files: Vec::new(),
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
    let prepare: Prepare = Arc::new(move |_, _, _| {
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: Ok(Reviewer {
                provider: Arc::clone(&review_handle) as Arc<dyn Provider>,
                model: model_of("fake/reviewer-2"),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                thinking_levels: Vec::new(),
            }),
            web_search: Hosted::Keep,
            notice: None,
            applied: None,
            credential_files: Vec::new(),
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
            worktree: None,
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
        move |args: &contract::commands::ModelArgs,
              _label: Option<&str>,
              chosen: Option<ThinkingLevel>| {
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
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
            })
        },
    );

    let (inbox_tx, inbox_rx) = mpsc::channel::<Delivery>();
    let home = root.path().to_path_buf();
    let session_log = dir.join("events.jsonl").display().to_string();
    let prompt_clock = Arc::clone(&clock) as Arc<dyn contract::clock::Clock>;
    let mut prompt = r#loop::PromptInputs::new(
        home.clone(),
        "/bin/sh".into(),
        session_log,
        prompt_clock,
        fakes::CONTEXT_WINDOW,
    );
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
    // The watcher exists before the turn thread starts: it only sees events
    // appended after it is created, and the thread re-raises the request.
    let mut watcher = log.watch();
    // The finishing turn runs on its own thread; a live `model(p)` arrives
    // during its approval wait and holds behind `n`.
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    // Wait for the re-raised request, then send the live switch and the reply.
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
                answer: allow(),
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

/// A `Prepare` that switches to `NEW_MODEL` on `provider`, having read the
/// `file` credential source `read`.
fn prepare_reading(provider: Arc<ScriptedProvider>, read: std::path::PathBuf) -> Prepare {
    Arc::new(move |_, _, chosen| {
        Ok(Prepared {
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model_of(NEW_MODEL),
            thinking: None,
            chosen,
            credential: Some("work".into()),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: None,
            handoff: HandoffSettings::default(),
            reviewer: no_reviewer(),
            web_search: Hosted::Keep,
            notice: None,
            applied: None,
            credential_files: vec![read.clone()],
        })
    })
}

/// The `permission_resolved` lines of `lines`, by decision and who decided.
fn decisions(lines: &[Envelope]) -> Vec<(String, String)> {
    of_kind(lines, "permission_resolved")
        .into_iter()
        .map(|line| {
            (
                line.payload["decision"].as_str().unwrap().to_owned(),
                line.payload["decided_by"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn a_file_a_switch_read_is_denied_from_the_same_turn() {
    let keys = fakes::TempDir::new("fiber-switch-read-deny");
    let key = keys.path().join("key");
    std::fs::write(&key, "sk-switch-secret").unwrap();
    let key = key.canonicalize().unwrap();
    let read = Arc::new(TestTool::declaring(
        "read",
        "sk-switch-secret",
        vec![Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let peek = Arc::new(TestTool::reads("peek", "nothing"));
    let next = new_provider(vec![Scripted::text("New.")]);
    // The switch arrives during the first model call; the loop admits it
    // at the step boundary, and the next step's call reads the key.
    let mut session = Session::with_tools_injecting(
        vec![
            calls_reply("", &[("peek", serde_json::json!({"city": "Paris"}))]),
            calls_reply("", &[("read", serde_json::json!({"city": "Paris"}))]),
            Scripted::text("Done."),
        ],
        vec![model(NEW_MODEL, None)],
        vec![
            read.clone() as Arc<dyn contract::tool::Tool>,
            peek as Arc<dyn contract::tool::Tool>,
        ],
    );
    with_switch(
        &mut session,
        prepare_reading(Arc::clone(&next), key.clone()),
        switchable(),
    );
    let (outcome, lines) = run(&mut session, "go");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let call: &[&str] = &[
        "assistant_message_started",
        "assistant_message_delta",
        "tool_call_arguments_delta",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
    ];
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            call,
            &["tool_call_started", "tool_call_completed"],
            STEP,
            call,
            &["permission_resolved", "tool_call_completed"],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert!(
        of_kind(&lines, "model_changed").is_empty(),
        "not applied yet"
    );
    assert_eq!(
        decisions(&lines),
        vec![("deny".to_owned(), "credential_deny".to_owned())]
    );
    let denied = of_kind(&lines, "permission_resolved");
    assert_eq!(
        denied[0].payload["reason"],
        "The call touches a configured credential file."
    );
    assert!(read.ran().is_empty(), "a denied call never runs");
}

#[test]
fn a_file_an_idle_switch_read_is_denied_in_the_next_turn() {
    let keys = fakes::TempDir::new("fiber-switch-idle-deny");
    let key = keys.path().join("key");
    std::fs::write(&key, "sk-switch-secret").unwrap();
    let key = key.canonicalize().unwrap();
    let read = Arc::new(TestTool::declaring(
        "read",
        "sk-switch-secret",
        vec![Effect::Reads],
        Some(vec![key.display().to_string()]),
    ));
    let next = new_provider(vec![
        calls_reply("", &[("read", serde_json::json!({"city": "Paris"}))]),
        Scripted::text("Done."),
    ]);
    let mut session = Session::with_tools(
        vec![],
        None,
        vec![read.clone() as Arc<dyn contract::tool::Tool>],
    );
    with_switch(
        &mut session,
        prepare_reading(Arc::clone(&next), key.clone()),
        switchable(),
    );
    session.inbox.send(model(NEW_MODEL, None)).unwrap();
    let (outcome, lines) = run(&mut session, "go");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(
        decisions(&lines),
        vec![("deny".to_owned(), "credential_deny".to_owned())]
    );
    assert!(read.ran().is_empty(), "a denied call never runs");
}

/// Every `(model, label)` a recording credential `Prepare` saw, in order.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A `Prepare` that keeps the model and answers `Some(label)` with
/// `credential: Some(label)`: a `model` switch keeps `work`. Records every
/// `(model, label)` it was given, in order.
fn prepare_keeping_model(provider: Arc<ScriptedProvider>, seen: Seen) -> Prepare {
    Arc::new(
        move |args: &contract::commands::ModelArgs,
              label: Option<&str>,
              _chosen: Option<ThinkingLevel>| {
            seen.lock()
                .unwrap()
                .push((args.model.clone(), label.map(str::to_owned)));
            Ok(Prepared {
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: model_of(&args.model),
                thinking: None,
                chosen: None,
                credential: Some(label.unwrap_or("work").to_owned()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
            })
        },
    )
}

const BETWEEN_TURNS_CREDENTIAL: Case = Case {
    name: "credential_between_turns_switches_before_the_next_turn_started",
    switch: Switch::Credential,
    shape: Shape::BetweenTurns,
};

#[test]
fn credential_between_turns_switches_before_the_next_turn_started() {
    run_switch_case(&BETWEEN_TURNS_CREDENTIAL);
}

const DURING_TURN_CREDENTIAL: Case = Case {
    name: "credential_during_a_turn_applies_after_turn_completed",
    switch: Switch::Credential,
    shape: Shape::DuringTurn,
};

#[test]
fn credential_during_a_turn_applies_after_turn_completed() {
    run_switch_case(&DURING_TURN_CREDENTIAL);
}

const NOOP_CREDENTIAL: Case = Case {
    name: "credential_with_the_current_label_changes_nothing",
    switch: Switch::Credential,
    shape: Shape::Noop,
};

#[test]
fn credential_with_the_current_label_changes_nothing() {
    run_switch_case(&NOOP_CREDENTIAL);
}

const REJECTED_CREDENTIAL: Case = Case {
    name: "credential_rejected_by_prepare_changes_nothing",
    switch: Switch::Credential,
    shape: Shape::Rejected,
};

#[test]
fn credential_rejected_by_prepare_changes_nothing() {
    run_switch_case(&REJECTED_CREDENTIAL);
}

#[test]
fn mixed_model_and_credential_switches_apply_in_arrival_order() {
    let next = new_provider(vec![Scripted::text("New.")]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::with_tools_injecting(
        vec![Scripted::text("Old."), Scripted::text("Spare.")],
        vec![
            model("fake/mid", None),
            support::credential("home"),
            model("fake/n", None),
        ],
        Vec::new(),
    );
    with_switch(
        &mut session,
        prepare_keeping_model(Arc::clone(&next), Arc::clone(&seen)),
        switchable(),
    );
    session.inbox.send(delivery("hi")).unwrap();
    let outcome = session.turn();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(&session.lines(), &[OPENING, STEP, REPLY, ENDED]);

    // Each resolves against the settings queued before it: the credential
    // takes the queued switch's model.
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [
            ("fake/mid".to_owned(), None),
            ("fake/mid".to_owned(), Some("home".to_owned())),
            ("fake/n".to_owned(), None),
        ]
    );

    let (outcome, lines) = run(&mut session, "again");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_kinds(
        &lines,
        &[
            &[
                "model_changed",
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
    assert_eq!(changed.len(), 3);
    assert_eq!(changed[0].payload["after"]["model"], "fake/mid");
    assert_eq!(changed[0].payload["after"]["credential"], "work");
    assert_eq!(changed[1].payload["before"]["model"], "fake/mid");
    assert_eq!(changed[1].payload["after"]["model"], "fake/mid");
    assert_eq!(changed[1].payload["after"]["credential"], "home");
    assert_eq!(changed[2].payload["before"]["credential"], "home");
    assert_eq!(changed[2].payload["after"]["model"], "fake/n");
    assert_eq!(changed[2].payload["after"]["credential"], "work");
    assert_eq!(next.requests().len(), 1);
}

const NO_SWITCHER_CREDENTIAL: Case = Case {
    name: "credential_without_a_switcher_is_invalid_arguments",
    switch: Switch::Credential,
    shape: Shape::NoSwitcher,
};

#[test]
fn credential_without_a_switcher_is_invalid_arguments() {
    run_switch_case(&NO_SWITCHER_CREDENTIAL);
}

const AFTER_CLOSE_CREDENTIAL: Case = Case {
    name: "credential_after_close_is_closing_and_prepare_is_not_called",
    switch: Switch::Credential,
    shape: Shape::AfterClose,
};

#[test]
fn credential_after_close_is_closing_and_prepare_is_not_called() {
    run_switch_case(&AFTER_CLOSE_CREDENTIAL);
}

const APPROVAL_WAIT_CREDENTIAL: Case = Case {
    name: "credential_during_an_approval_wait_is_accepted_and_applied_after_the_turn",
    switch: Switch::Credential,
    shape: Shape::ApprovalWait,
};

#[test]
fn credential_during_an_approval_wait_is_accepted_and_applied_after_the_turn() {
    run_switch_case(&APPROVAL_WAIT_CREDENTIAL);
}

/// One row of the deferred-switch table: a switch held before a resumed
/// finishing turn (`held`) with another arriving live during its approval
/// wait (`live`), each a driver command sending `value`. `seen` holds the
/// prepare calls in arrival order, and `first_after`, `second_before` and
/// `second_after` hold the expected `model_changed` payloads (`None` skips
/// that side). `name` names the case in every failure message.
struct DeferredCase {
    name: &'static str,
    held: (Switch, &'static str),
    live: (Switch, &'static str),
    seen: &'static [(&'static str, Option<&'static str>)],
    first_after: (&'static str, &'static str),
    second_before: (Option<&'static str>, Option<&'static str>),
    second_after: (&'static str, &'static str),
}

/// Runs one deferred-switch case: a resumed finishing turn holds the
/// `held` switch, the `live` switch arrives during its approval wait, then
/// one more turn admits both in arrival order. Checks the prepare calls
/// and the `model_changed` payloads against the case row; every failure
/// message names the case.
fn run_deferred_case(case: &DeferredCase) {
    let name = case.name;
    let held = switch_delivery(case.held.0, case.held.1);
    let live = switch_delivery(case.live.0, case.live.1);
    use contract::events::RuleScope;
    use contract::events::{
        AskStep, Empty, PermissionRequested, SessionStarted, StandingRule, TurnStarted as TurnBegin,
    };
    use contract::shapes::{ContentPart as Part, DeclaredEffects, Origin, Sender as From};
    use contract::{ActionId as Aid, CommandId as Cid, SessionId as Sid, TurnId as Tid};

    let root = fakes::TempDir::new("fiber-switch-deferred-credential");
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
            worktree: None,
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

    let next = new_provider(vec![Scripted::text("Next.")]);
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let finishing = Arc::new(ScriptedProvider::new(vec![Scripted::text("Fin.")]));
    let prepare: Prepare = Arc::new(
        move |args: &contract::commands::ModelArgs,
              label: Option<&str>,
              chosen: Option<ThinkingLevel>| {
            recorded
                .lock()
                .unwrap()
                .push((args.model.clone(), label.map(str::to_owned)));
            Ok(Prepared {
                provider: Arc::clone(&next) as Arc<dyn Provider>,
                model: model_of(&args.model),
                thinking: None,
                chosen,
                credential: Some(label.unwrap_or("work").to_owned()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: no_reviewer(),
                web_search: Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
            })
        },
    );

    let (inbox_tx, inbox_rx) = mpsc::channel::<Delivery>();
    let home = root.path().to_path_buf();
    let session_log = dir.join("events.jsonl").display().to_string();
    let prompt_clock = Arc::clone(&clock) as Arc<dyn contract::clock::Clock>;
    let mut prompt = r#loop::PromptInputs::new(
        home.clone(),
        "/bin/sh".into(),
        session_log,
        prompt_clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let rules = Arc::new(support::FakeRules::empty());
    let tool = Arc::new(support::TestTool::reads("read", "ok"));
    let mut looped = r#loop::Loop::resume(
        Arc::clone(&log),
        r#loop::resumed(&dir).unwrap(),
        Arc::clone(&finishing) as Arc<dyn Provider>,
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

    // A switch held aside before the finishing turn starts.
    inbox_tx.send(held).unwrap();
    // The watcher exists before the turn thread starts: it only sees events
    // appended after it is created, and the thread re-raises the request.
    let watcher = log.watch();
    // The finishing turn runs on its own thread; a live switch arrives
    // during its approval wait and holds behind the held one.
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    // Wait for the re-raised request, then send the live switch and the reply:
    // one `DEADLINE` for the whole wait, never one per line.
    support::read_until(
        watcher,
        &format!("case {name}: a re-raised permission_requested line"),
        |line| line.kind == "permission_requested",
    );
    inbox_tx.send(live).unwrap();
    inbox_tx
        .send(Delivery::Reply(
            contract::commands::Reply {
                request_id: RequestId("r_9".into()),
                answer: allow(),
            },
            support::ignore(),
        ))
        .unwrap();
    let (mut looped, outcome) = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("case {name}: the finishing turn ended"));
    assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");

    // The next turn admits both in arrival order, bounded so a hang
    // reports the wait instead of hanging the test.
    inbox_tx.send(support::delivery("next")).unwrap();
    let (_looped, outcome) = fakes::within(
        &format!("case {name}: the next turn"),
        DEADLINE,
        move || {
            let outcome = looped.turn().unwrap();
            (looped, outcome)
        },
    );
    assert_eq!(outcome, Some(TurnOutcome::Completed), "case {name}");
    let lines = log::read(&dir).unwrap();
    assert_case_kinds(
        name,
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
    let changed: Vec<contract::Envelope> = lines
        .iter()
        .filter(|line| line.kind == "model_changed")
        .cloned()
        .collect();
    let seen = seen.lock().unwrap().clone();
    let expected: Vec<(String, Option<String>)> = case
        .seen
        .iter()
        .map(|(model, label)| ((*model).to_owned(), (*label).map(str::to_owned)))
        .collect();
    assert_eq!(seen, expected, "case {name}");
    assert_eq!(changed.len(), 2, "case {name}");
    assert_eq!(
        changed[0].payload["after"]["model"], case.first_after.0,
        "case {name}"
    );
    assert_eq!(
        changed[0].payload["after"]["credential"], case.first_after.1,
        "case {name}"
    );
    if let Some(before) = case.second_before.0 {
        assert_eq!(changed[1].payload["before"]["model"], before, "case {name}");
    }
    if let Some(before) = case.second_before.1 {
        assert_eq!(
            changed[1].payload["before"]["credential"], before,
            "case {name}"
        );
    }
    assert_eq!(
        changed[1].payload["after"]["model"], case.second_after.0,
        "case {name}"
    );
    assert_eq!(
        changed[1].payload["after"]["credential"], case.second_after.1,
        "case {name}"
    );
}

const DEFERRED_MODEL_CREDENTIAL: DeferredCase = DeferredCase {
    name: "deferred_model_then_live_credential_keeps_arrival_order",
    held: (Switch::Model, "fake/n"),
    live: (Switch::Credential, "home"),
    seen: &[("fake/n", None), ("fake/n", Some("home"))],
    first_after: ("fake/n", "work"),
    second_before: (Some("fake/n"), None),
    second_after: ("fake/n", "home"),
};

#[test]
fn deferred_model_then_live_credential_keeps_arrival_order() {
    run_deferred_case(&DEFERRED_MODEL_CREDENTIAL);
}

const DEFERRED_CREDENTIAL_MODEL: DeferredCase = DeferredCase {
    name: "deferred_credential_then_live_model_keeps_arrival_order",
    held: (Switch::Credential, "home"),
    live: (Switch::Model, "fake/p"),
    seen: &[(MODEL, Some("home")), ("fake/p", None)],
    first_after: (MODEL, "home"),
    second_before: (None, Some("home")),
    second_after: ("fake/p", "work"),
};

#[test]
fn deferred_credential_then_live_model_keeps_arrival_order() {
    run_deferred_case(&DEFERRED_CREDENTIAL_MODEL);
}

const DEFERRED_CREDENTIAL_CREDENTIAL: DeferredCase = DeferredCase {
    name: "deferred_credential_then_live_credential_keeps_arrival_order",
    held: (Switch::Credential, "home"),
    live: (Switch::Credential, "office"),
    seen: &[(MODEL, Some("home")), (MODEL, Some("office"))],
    first_after: (MODEL, "home"),
    second_before: (None, Some("home")),
    second_after: (MODEL, "office"),
};

#[test]
fn deferred_credential_then_live_credential_keeps_arrival_order() {
    run_deferred_case(&DEFERRED_CREDENTIAL_CREDENTIAL);
}
