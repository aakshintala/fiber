//! A running tool call raises an interaction and waits for its reply
//! (`docs/events.md`, "Interactions"): the loop writes both lines, fits the
//! reply, and resolves the interaction itself when no answer can come
//! (`docs/permissions.md`, "Headless"; `docs/architecture.md`,
//! "Cancellation" and "One inbox").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock as _;
use contract::commands::{Reply, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{
    Answer, CacheLifetime, Event, Interaction, InteractionRequested, Progress, TurnOutcome,
};
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use contract::inbox::{Ack, Delivery};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::provider::{Provider, ToolDefinition};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, True};
use contract::tool::{Answered, Ask, Asking, Cancel, Check, Effects, EffectsError, Output, Tool};
use contract::{ActionId, CommandId, Envelope, ErrorCode, JobId, RequestId};
use fakes::{Scripted, ScriptedProvider};
use r#loop::{Error, HandoffSettings, Loop, Model, Prepared, Switchable};
use serde_json::{Map, Value, json};

use support::{
    DEADLINE, Gate, OPENING, STEP, Script, Session, Tap, TestTool, assert_kinds, calls_reply,
    delivery, kinds, message, steer,
};

/// Builds one ask, given the asking call's own id.
type Make = Box<dyn Fn(&ActionId) -> Asking + Send + Sync>;

/// A tool whose call makes each ask of `asks` in turn, after `before`
/// opens when set, and returns each answer as a line of text.
struct Asks {
    asks: Vec<Make>,
    before: Option<Arc<Gate>>,
    told: Mutex<mpsc::Sender<Answered>>,
}

impl Asks {
    fn new(asks: Vec<Make>) -> (Arc<Self>, mpsc::Receiver<Answered>) {
        Self::gated(asks, None)
    }

    fn gated(asks: Vec<Make>, before: Option<Arc<Gate>>) -> (Arc<Self>, mpsc::Receiver<Answered>) {
        let (tx, rx) = mpsc::channel();
        let tool = Self {
            asks,
            before,
            told: Mutex::new(tx),
        };
        (Arc::new(tool), rx)
    }
}

impl Tool for Asks {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "asks".into(),
            description: "Asks the person.".into(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        panic!("the loop runs a call through run_asking")
    }

    fn run_asking(
        &self,
        _: &Map<String, Value>,
        _: &dyn Cancel,
        _: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        if let Some(gate) = &self.before {
            gate.wait();
        }
        let own = ask.action();
        let mut said = Vec::new();
        for make in &self.asks {
            let answered = ask.ask(make(&own));
            said.push(format!("{answered:?}"));
            let _sent = self.told.lock().unwrap().send(answered);
        }
        Output {
            content: vec![ContentPart::Text {
                text: said.join("\n"),
            }],
            ..Output::default()
        }
    }
}

fn confirm() -> Interaction {
    Interaction::Confirm {
        prompt: "Deploy?".into(),
    }
}

fn plain(interaction: Interaction) -> Make {
    Box::new(move |_| Asking {
        interaction: interaction.clone(),
        action_ids: Vec::new(),
        until: None,
        check: None,
        suspends: false,
    })
}

fn checked(interaction: Interaction, check: fn(&Answer) -> bool) -> Make {
    Box::new(move |_| Asking {
        interaction: interaction.clone(),
        action_ids: Vec::new(),
        until: None,
        check: Some(Box::new(check) as Check),
        suspends: false,
    })
}

/// A session whose first reply calls each of `calls`, and whose second
/// says "Done.", with `tools` registered.
fn session(calls: &[&str], tools: Vec<Arc<dyn Tool>>) -> Session {
    let calls: Vec<(&str, Value)> = calls.iter().map(|name| (*name, json!({}))).collect();
    Session::with_tools(
        vec![calls_reply("", &calls), Scripted::text("Done.")],
        None,
        tools,
    )
}

/// Runs one turn on its own thread.
fn spawn_turn(session: &mut Session) -> mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, Error>)> {
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn();
        let _sent = done.send((looped, outcome));
    });
    finished
}

/// Waits for the turn `spawn_turn` started, putting the loop back.
fn finish(
    session: &mut Session,
    finished: &mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, Error>)>,
) -> Result<Option<TurnOutcome>, Error> {
    let (looped, outcome) = finished
        .recv_timeout(DEADLINE)
        .expect("the turn ended in time");
    session.looped = Some(looped);
    outcome
}

