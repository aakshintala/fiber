//! A cost that settles late (`docs/model-routing.md`, "Cost"): a call
//! recorded without the vendor's own figure is looked up once, 30 seconds
//! later on the session's clock, and a returned cost is a second
//! `usage_recorded` with the same `generation_id` (`docs/events.md`, "Usage
//! and notices").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::commands::{Reply as Answer, ReplyAnswer};
use contract::events::{CacheLifetime, Decision, TurnOutcome};
use contract::inbox::{Ack, Delivery};
use contract::provider::{CostLookup, ModelCall, ModelRequest, Provider};
use contract::shapes::{Effect, Failure};
use contract::{Envelope, ErrorCode, GenerationId, RequestId};
use fakes::clock::FakeClock;
use fakes::{Scripted, ScriptedProvider, call_usage};
use r#loop::{BlockLimits, Model, Reviewer, TurnCancel};
use serde_json::json;

use support::{
    DEADLINE, REVIEWER_MODEL, Session, TestTool, allow, calls_reply, delivery, kinds, on_request,
    reply_delivery,
};

/// How long after a call's first record its lookup runs.
const AFTER: Duration = Duration::from_secs(30);

/// A lookup returning `cost` for every generation, which records each one
/// it is asked for and says when the last holder lets go of it.
struct Lookup {
    cost: Option<f64>,
    calls: Arc<Mutex<Vec<String>>>,
    dropped: Mutex<Sender<()>>,
}

impl CostLookup for Lookup {
    fn cost(&self, generation_id: &GenerationId) -> Option<f64> {
        self.calls.lock().unwrap().push(generation_id.0.clone());
        self.cost
    }
}

impl Drop for Lookup {
    fn drop(&mut self) {
        match self.dropped.lock().unwrap().send(()) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// What a test keeps of its lookup.
struct Seen {
    calls: Arc<Mutex<Vec<String>>>,
    dropped: Receiver<()>,
}

impl Seen {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn lookup(cost: Option<f64>) -> (Arc<Lookup>, Seen) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (dropped_tx, dropped) = mpsc::channel();
    let lookup = Arc::new(Lookup {
        cost,
        calls: Arc::clone(&calls),
        dropped: Mutex::new(dropped_tx),
    });
    (lookup, Seen { calls, dropped })
}

/// Runs on the loop's thread as the `n`-th call (1-based) is made.
type Hook = Box<dyn Fn(usize) + Send + Sync>;

/// A provider whose calls are another's, with `lookup` as its cost lookup.
struct Looked {
    inner: Arc<dyn Provider>,
    lookup: Arc<Lookup>,
    hook: Hook,
    calls: AtomicUsize,
}

impl Provider for Looked {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let call = self.inner.call(request);
        (self.hook)(n);
        call
    }

    fn cost_lookup(&self) -> Option<Arc<dyn CostLookup>> {
        Some(Arc::clone(&self.lookup) as Arc<dyn CostLookup>)
    }
}

/// A session reaching `script` through a provider whose lookup returns
/// `cost`. `hook` is made from the session's clock.
fn looked(
    script: Vec<Scripted>,
    lifetime: CacheLifetime,
    cost: Option<f64>,
    hook: impl FnOnce(Arc<FakeClock>) -> Hook,
) -> (Session, Seen) {
    let (lookup, seen) = lookup(cost);
    let session = Session::wrapped(script, lifetime, |scripted, clock| {
        Arc::new(Looked {
            inner: scripted,
            lookup,
            hook: hook(clock),
            calls: AtomicUsize::new(0),
        })
    });
    (session, seen)
}

fn no_hook(_: Arc<FakeClock>) -> Hook {
    Box::new(|_| {})
}

fn failed_after(generation: &str) -> Scripted {
    Scripted::failed_after(
        Failure {
            code: ErrorCode::InvalidRequest,
            message: "The call failed.".into(),
            retry_after_ms: None,
            provider: None,
        },
        call_usage(generation),
    )
}

/// A reply of `text` from `generation`, carrying the vendor's own `cost`.
fn priced(text: &str, generation: &str, cost: f64) -> Scripted {
    let mut scripted = unpriced(text, generation);
    if let Ok(reply) = &mut scripted.end {
        reply.cost = Some(cost);
    }
    scripted
}

/// A reply of `text` from `generation`, without the vendor's figure.
fn unpriced(text: &str, generation: &str) -> Scripted {
    let mut scripted = Scripted::text(text);
    if let Ok(reply) = &mut scripted.end {
        reply.generation_id = Some(GenerationId(generation.into()));
    }
    scripted
}

/// Waits for the lookup worker to park at `due`, moves the clock there, and
/// waits for the worker to park again with nothing queued: what it settled
/// is pushed.
fn settle(clock: &FakeClock, due: Instant) {
    let mark = clock
        .mark_parked(due, DEADLINE)
        .expect("the lookup worker parks at the due");
    clock.advance(due.saturating_duration_since(clock.now()));
    assert!(
        clock.await_parked_since(&mark, None, DEADLINE),
        "the lookup worker parks with nothing queued once it has pushed"
    );
}

/// Closes the inbox and runs the loop until it returns.
fn run_to_end(session: &mut Session) {
    drop(std::mem::replace(&mut session.inbox, mpsc::channel().0));
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run().is_ok()).unwrap());
    assert!(finished.recv_timeout(DEADLINE).expect("run ended in time"));
}

