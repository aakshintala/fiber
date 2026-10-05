//! Idle exit (`docs/invocation.md`, "Lifecycle"): a session with no turn
//! running ends `run` once `session.idle_exit_ms` has passed on the loop's
//! clock. Waiting on an approval is idle. A delivery that starts no turn
//! does not move the deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::commands::{Reply, ReplyAnswer};
use contract::events::Decision;
use contract::inbox::{Ack, Delivery};
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::Effect;
use contract::tool::Tool;
use contract::{CommandId, RequestId};
use fakes::Scripted;

use support::{DEADLINE, Session, TestTool, calls_reply, delivery, ignore, kinds};

fn arm(session: &mut Session, after: Option<Duration>) {
    session.looped = Some(session.looped.take().unwrap().idle_exit(after));
}

/// Runs `run` on its own thread and returns the channel it reports on.
fn spawn_run(session: &mut Session) -> mpsc::Receiver<Result<(), r#loop::Error>> {
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let result = looped.run();
        done.send(result).unwrap();
    });
    finished
}

fn parked(clock: &fakes::clock::FakeClock, until: Instant, what: &str) {
    assert!(clock.await_parked(until, DEADLINE), "{what}");
}

fn wake(session: &Session) {
    session.inbox.send(Delivery::Cancelled).unwrap();
}

fn stale_reply(session: &Session) -> mpsc::Receiver<bool> {
    let (done, rejected) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Reply(
            Reply {
                request_id: RequestId("r_absent".into()),
                answer: ReplyAnswer::Approval {
                    decision: Decision::Deny,
                    feedback: None,
                    remember: None,
                },
            },
            Ack(Box::new(move |answer| {
                done.send(answer.is_err()).unwrap();
            })),
        ))
        .unwrap();
    rejected
}