/// An ack that reports its answer.
fn acked() -> (Ack, mpsc::Receiver<contract::inbox::Answer>) {
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    (ack, rx)
}

fn reply(
    request: &str,
    answer: ReplyAnswer,
) -> (Delivery, mpsc::Receiver<contract::inbox::Answer>) {
    let (ack, rx) = acked();
    let delivery = Delivery::Reply(
        Reply {
            request_id: RequestId(request.into()),
            answer,
        },
        ack,
    );
    (delivery, rx)
}

/// Sends `answer` to `request` and returns how the command was answered.
fn answer(session: &Session, request: &str, answer: ReplyAnswer) -> contract::inbox::Answer {
    let (delivery, rx) = reply(request, answer);
    session.inbox.send(delivery).unwrap();
    rx.recv_timeout(DEADLINE).expect("the reply is answered")
}

fn yes() -> ReplyAnswer {
    ReplyAnswer::Confirmed { confirmed: true }
}

fn request_id(line: &Envelope) -> String {
    line.payload["request_id"].as_str().unwrap().to_owned()
}

fn rejected(answered: &contract::inbox::Answer, code: ErrorCode) {
    match answered {
        Err(rejection) => assert_eq!(rejection.code, code, "{rejection:?}"),
        Ok(ok) => panic!("the command was accepted: {ok:?}"),
    }
}

fn told(answers: &mpsc::Receiver<Answered>) -> Answered {
    answers
        .recv_timeout(DEADLINE)
        .expect("the tool got its answer")
}

fn of_kind<'a>(lines: &'a [Envelope], kind: &str) -> Vec<&'a Envelope> {
    lines.iter().filter(|line| line.kind == kind).collect()
}

/// Asserts a resolved line is Fiber's decline.
fn declined_by_fiber(line: &Envelope) {
    assert_eq!(line.payload["by"], "fiber");
    assert_eq!(line.payload["declined"], true);
    assert_eq!(line.action_id, None);
}

/// The first reply's one tool call: the tail after [`support::OPENING`]
/// and [`support::STEP`] open the turn.
const FIRST_CALL: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "tool_call_arguments_delta",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
];

/// The first reply's two tool calls: the tail after [`support::OPENING`]
/// and [`support::STEP`] open the turn.
const TWO_CALLS: &[&str] = &[
    "assistant_message_started",
    "assistant_message_delta",
    "tool_call_arguments_delta",
    "tool_call_arguments_delta",
    "tool_call_requested",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
];

const DONE: &[&str] = &[
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

#[test]
fn a_confirm_is_answered_and_the_ack_follows_its_line() {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    let requested = tap.wait_for("interaction_requested");
    // The ack reads the log: its line must already be there.
    let mut watcher = session.log.watch();
    let (seen_tx, seen) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let mut resolved = false;
        while let Ok(Some(line)) = watcher.try_recv() {
            resolved |= line.kind == "interaction_resolved";
        }
        let _sent = seen_tx.send((answer, resolved));
    }));
    let reply = Reply {
        request_id: RequestId(request_id(&requested)),
        answer: yes(),
    };
    session.inbox.send(Delivery::Reply(reply, ack)).unwrap();
    let (answered, resolved_first) = seen.recv_timeout(DEADLINE).unwrap();
    assert_eq!(answered, Ok(None));
    assert!(resolved_first, "the reply is accepted after its line");
    assert_eq!(
        told(&answers),
        Answered::Reply(Answer::Confirmed { confirmed: true })
    );
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    let started = &of_kind(&lines, "tool_call_started")[0];
    let own = started.action_id.clone().unwrap();
    let requested = &of_kind(&lines, "interaction_requested")[0];
    assert_eq!(requested.payload["kind"], "confirm");
    assert_eq!(requested.payload["prompt"], "Deploy?");
    assert_eq!(requested.payload["action_ids"], json!([own.0]));
    assert_eq!(requested.action_id, None);
    assert_eq!(requested.turn_id, started.turn_id);
    assert!(requested.payload.get("extension").is_none());
    let resolved = &of_kind(&lines, "interaction_resolved")[0];
    assert_eq!(resolved.payload["by"], "person");
    assert_eq!(resolved.payload["confirmed"], true);
    assert_eq!(
        resolved.payload["request_id"],
        requested.payload["request_id"]
    );
    assert_eq!(resolved.turn_id, started.turn_id);
    assert_eq!(resolved.action_id, None);
    let completed = &of_kind(&lines, "tool_call_completed")[0];
    assert_eq!(
        completed.payload["content"][0]["text"],
        "Reply(Confirmed { confirmed: true })"
    );
}