fn usage_lines(lines: &[Envelope]) -> Vec<&Envelope> {
    lines
        .iter()
        .filter(|line| line.kind == "usage_recorded")
        .collect()
}

/// A failed call's first turn: its usage, then the failure.
const FAILED_FIRST_KINDS: [&str; 9] = [
    "session_started",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

/// A cancelled call's first turn: its usage, then the turn ends.
const CANCELLED_FIRST_KINDS: [&str; 8] = [
    "session_started",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "usage_recorded",
    "turn_completed",
];

/// A completed reply's first turn.
const COMPLETED_FIRST_KINDS: [&str; 12] = [
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
];

/// A second turn opening with the late record of the first.
const LATE_NEXT_KINDS: [&str; 10] = [
    "usage_recorded",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

/// The first turn's one `usage_recorded`, and its lines.
fn first_turn(session: &mut Session) -> (Envelope, Vec<Envelope>) {
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let usage = usage_lines(&lines);
    assert_eq!(usage.len(), 1, "one record at once");
    (usage[0].clone(), lines)
}

/// Runs a second turn and returns its lines.
fn next_turn(session: &mut Session) -> Vec<Envelope> {
    session.inbox.send(delivery("again")).unwrap();
    session.turn();
    session.lines()
}

/// Asserts the second turn's lines open with the late record of `first`,
/// at `cost`, before `turn_started`.
fn assert_late_record_first(lines: &[Envelope], first: &Envelope, cost: f64) {
    assert_eq!(kinds(lines), LATE_NEXT_KINDS);
    let late = &lines[0];
    assert_eq!(late.turn_id, first.turn_id);
    assert_eq!(late.action_id, first.action_id);
    let mut want = first.payload.clone();
    want["cost"] = json!(cost);
    assert_eq!(late.payload, want);
}

#[test]
fn a_failed_call_s_cost_settles_30_seconds_later_and_is_counted_once() {
    let (mut session, seen) = looked(
        vec![failed_after("gen_failed"), priced("Again.", "gen_2", 0.01)],
        CacheLifetime::OneHour,
        Some(0.5),
        no_hook,
    );
    let start = session.clock.now();
    let (first, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), FAILED_FIRST_KINDS);
    assert!(first.payload["cost"].is_null());
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_failed"]);
    let lines = next_turn(&mut session);
    assert_late_record_first(&lines, &first, 0.5);

    run_to_end(&mut session);
    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), false, None).unwrap();
    let all = log::read(&session.dir).unwrap();
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "usage_recorded",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let exited = all.last().unwrap();
    assert_eq!(exited.kind, "fiber_exited");
    // Two calls of 10 input tokens each: the correction replaces its call.
    assert_eq!(exited.payload["usage"]["tokens"]["input"], 20);
    assert_eq!(exited.payload["usage"]["cost"].as_f64(), Some(0.51));
}

#[test]
fn a_cancelled_call_s_cost_settles_30_seconds_later() {
    let cancel: Arc<OnceLock<Arc<TurnCancel>>> = Arc::new(OnceLock::new());
    let (mut session, seen) = looked(
        vec![
            Scripted::cancelled_after(call_usage("gen_cancelled")),
            priced("Again.", "gen_2", 0.01),
        ],
        CacheLifetime::OneHour,
        Some(0.25),
        {
            let cancel = Arc::clone(&cancel);
            move |_| {
                Box::new(move |n| {
                    if n == 1 {
                        cancel.get().unwrap().cancel();
                    }
                })
            }
        },
    );
    cancel.set(Arc::clone(&session.cancel)).ok().unwrap();
    let start = session.clock.now();
    let (first, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), CANCELLED_FIRST_KINDS);
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_cancelled"]);
    let lines = next_turn(&mut session);
    assert_late_record_first(&lines, &first, 0.25);
}