fn durable(session: &Session) -> Vec<String> {
    log::read(&session.dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect()
}

#[test]
fn an_empty_inbox_parks_until_the_idle_deadline() {
    let mut session = Session::new(Vec::new(), None);
    arm(&mut session, Some(Duration::from_secs(60)));
    let clock = Arc::clone(&session.clock);
    let deadline = clock.now() + Duration::from_secs(60);
    let finished = spawn_run(&mut session);
    parked(&clock, deadline, "the idle wait parks until the deadline");

    clock.advance(Duration::from_millis(59_999));
    let rejected = stale_reply(&session);
    wake(&session);
    assert!(
        rejected
            .recv_timeout(DEADLINE)
            .expect("the early wake was admitted"),
        "a reply while idle is rejected"
    );
    parked(
        &clock,
        deadline,
        "a wake before the deadline keeps the same idle wait",
    );
    assert!(
        finished.try_recv().is_err(),
        "59.999s does not end the idle wait"
    );

    clock.advance(Duration::from_millis(1));
    wake(&session);
    let ran = finished
        .recv_timeout(DEADLINE)
        .expect("run ended at the deadline");
    assert!(ran.is_ok(), "{ran:?}");
    assert_eq!(durable(&session), ["session_started"]);
}

#[test]
fn a_queued_prompt_starts_a_turn_even_at_a_zero_delay() {
    let mut session = Session::new(vec![Scripted::text("ok.")], None);
    arm(&mut session, Some(Duration::ZERO));
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    assert_eq!(
        kinds(&session.lines()),
        [
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
    );
}

#[test]
fn the_delay_restarts_after_a_turn() {
    let mut session = Session::new(vec![Scripted::text("ok.")], None);
    arm(&mut session, Some(Duration::from_secs(60)));
    let clock = Arc::clone(&session.clock);
    let origin = clock.now();
    let finished = spawn_run(&mut session);
    parked(
        &clock,
        origin + Duration::from_secs(60),
        "idle before the first turn",
    );
    clock.advance(Duration::from_secs(30));
    session.inbox.send(delivery("hi")).unwrap();
    parked(
        &clock,
        origin + Duration::from_secs(90),
        "the delay restarts from turn_completed",
    );
    assert!(
        finished.try_recv().is_err(),
        "the restarted delay has not passed"
    );
    // Dropping the sender ends the wait, so the thread can finish.
    drop(session.inbox);
    let ran = finished
        .recv_timeout(DEADLINE)
        .expect("run ends once the inbox is gone");
    assert!(ran.is_ok(), "{ran:?}");
}

#[test]
fn a_rejected_reply_a_steer_drop_and_a_wake_do_not_move_the_deadline() {
    let mut session = Session::new(Vec::new(), None);
    arm(&mut session, Some(Duration::from_secs(60)));
    let clock = Arc::clone(&session.clock);
    let deadline = clock.now() + Duration::from_secs(60);
    let finished = spawn_run(&mut session);
    parked(&clock, deadline, "idle before the junk");
    clock.advance(Duration::from_secs(10));

    let rejected = stale_reply(&session);
    assert!(
        rejected
            .recv_timeout(DEADLINE)
            .expect("the reply was admitted"),
        "a reply while idle is rejected"
    );
    parked(
        &clock,
        deadline,
        "a rejected reply does not move the deadline",
    );

    let (dropped, drop_rx) = mpsc::channel();
    session
        .inbox
        .send(Delivery::SteerDrop(
            CommandId("c_missing".into()),
            Ack(Box::new(move |answer| {
                dropped.send(answer.is_err()).unwrap();
            })),
        ))
        .unwrap();
    assert!(
        drop_rx
            .recv_timeout(DEADLINE)
            .expect("the drop was admitted"),
        "a steer_drop while idle is rejected"
    );
    parked(&clock, deadline, "a steer_drop does not move the deadline");

    wake(&session);
    parked(&clock, deadline, "a wake does not move the deadline");
    assert!(
        finished.try_recv().is_err(),
        "junk does not end the idle wait"
    );
}

#[test]
fn close_ends_the_idle_wait_at_once() {
    let mut session = Session::new(Vec::new(), None);
    arm(&mut session, Some(Duration::from_secs(60)));
    let clock = Arc::clone(&session.clock);
    let deadline = clock.now() + Duration::from_secs(60);
    let finished = spawn_run(&mut session);
    parked(&clock, deadline, "idle before close");
    let (closed, close_rx) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Close(Ack(Box::new(move |answer| {
            closed.send(answer.is_ok()).unwrap();
        }))))
        .unwrap();
    assert!(
        close_rx.recv_timeout(DEADLINE).expect("close was admitted"),
        "close is accepted"
    );
    let ran = finished
        .recv_timeout(DEADLINE)
        .expect("close ends the idle wait");
    assert!(ran.is_ok(), "{ran:?}");
    assert_eq!(durable(&session), ["session_started"]);
}

#[test]
fn no_deadline_never_expires() {
    let mut session = Session::new(Vec::new(), None);
    arm(&mut session, None);
    let clock = Arc::clone(&session.clock);
    let finished = spawn_run(&mut session);
    assert!(
        wait_parked_unbounded(&clock),
        "a loop with no idle delay parks with no deadline"
    );
    clock.advance(Duration::from_secs(24 * 60 * 60));
    let rejected = stale_reply(&session);
    wake(&session);
    assert!(
        rejected
            .recv_timeout(DEADLINE)
            .expect("the wake was admitted"),
        "a reply while idle is rejected"
    );
    assert!(
        wait_parked_unbounded(&clock),
        "a wake does not expire a loop with no idle delay"
    );
    assert!(
        finished.try_recv().is_err(),
        "no deadline means the idle wait does not end"
    );
}

/// True once a thread is parked in `wait_until` with no deadline, within
/// [`DEADLINE`].
fn wait_parked_unbounded(clock: &Arc<fakes::clock::FakeClock>) -> bool {
    let clock = Arc::clone(clock);
    let (done, waiting) = mpsc::channel();
    thread::spawn(move || {
        while !clock.parked().contains(&None) {
            thread::yield_now();
        }
        if let Ok(()) = done.send(()) {}
    });
    waiting.recv_timeout(DEADLINE).is_ok()
}

fn shell(subject: &str) -> Arc<TestTool> {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = Some(subject.to_owned());
    Arc::new(tool)
}

fn standing() -> StandingRules {
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

fn asking() -> Session {
    let tool = shell("npm publish");
    let session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", serde_json::json!({"city": "Paris"}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    );
    session.rules.set(standing());
    session
}

#[test]
fn an_approval_wait_parks_until_the_idle_deadline_and_writes_nothing_more() {
    let mut session = asking();
    arm(&mut session, Some(Duration::from_secs(60)));
    let clock = Arc::clone(&session.clock);
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    let start = clock.now();
    // The turn runs before the approval wait, on the same instant: the
    // deadline is that instant plus the delay.
    parked(
        &clock,
        start + Duration::from_secs(60),
        "the approval wait parks until the deadline",
    );

    clock.advance(Duration::from_millis(59_999));
    let rejected = stale_reply(&session);
    wake(&session);
    assert!(
        rejected
            .recv_timeout(DEADLINE)
            .expect("the early wake was admitted"),
        "a reply naming nothing pending is rejected"
    );
    parked(
        &clock,
        start + Duration::from_secs(60),
        "a wake before the deadline keeps the approval wait",
    );
    assert!(
        finished.try_recv().is_err(),
        "59.999s does not end the approval wait"
    );

    clock.advance(Duration::from_millis(1));
    wake(&session);
    let ran = finished
        .recv_timeout(DEADLINE)
        .expect("run ended at the approval deadline");
    assert!(ran.is_ok(), "{ran:?}");
    assert_eq!(
        durable(&session),
        [
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
        ]
    );
}

#[test]
fn a_reply_before_the_deadline_resolves_the_approval() {
    let mut session = asking();
    arm(&mut session, Some(Duration::from_secs(60)));
    session.inbox.send(delivery("hi")).unwrap();
    let inbox = session.inbox.clone();
    let answered = support::on_request(&session, move |request_id| {
        inbox
            .send(Delivery::Reply(
                Reply {
                    request_id,
                    answer: ReplyAnswer::Approval {
                        decision: Decision::Allow,
                        feedback: None,
                        remember: None,
                    },
                },
                ignore(),
            ))
            .unwrap();
    });
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    answered.join().unwrap();
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
    assert_eq!(lines.last().unwrap().payload["outcome"], "completed");
}