#[test]
fn an_unfit_reply_is_rejected_and_the_interaction_stays_pending() {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    let labels = ReplyAnswer::Labels {
        labels: vec!["a".into()],
    };
    let answered = answer(&session, &request, labels);
    rejected(&answered, ErrorCode::InvalidArguments);
    assert_eq!(
        answered.unwrap_err().message,
        "That answer does not fit the pending request."
    );
    assert_eq!(answer(&session, &request, yes()), Ok(None));
    assert_eq!(
        told(&answers),
        Answered::Reply(Answer::Confirmed { confirmed: true })
    );
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    assert_eq!(
        of_kind(&lines, "interaction_resolved")[0].payload["by"],
        "person"
    );
}

#[test]
fn the_askers_check_rejects_a_reply_and_never_sees_a_decline() {
    let text = || Interaction::TextInput {
        prompt: "Port?".into(),
    };
    let (tool, answers) = Asks::new(vec![
        checked(text(), |answer| {
            *answer != Answer::Text { text: "x".into() }
        }),
        checked(text(), |_| false),
    ]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    let first = request_id(&tap.wait_for("interaction_requested"));
    let x = ReplyAnswer::Text { text: "x".into() };
    rejected(&answer(&session, &first, x), ErrorCode::InvalidArguments);
    let seven = ReplyAnswer::Text { text: "7".into() };
    assert_eq!(answer(&session, &first, seven), Ok(None));
    assert_eq!(
        told(&answers),
        Answered::Reply(Answer::Text { text: "7".into() })
    );
    let second = request_id(&tap.wait_for("interaction_requested"));
    let declined = ReplyAnswer::Declined { declined: True };
    assert_eq!(answer(&session, &second, declined), Ok(None));
    assert_eq!(
        told(&answers),
        Answered::Reply(Answer::Declined { declined: True })
    );
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    let resolved = of_kind(&lines, "interaction_resolved");
    assert_eq!(resolved[0].payload["text"], "7");
    assert_eq!(resolved[1].payload["by"], "person");
    assert_eq!(resolved[1].payload["declined"], true);
}

#[test]
fn a_reply_naming_another_request_or_a_resolved_one_is_stale() {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    rejected(&answer(&session, "r_other", yes()), ErrorCode::StaleRequest);
    assert_eq!(answer(&session, &request, yes()), Ok(None));
    told(&answers);
    rejected(&answer(&session, &request, yes()), ErrorCode::StaleRequest);
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
}

/// Runs a turn whose one call asks a confirm nobody can answer, and checks
/// Fiber's decline names a request never raised.
fn nobody_answers(mut session: Session, answers: &mpsc::Receiver<Answered>) {
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(told(answers), Answered::NoAnswer);
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    let resolved = &of_kind(&lines, "interaction_resolved")[0];
    declined_by_fiber(resolved);
    assert!(request_id(resolved).starts_with("r_"));
}

#[test]
fn a_session_nobody_can_answer_declines_at_once_without_a_request() {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    let session = session(&["asks"], vec![tool])
        .inbox_woken()
        .answerable(false);
    nobody_answers(session, &answers);
}

#[test]
fn a_loop_without_an_inbox_wake_declines_at_once_without_a_request() {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    nobody_answers(session(&["asks"], vec![tool]), &answers);
}

/// A tool whose call returns what its asker says of `answerable`, and
/// never asks.
struct Answerable;

impl Tool for Answerable {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "answerable".into(),
            description: "Says whether a person can answer.".into(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        panic!("the loop runs a call through run_asking")
    }

    fn run_asking(
        &self,
        _: &Map<String, Value>,
        _: &dyn Cancel,
        _: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: ask.answerable().to_string(),
            }],
            ..Output::default()
        }
    }
}

/// Runs a turn whose one call reports `answerable`, and returns what it
/// said; the log holds no interaction line.
fn answerable_in(mut session: Session) -> Value {
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &["tool_call_started", "tool_call_completed"],
            DONE,
        ],
    );
    of_kind(&lines, "tool_call_completed")[0].payload["content"][0]["text"].clone()
}