#[test]
fn a_lookup_returning_nothing_leaves_the_first_record_alone() {
    let (mut session, seen) = looked(
        vec![failed_after("gen_failed"), priced("Again.", "gen_2", 0.01)],
        CacheLifetime::OneHour,
        None,
        no_hook,
    );
    let start = session.clock.now();
    let (first, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), FAILED_FIRST_KINDS);
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_failed"]);
    let lines = next_turn(&mut session);
    assert_eq!(
        kinds(&lines),
        [
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
    );
    run_to_end(&mut session);
    let all = log::read(&session.dir).unwrap();
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let failed: Vec<_> = usage_lines(&all)
        .into_iter()
        .filter(|l| l.payload["generation_id"] == "gen_failed")
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].payload, first.payload);
}

#[test]
fn a_session_that_ends_before_30_seconds_writes_no_second_record() {
    let (mut session, seen) = looked(
        vec![failed_after("gen_failed")],
        CacheLifetime::OneHour,
        Some(0.5),
        no_hook,
    );
    let start = session.clock.now();
    let (_, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), FAILED_FIRST_KINDS);
    assert!(session.clock.await_parked(start + AFTER, DEADLINE));
    run_to_end(&mut session);
    session.clock.advance(AFTER);
    // The lookup goes once the loop and the stopped worker have let go.
    seen.dropped
        .recv_timeout(DEADLINE)
        .expect("the worker let go of the lookup");
    assert!(seen.calls().is_empty());
    let all = log::read(&session.dir).unwrap();
    assert_eq!(kinds(&all), FAILED_FIRST_KINDS);
    assert_eq!(usage_lines(&all).len(), 1);
}

#[test]
fn a_cost_settled_while_idle_is_written_when_the_session_ends() {
    let (mut session, seen) = looked(
        vec![failed_after("gen_failed")],
        CacheLifetime::OneHour,
        Some(0.5),
        no_hook,
    );
    let start = session.clock.now();
    let (first, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), FAILED_FIRST_KINDS);
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_failed"]);
    run_to_end(&mut session);
    let all = log::read(&session.dir).unwrap();
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "usage_recorded",
        ]
    );
    let usage = usage_lines(&all);
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[1].turn_id, first.turn_id);
    assert_eq!(usage[1].action_id, first.action_id);
    assert_eq!(usage[1].payload["cost"].as_f64(), Some(0.5));
    assert_eq!(all.last().unwrap().kind, "usage_recorded");
}

#[test]
fn a_provider_without_a_lookup_writes_one_record_and_starts_no_worker() {
    let mut session = Session::new(vec![failed_after("gen_failed")], None);
    let (_, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), FAILED_FIRST_KINDS);
    assert!(
        session.clock.parked().is_empty(),
        "nothing waits on the clock"
    );
    run_to_end(&mut session);
    let all = log::read(&session.dir).unwrap();
    assert_eq!(kinds(&all), FAILED_FIRST_KINDS);
    assert_eq!(usage_lines(&all).len(), 1);
}

#[test]
fn only_a_call_without_the_vendor_s_figure_is_looked_up() {
    let (mut session, seen) = looked(
        vec![priced("First.", "gen_1", 0.01), failed_after("gen_2")],
        CacheLifetime::OneHour,
        Some(0.5),
        no_hook,
    );
    let start = session.clock.now();
    let (_, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), COMPLETED_FIRST_KINDS);
    let lines = next_turn(&mut session);
    assert_eq!(
        kinds(&lines),
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(usage_lines(&lines).len(), 1);
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_2"]);
}

#[test]
fn a_call_whose_generation_was_never_named_is_never_looked_up() {
    let (mut session, seen) = looked(
        vec![
            Scripted::failed(Failure {
                code: ErrorCode::InvalidRequest,
                message: "The call failed.".into(),
                retry_after_ms: None,
                provider: None,
            }),
            failed_after("gen_2"),
        ],
        CacheLifetime::OneHour,
        Some(0.5),
        no_hook,
    );
    let (first, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), FAILED_FIRST_KINDS);
    let minted = first.payload["generation_id"].as_str().unwrap().to_owned();
    assert!(minted.starts_with("fiber-"), "{minted}");
    assert!(first.payload["cost"].is_null());
    assert!(
        session.clock.parked().is_empty(),
        "no lookup waits, so no worker started"
    );
    let start = session.clock.now();
    let lines = next_turn(&mut session);
    assert_eq!(usage_lines(&lines).len(), 1);
    assert_eq!(usage_lines(&lines)[0].payload["generation_id"], "gen_2");
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_2"]);
}

#[test]
fn a_completed_reply_the_provider_never_named_is_never_looked_up() {
    let mut unnamed = Scripted::text("First.");
    if let Ok(reply) = &mut unnamed.end {
        reply.generation_id = None;
    }
    let (mut session, seen) = looked(
        vec![unnamed, failed_after("gen_2")],
        CacheLifetime::OneHour,
        Some(0.5),
        no_hook,
    );
    let (first, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), COMPLETED_FIRST_KINDS);
    let minted = first.payload["generation_id"].as_str().unwrap().to_owned();
    assert!(minted.starts_with("fiber-"), "{minted}");
    assert!(
        session.clock.parked().is_empty(),
        "no lookup waits, so no worker started"
    );
    let start = session.clock.now();
    next_turn(&mut session);
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_2"]);
}

#[test]
fn a_completed_reply_without_the_vendor_s_figure_is_looked_up() {
    let (mut session, seen) = looked(
        vec![unpriced("First.", "gen_1"), priced("Again.", "gen_2", 0.01)],
        CacheLifetime::OneHour,
        Some(0.5),
        no_hook,
    );
    let start = session.clock.now();
    let (first, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), COMPLETED_FIRST_KINDS);
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_1"]);
    let lines = next_turn(&mut session);
    assert_late_record_first(&lines, &first, 0.5);
}

#[test]
fn a_reviewer_call_s_cost_settles_with_no_action() {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = None;
    tool.prefix = None;
    let tool = Arc::new(tool);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", json!({"city": "Paris"}))]),
            priced("Done.", "gen_done", 0.01),
            priced("Again.", "gen_again", 0.01),
        ],
        None,
        vec![tool as Arc<dyn contract::tool::Tool>],
    );
    let (lookup, seen) = lookup(Some(0.125));
    let reviewer = Arc::new(Looked {
        inner: Arc::new(ScriptedProvider::new([Scripted::failed_after(
            Failure {
                code: ErrorCode::Timeout,
                message: "the reviewer timed out".into(),
                retry_after_ms: None,
                provider: None,
            },
            call_usage("gen_review"),
        )])),
        lookup,
        hook: Box::new(|_| {}),
        calls: AtomicUsize::new(0),
    });
    let looped = session.looped.take().unwrap().reviewer(
        Ok(Reviewer {
            provider: reviewer,
            model: Model {
                reference: REVIEWER_MODEL.into(),
                cost: None,
                subscription: false,
            },
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            thinking_levels: Vec::new(),
        }),
        BlockLimits::default(),
    );
    session.looped = Some(looped);
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply_delivery(id, allow())).unwrap()
    });
    let start = session.clock.now();
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    answered.join().unwrap();
    let review = usage_lines(&lines)
        .into_iter()
        .find(|l| l.payload["generation_id"] == "gen_review")
        .unwrap()
        .clone();
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_review"]);
    let lines = next_turn(&mut session);
    assert!(review.action_id.is_none());
    assert_late_record_first(&lines, &review, 0.125);
}

/// Moves the clock to `to` and wakes the loop with a reply naming nothing,
/// as a real clock's timeout would, returning once the loop took it.
fn advance_and_wake(session: &Session, to: Instant) {
    session
        .clock
        .advance(to.saturating_duration_since(session.clock.now()));
    let (done, taken) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Reply(
            Answer {
                request_id: RequestId("r_absent".into()),
                answer: ReplyAnswer::Approval {
                    decision: Decision::Deny,
                    feedback: None,
                    remember: None,
                },
            },
            Ack(Box::new(move |_| {
                done.send(()).unwrap();
            })),
        ))
        .unwrap();
    taken
        .recv_timeout(DEADLINE)
        .expect("the loop took the wake");
}