#[test]
fn a_call_is_answerable_with_an_inbox_wake_in_an_answerable_session() {
    let session = session(&["answerable"], vec![Arc::new(Answerable)]).inbox_woken();
    assert_eq!(answerable_in(session), "true");
}

#[test]
fn a_call_is_not_answerable_in_a_session_nobody_can_answer() {
    let session = session(&["answerable"], vec![Arc::new(Answerable)])
        .inbox_woken()
        .answerable(false);
    assert_eq!(answerable_in(session), "false");
}

#[test]
fn a_call_is_not_answerable_without_an_inbox_wake() {
    let session = session(&["answerable"], vec![Arc::new(Answerable)]);
    assert_eq!(answerable_in(session), "false");
}

#[test]
fn close_while_pending_declines_it_and_every_later_ask() {
    let (tool, answers) = Asks::new(vec![plain(confirm()), plain(confirm())]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let looped = session.looped.take().unwrap();
    let (done, ran) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(looped.run());
    });
    tap.wait_for("interaction_requested");
    let (ack, closed) = acked();
    session.inbox.send(Delivery::Close(ack)).unwrap();
    assert_eq!(closed.recv_timeout(DEADLINE).unwrap(), Ok(None));
    assert_eq!(told(&answers), Answered::NoAnswer);
    assert_eq!(told(&answers), Answered::NoAnswer);
    let lines = session.lines();
    assert!(
        ran.recv_timeout(DEADLINE)
            .expect("run returns after close")
            .is_ok()
    );
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    for resolved in of_kind(&lines, "interaction_resolved") {
        declined_by_fiber(resolved);
    }
}

/// Runs a turn whose call is pending when `stop` runs, and checks Fiber's
/// decline comes before the call's completion.
fn stopped_while_pending(stop: impl FnOnce(&Session)) -> (Vec<Envelope>, Option<TurnOutcome>) {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    tap.wait_for("interaction_requested");
    stop(&session);
    assert_eq!(told(&answers), Answered::NoAnswer);
    let outcome = finish(&mut session, &finished).unwrap();
    let lines = session.events_until("the call's completion and the turn's end", |line| {
        line.kind == "turn_completed"
    });
    (lines, outcome)
}

#[test]
fn a_cancel_while_pending_declines_it_before_the_call_completes() {
    let (lines, outcome) = stopped_while_pending(|session| {
        assert!(session.cancel.cancel());
    });
    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
                "turn_completed",
            ],
        ],
    );
    declined_by_fiber(of_kind(&lines, "interaction_resolved")[0]);
    assert_eq!(
        of_kind(&lines, "tool_call_completed")[0].payload["status"],
        "cancelled"
    );
}

#[test]
fn a_shutdown_while_pending_declines_it_before_the_call_completes() {
    let (lines, _) = stopped_while_pending(|session| session.cancel.shutdown(143));
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
                "turn_completed",
            ],
        ],
    );
    declined_by_fiber(of_kind(&lines, "interaction_resolved")[0]);
    assert_eq!(
        of_kind(&lines, "tool_call_completed")[0].payload["status"],
        "cancelled"
    );
}

#[test]
fn the_inbox_closing_while_pending_declines_it() {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    tap.wait_for("interaction_requested");
    // Every sender goes: the session's own and the one the wake reaches.
    session.inbox = mpsc::channel().0;
    session.woken = None;
    assert_eq!(told(&answers), Answered::NoAnswer);
    declined_by_fiber(&tap.wait_for("interaction_resolved"));
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
}

/// Runs a turn whose call asks with `until` 10 s on, at `at`, advances the clock to
/// 1 ms before it and checks the request still pends, then to it and
/// checks Fiber declines it.
fn until_passes(mut session: Session, answers: &mpsc::Receiver<Answered>, at: Instant) {
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    tap.wait_for("interaction_requested");
    assert!(clock.await_parked(at, DEADLINE), "the step waits until it");
    let mark = clock.advance_marked(Duration::from_millis(9_999));
    assert!(
        clock.await_parked_since(&mark, Some(at), DEADLINE),
        "the step waits again"
    );
    assert!(
        tap.pending()
            .iter()
            .all(|line| line.kind != "interaction_resolved"),
        "1 ms before it, the request still pends"
    );
    assert!(answers.try_recv().is_err(), "the call still waits");
    clock.advance(Duration::from_millis(1));
    assert_eq!(told(answers), Answered::NoAnswer);
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    declined_by_fiber(of_kind(&lines, "interaction_resolved")[0]);
}