#[test]
fn a_warming_refresh_s_cost_settles_in_no_turn_and_no_action() {
    const MINUTE: Duration = Duration::from_secs(60);
    let (mut session, seen) = looked(
        vec![priced("ok.", "gen_turn", 0.01), unpriced("", "gen_warm")],
        CacheLifetime::FiveMinutes,
        Some(0.75),
        no_hook,
    );
    let looped = session.looped.take().unwrap();
    session.looped = Some(looped.idle_exit(Some(MINUTE)).warm(Some(1)));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run().is_ok()).unwrap());

    let refresh = start + Duration::from_secs(270);
    assert!(
        session.clock.await_parked(refresh, DEADLINE),
        "a refresh is due"
    );
    advance_and_wake(&session, refresh);
    let due = refresh + AFTER;
    let mark = session
        .clock
        .mark_parked(due, DEADLINE)
        .expect("the lookup worker parks at the due");
    advance_and_wake(&session, due);
    assert!(
        session.clock.await_parked_since(&mark, None, DEADLINE),
        "the lookup worker settled"
    );
    assert_eq!(seen.calls(), ["gen_warm"]);
    let exit = start + Duration::from_secs(300) + MINUTE;
    assert!(
        session.clock.await_parked(exit, DEADLINE),
        "idle counts from the cap"
    );
    advance_and_wake(&session, exit);
    assert!(finished.recv_timeout(DEADLINE).expect("run ended in time"));

    let all = log::read(&session.dir).unwrap();
    assert_eq!(
        kinds(&all),
        [
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
            "usage_recorded",
            "usage_recorded",
        ]
    );
    let warm: Vec<_> = usage_lines(&all)
        .into_iter()
        .filter(|l| l.payload["generation_id"] == "gen_warm")
        .collect();
    assert_eq!(warm.len(), 2);
    assert!(warm[0].payload["cost"].is_null());
    assert_eq!(warm[1].payload["cost"].as_f64(), Some(0.75));
    for line in warm {
        assert!(line.turn_id.is_none());
        assert!(line.action_id.is_none());
    }
}

#[test]
fn a_cost_that_settles_mid_turn_is_written_at_the_next_step() {
    let start: Arc<OnceLock<Instant>> = Arc::new(OnceLock::new());
    let (mut session, seen) = looked(
        vec![
            failed_after("gen_failed"),
            // A call to a tool nobody registered completes failed, and the
            // turn takes a second step.
            calls_reply("", &[("nope", json!({}))]),
            priced("Done.", "gen_done", 0.01),
        ],
        CacheLifetime::OneHour,
        Some(0.5),
        {
            let start = Arc::clone(&start);
            move |clock| {
                Box::new(move |n| {
                    // The second turn's first call: the lookup comes due and
                    // settles while the turn runs.
                    if n == 2 {
                        settle(&clock, *start.get().unwrap() + AFTER);
                    }
                })
            }
        },
    );
    start.set(session.clock.now()).unwrap();
    let (first, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), FAILED_FIRST_KINDS);
    let lines = next_turn(&mut session);
    assert_eq!(
        kinds(&lines),
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_completed",
            "usage_recorded",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(seen.calls(), ["gen_failed"]);
    let steps: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.kind == "step_started")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(steps.len(), 2, "{:?}", kinds(&lines));
    let late = &lines[steps[1] - 1];
    assert_eq!(late.kind, "usage_recorded", "{:?}", kinds(&lines));
    assert_eq!(late.payload["generation_id"], "gen_failed");
    assert_eq!(late.turn_id, first.turn_id);
    assert_eq!(late.payload["cost"].as_f64(), Some(0.5));
}

#[test]
fn a_late_cost_counts_toward_the_next_turn_s_budget() {
    let (session, seen) = looked(
        vec![failed_after("gen_failed"), priced("Again.", "gen_2", 0.01)],
        CacheLifetime::OneHour,
        Some(1.0),
        no_hook,
    );
    let mut session = session.budget(Some(0.5));
    let start = session.clock.now();
    let (_, first_lines) = first_turn(&mut session);
    assert_eq!(kinds(&first_lines), FAILED_FIRST_KINDS);
    settle(&session.clock, start + AFTER);
    assert_eq!(seen.calls(), ["gen_failed"]);
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "usage_recorded",
            "turn_started",
            "step_started",
            "turn_completed",
        ]
    );
    assert_eq!(
        lines.last().unwrap().payload["error"]["code"],
        "budget_exceeded"
    );
    assert_eq!(session.requests().len(), 1);
}