/// An ask whose `until` is set once the session's clock is known.
fn until_later() -> (Make, Arc<Mutex<Option<Instant>>>) {
    let at = Arc::new(Mutex::new(None::<Instant>));
    let read = Arc::clone(&at);
    let make: Make = Box::new(move |_| Asking {
        interaction: confirm(),
        action_ids: Vec::new(),
        until: *read.lock().unwrap(),
        check: None,
        suspends: false,
    });
    (make, at)
}

#[test]
fn until_passing_declines_the_request() {
    let (make, at_slot) = until_later();
    let (tool, answers) = Asks::new(vec![make]);
    let session = session(&["asks"], vec![tool]).inbox_woken();
    let at = session.clock.now() + Duration::from_secs(10);
    *at_slot.lock().unwrap() = Some(at);
    until_passes(session, &answers, at);
}

/// Jobs a test turns on and off: one job runs while `running` is set.
#[derive(Default)]
struct Toggle {
    running: AtomicBool,
}

impl Jobs for Toggle {
    fn open(&self, _opening: Opening) -> Result<Opened, OpenError> {
        Err(OpenError::Io {
            path: PathBuf::from("jobs"),
            source: std::io::Error::other("no jobs here"),
        })
    }

    fn stop(&self, _job_id: &JobId) -> bool {
        false
    }

    fn stop_delegates(&self) -> usize {
        0
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _call: Foreground) {}

    fn running(&self) -> Vec<JobId> {
        if self.running.load(Ordering::SeqCst) {
            vec![JobId("j_1".into())]
        } else {
            Vec::new()
        }
    }

    fn deliver_to(&self, _inbox: mpsc::Sender<Delivery>) {}
}

#[test]
fn until_passing_declines_the_request_while_a_job_runs() {
    let (make, at_slot) = until_later();
    let (tool, answers) = Asks::new(vec![make]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let jobs = Arc::new(Toggle::default());
    jobs.running.store(true, Ordering::SeqCst);
    let looped = session.looped.take().unwrap();
    session.looped = Some(looped.jobs(jobs as Arc<dyn Jobs>));
    let at = session.clock.now() + Duration::from_secs(10);
    *at_slot.lock().unwrap() = Some(at);
    until_passes(session, &answers, at);
}

#[test]
fn action_ids_are_written_as_given_or_as_the_call_alone() {
    // `until` already passed, so Fiber declines each at once after its
    // request line.
    let past = Arc::new(Mutex::new(None::<Instant>));
    let (first, second) = (Arc::clone(&past), Arc::clone(&past));
    let (tool, answers) = Asks::new(vec![
        Box::new(move |own: &ActionId| Asking {
            interaction: confirm(),
            action_ids: vec![ActionId("a_other".into()), own.clone()],
            until: *first.lock().unwrap(),
            check: None,
            suspends: false,
        }),
        Box::new(move |_| Asking {
            interaction: confirm(),
            action_ids: Vec::new(),
            until: *second.lock().unwrap(),
            check: None,
            suspends: false,
        }),
    ]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    *past.lock().unwrap() = Some(session.clock.now());
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(told(&answers), Answered::NoAnswer);
    assert_eq!(told(&answers), Answered::NoAnswer);
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    let own = of_kind(&lines, "tool_call_started")[0]
        .action_id
        .clone()
        .unwrap();
    let requested = of_kind(&lines, "interaction_requested");
    assert_eq!(
        requested[0].payload["action_ids"],
        json!(["a_other", own.0])
    );
    assert_eq!(requested[1].payload["action_ids"], json!([own.0]));
}

#[test]
fn two_calls_ask_at_once_and_each_gets_its_own_answer() {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    let mut session = session(&["asks", "asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    let one = tap.wait_for("interaction_requested");
    let two = tap.wait_for("interaction_requested");
    // Reverse order: the later request first.
    let no = ReplyAnswer::Confirmed { confirmed: false };
    assert_eq!(answer(&session, &request_id(&two), no), Ok(None));
    assert_eq!(answer(&session, &request_id(&one), yes()), Ok(None));
    told(&answers);
    told(&answers);
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    // The two calls race to raise their asks, while completions are
    // written in request order: when the call raised second answers
    // first, its completion lands before the other call's resolution.
    // Either interleaving is correct, so the one kinds assertion sorts
    // that window before comparing.
    let mut ordered = kinds(&lines);
    let prefix = OPENING.len() + STEP.len() + TWO_CALLS.len();
    ordered[prefix + 4..prefix + 8].sort_unstable();
    assert_eq!(
        ordered,
        [
            OPENING,
            STEP,
            TWO_CALLS,
            &[
                "tool_call_started",
                "tool_call_started",
                "interaction_requested",
                "interaction_requested",
                "interaction_resolved",
                "interaction_resolved",
                "tool_call_completed",
                "tool_call_completed",
            ],
            DONE,
        ]
        .concat()
    );
    // Each call's own resolution still precedes its own completion.
    for asked in of_kind(&lines, "interaction_requested") {
        let request = asked.payload["request_id"].as_str().unwrap();
        let action = asked.payload["action_ids"][0].as_str().unwrap();
        let resolved = lines
            .iter()
            .position(|line| {
                line.kind == "interaction_resolved"
                    && line.payload.get("request_id").and_then(Value::as_str) == Some(request)
            })
            .unwrap();
        let completed = lines
            .iter()
            .position(|line| {
                line.kind == "tool_call_completed"
                    && line.action_id.as_ref().is_some_and(|id| id.0 == action)
            })
            .unwrap();
        assert!(
            resolved < completed,
            "{request} resolves before {action} completes"
        );
    }
    let said = |line: &Envelope| -> String {
        let asker = line.payload["action_ids"][0].as_str().unwrap().to_owned();
        let completed = of_kind(&lines, "tool_call_completed")
            .into_iter()
            .find(|done| done.action_id.as_ref().map(|id| id.0.as_str()) == Some(&asker))
            .unwrap();
        completed.payload["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(said(&one), "Reply(Confirmed { confirmed: true })");
    assert_eq!(said(&two), "Reply(Confirmed { confirmed: false })");
}

fn delta(text: &str) -> Script {
    Script::Emit(Box::new(Event::ToolCallDelta(Progress {
        text: Some(text.into()),
        details: None,
    })))
}

#[test]
fn another_calls_deltas_keep_flowing_while_one_is_pending() {
    let (asks, answers) = Asks::new(vec![plain(confirm())]);
    let gates: Vec<Arc<Gate>> = (0..3).map(|_| Arc::new(Gate::default())).collect();
    let mut streams = TestTool::reads("streams", "Streamed.");
    streams.script = vec![
        Script::Wait(Arc::clone(&gates[0])),
        delta("one"),
        Script::Wait(Arc::clone(&gates[1])),
        delta("two"),
        Script::Wait(Arc::clone(&gates[2])),
    ];
    let streams = Arc::new(streams);
    let mut session = Session::with_tools(
        vec![
            calls_reply(
                "",
                &[("asks", json!({})), ("streams", json!({"city": "Paris"}))],
            ),
            Scripted::text("Done."),
        ],
        None,
        vec![asks, Arc::clone(&streams) as Arc<dyn Tool>],
    )
    .inbox_woken();
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    gates[0].open();
    tap.wait_for_delta("one");
    gates[1].open();
    // The second change is held for the 100 ms interval, then written.
    let due = clock.now() + Duration::from_millis(100);
    assert!(clock.await_parked(due, DEADLINE), "the held change waits");
    clock.advance(Duration::from_millis(100));
    tap.wait_for_delta("two");
    gates[2].open();
    assert_eq!(answer(&session, &request, yes()), Ok(None));
    told(&answers);
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    for gate in &gates {
        gate.check("streams");
    }
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            TWO_CALLS,
            &[
                "tool_call_started",
                "tool_call_started",
                "interaction_requested",
                "tool_call_delta",
                "tool_call_delta",
                "interaction_resolved",
                "tool_call_completed",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    let ids: Vec<_> = of_kind(&lines, "tool_call_started")
        .iter()
        .map(|line| line.action_id.clone())
        .collect();
    let completed: Vec<_> = of_kind(&lines, "tool_call_completed")
        .iter()
        .map(|line| line.action_id.clone())
        .collect();
    assert_eq!(completed, ids, "completions in request order");
}

fn prepare_to(provider: Arc<ScriptedProvider>) -> r#loop::Prepare {
    Arc::new(
        move |_args: &contract::commands::ModelArgs, _label: Option<&str>, chosen| {
            Ok(Prepared {
                provider: Arc::clone(&provider) as Arc<dyn Provider>,
                model: Model {
                    reference: "fake/model-2".into(),
                    cost: None,
                    subscription: false,
                },
                thinking: chosen,
                chosen,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: HandoffSettings::default(),
                reviewer: Err(contract::shapes::Failure {
                    code: ErrorCode::NoModel,
                    message: r#loop::NO_MODEL_MESSAGE.into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                web_search: r#loop::Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
            })
        },
    )
}

#[test]
fn other_deliveries_are_admitted_while_a_call_waits() {
    let (tool, answers) = Asks::new(vec![plain(confirm())]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let switched = Arc::new(ScriptedProvider::new(vec![Scripted::text("Second.")]));
    let looped = session.looped.take().unwrap().switcher(
        prepare_to(Arc::clone(&switched)),
        Switchable { chosen: None },
    );
    session.looped = Some(looped);
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    let send = |delivery: Delivery| session.inbox.send(delivery).unwrap();
    let (ack, busy) = acked();
    send(Delivery::Prompt(message("busy"), ack));
    rejected(&busy.recv_timeout(DEADLINE).unwrap(), ErrorCode::Busy);
    let (ack, kept) = acked();
    send(Delivery::Steer(message("keep"), ack));
    assert_eq!(kept.recv_timeout(DEADLINE).unwrap(), Ok(None));
    let (ack, queued) = acked();
    send(Delivery::Steer(message("drop"), ack));
    assert_eq!(queued.recv_timeout(DEADLINE).unwrap(), Ok(None));
    let (ack, dropped) = acked();
    send(Delivery::SteerDrop(CommandId("c_drop".into()), ack));
    assert_eq!(dropped.recv_timeout(DEADLINE).unwrap(), Ok(None));
    send(Delivery::Interaction(InteractionRequested {
        request_id: RequestId("r_ext".into()),
        interaction: confirm(),
        action_ids: None,
        extension: Some("ext".into()),
        resumes: false,
    }));
    let extension = tap.wait_for("interaction_requested");
    assert_eq!(extension.payload["extension"], "ext");
    let (ack, switch) = acked();
    send(Delivery::Model(
        contract::commands::ModelArgs {
            model: "fake/model-2".into(),
            thinking: None,
        },
        ack,
    ));
    assert_eq!(switch.recv_timeout(DEADLINE).unwrap(), Ok(None));
    assert_eq!(answer(&session, &request, yes()), Ok(None));
    told(&answers);
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_requested",
                "steering_queue",
                "steering_queue",
                "steering_queue",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
                "step_started",
                "steering_applied",
                "steering_queue",
            ],
            &DONE[1..],
        ],
    );
    let applied = &of_kind(&lines, "steering_applied")[0];
    let text = serde_json::to_string(&applied.payload).unwrap();
    assert!(text.contains("keep") && !text.contains("drop"), "{text}");
    // The switch applies at the next turn boundary.
    session.inbox.send(steer("next")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let next = session.lines();
    assert_eq!(next[0].kind, "model_changed", "{:?}", kinds(&next));
    assert_eq!(switched.requests().len(), 1);
}

#[test]
fn an_ask_after_the_cancel_is_declined_at_once() {
    let gate = Arc::new(Gate::default());
    let (tool, answers) = Asks::gated(vec![plain(confirm())], Some(Arc::clone(&gate)));
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    tap.wait_for("tool_call_started");
    assert!(session.cancel.cancel());
    gate.open();
    assert_eq!(told(&answers), Answered::NoAnswer);
    assert_eq!(
        finish(&mut session, &finished).unwrap(),
        Some(TurnOutcome::Interrupted)
    );
    gate.check("before");
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            STEP,
            FIRST_CALL,
            &[
                "tool_call_started",
                "interaction_resolved",
                "tool_call_completed",
                "turn_completed",
            ],
        ],
    );
    declined_by_fiber(of_kind(&lines, "interaction_resolved")[0]);
}

/// Hooks that give the `streams` call an artifact, so its completion fails
/// to write once `artifacts/` is a file.
struct ArtifactHook;

impl Hooks for ArtifactHook {
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer {
        let outcome = if call.tool == "streams" {
            AfterToolOutcome::Changed {
                content: None,
                details: None,
                artifact: Some("full output".into()),
            }
        } else {
            AfterToolOutcome::Unchanged
        };
        AfterToolAnswer {
            outcome,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        drop(inbox);
    }
}

/// A session whose first reply calls `streams`, which waits on `first`,
/// then `asks`; `artifacts/` can be broken to fail call 1's completion.
fn failing(asks: Arc<Asks>, first: &Arc<Gate>) -> Session {
    let mut streams = TestTool::reads("streams", "Streamed.");
    streams.script = vec![Script::Wait(Arc::clone(first))];
    let mut session = Session::with_tools(
        vec![
            calls_reply(
                "",
                &[("streams", json!({"city": "Paris"})), ("asks", json!({}))],
            ),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::new(streams) as Arc<dyn Tool>, asks],
    )
    .inbox_woken();
    let looped = session.looped.take().unwrap();
    session.looped = Some(looped.hooks(Arc::new(ArtifactHook)));
    session
}

fn break_artifacts(session: &Session) {
    let artifacts = session.dir.join("artifacts");
    std::fs::remove_dir_all(&artifacts).unwrap();
    std::fs::write(&artifacts, "not a directory").unwrap();
}

#[test]
fn a_log_failure_while_a_call_is_pending_releases_it() {
    let first = Arc::new(Gate::default());
    let (asks, answers) = Asks::new(vec![plain(confirm()), plain(confirm())]);
    let mut session = failing(asks, &first);
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    tap.wait_for("interaction_requested");
    break_artifacts(&session);
    first.open();
    assert_eq!(told(&answers), Answered::NoAnswer);
    assert_eq!(told(&answers), Answered::NoAnswer);
    let ended = finish(&mut session, &finished);
    assert!(matches!(ended, Err(Error::Log(_))), "{ended:?}");
    first.check("first");
    assert!(
        tap.pending()
            .iter()
            .all(|line| line.kind != "interaction_resolved"),
        "nothing is written after the failure"
    );
}

#[test]
fn a_log_failure_before_a_calls_first_ask_releases_it() {
    let first = Arc::new(Gate::default());
    let second = Arc::new(Gate::default());
    let (asks, answers) = Asks::gated(vec![plain(confirm())], Some(Arc::clone(&second)));
    let mut session = failing(asks, &first);
    let tap = Tap::new(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = spawn_turn(&mut session);
    tap.wait_for("tool_call_started");
    tap.wait_for("tool_call_started");
    break_artifacts(&session);
    first.open();
    second.open();
    assert_eq!(told(&answers), Answered::NoAnswer);
    let ended = finish(&mut session, &finished);
    assert!(matches!(ended, Err(Error::Log(_))), "{ended:?}");
    first.check("first");
    second.check("second");
    assert!(
        tap.pending()
            .iter()
            .all(|line| line.kind != "interaction_resolved"),
        "nothing is written after the failure"
    );
}

#[test]
fn an_ask_that_suspends_is_logged_resumes_and_any_other_without_the_key() {
    // `until` already passed, so Fiber declines each at once after its
    // request line.
    let past = Arc::new(Mutex::new(None::<Instant>));
    let (first, second) = (Arc::clone(&past), Arc::clone(&past));
    let (tool, answers) = Asks::new(vec![
        Box::new(move |_| Asking {
            interaction: confirm(),
            action_ids: Vec::new(),
            until: *first.lock().unwrap(),
            check: None,
            suspends: true,
        }),
        Box::new(move |_| Asking {
            interaction: confirm(),
            action_ids: Vec::new(),
            until: *second.lock().unwrap(),
            check: None,
            suspends: false,
        }),
    ]);
    let mut session = session(&["asks"], vec![tool]).inbox_woken();
    *past.lock().unwrap() = Some(session.clock.now());
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(told(&answers), Answered::NoAnswer);
    assert_eq!(told(&answers), Answered::NoAnswer);
    let lines = session.lines();
    let requested = of_kind(&lines, "interaction_requested");
    assert_eq!(requested.len(), 2);
    assert_eq!(requested[0].payload["resumes"], true);
    assert!(
        requested[1].payload.get("resumes").is_none(),
        "{:?}",
        requested[1].payload
    );
}
