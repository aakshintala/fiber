//! Run-level tests for the tick: what a frame arms, what a tick draws,
//! and how ticks queue behind keys and history fetches, on the fake
//! clock beds `new_loop` lays (no home, so the working line never draws).

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::{Input, Loop};
use super::new_loop;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// One session envelope.
fn session_line(kind: &str, payload: serde_json::Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A loop with a running group: a turn, its step and its requested call.
/// No home, so the working line never draws; only the group line moves.
fn running() -> (Loop<TestBackend>, Arc<FakeClock>) {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    lp.app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    lp.app
        .on_line(session_line("step_started", serde_json::json!({}), None));
    lp.app.on_line(session_line(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "a.rs"}}),
        Some("a_1"),
    ));
    (lp, clock)
}

/// Steps `input` with nowhere to fetch from: with no hub no history is
/// asked for.
fn step(lp: &mut Loop<TestBackend>, input: Input) -> Option<i32> {
    let (_hub, idle) = mpsc::channel();
    lp.step(input, &idle)
}

/// The drawn screen as text.
fn screen(lp: &Loop<TestBackend>) -> String {
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(&lp.app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn a_running_group_arms_the_next_boundary() {
    let (mut lp, clock) = running();
    let origin = clock.origin();
    assert_eq!(step(&mut lp, Input::Resize), None);
    assert_eq!(
        lp.tick.armed(),
        origin.checked_add(Duration::from_millis(120))
    );
}

#[test]
fn an_idle_terminal_arms_nothing() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    assert_eq!(step(&mut lp, Input::Resize), None);
    assert_eq!(lp.tick.armed(), None);
}

#[test]
fn under_reduced_motion_a_running_group_arms_nothing() {
    let (mut lp, clock) = running();
    let _origin = clock.origin();
    lp.app.set_reduced_motion(true);
    assert_eq!(step(&mut lp, Input::Resize), None);
    // With no home the working line never draws and asks no wall second,
    // so this stays true after the line lands.
    assert_eq!(lp.tick.armed(), None);
}

#[test]
fn a_tick_through_the_thread_draws_the_next_frame() {
    let (mut lp, clock) = running();
    let origin = clock.origin();
    let at = origin
        .checked_add(Duration::from_millis(120))
        .expect("after the origin");
    let shared: Arc<dyn Clock> = clock.clone();
    let (tx, rx) = mpsc::channel();
    lp.tick.start(shared, tx);
    assert_eq!(step(&mut lp, Input::Resize), None);
    assert!(
        clock.await_parked(at, DEADLINE),
        "waited {DEADLINE:?} for the ticker to park on the clock"
    );
    clock.advance(Duration::from_millis(120));
    assert!(matches!(
        rx.recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the tick: {err}")),
        Input::Tick
    ));
    assert_eq!(step(&mut lp, Input::Tick), None);
    assert!(screen(&lp).contains("⠙"), "{}", screen(&lp));
    lp.tick.stop();
}

#[test]
fn a_tick_behind_a_key_is_acked_before_the_next() {
    let (mut lp, clock) = running();
    let origin = clock.origin();
    let at = origin
        .checked_add(Duration::from_millis(120))
        .expect("after the origin");
    let shared: Arc<dyn Clock> = clock.clone();
    let (tx, rx) = mpsc::channel();
    lp.tick.start(shared, tx);
    assert_eq!(step(&mut lp, Input::Resize), None);
    assert!(
        clock.await_parked(at, DEADLINE),
        "waited {DEADLINE:?} for the ticker to park on the clock"
    );
    clock.advance(Duration::from_millis(120));
    // Tick A arrives and is held: nothing is stepped yet.
    assert!(matches!(
        rx.recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for tick A: {err}")),
        Input::Tick
    ));
    // A key steps and re-arms; the clock moves past the new deadline
    // while tick A is still out. That nothing sends meanwhile lives in
    // `gate_step_table`'s in-flight row; no quiet wait proves it here.
    assert_eq!(step(&mut lp, Input::Bytes(b"x".to_vec())), None);
    clock.advance(Duration::from_millis(240));
    // Stepping tick A acks it. The step re-arms the next boundary, so
    // advancing to it delivers tick B whichever arm the thread reads:
    // the stale one past, or the new one reached now.
    assert_eq!(step(&mut lp, Input::Tick), None);
    clock.advance(Duration::from_millis(120));
    assert!(matches!(
        rx.recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for tick B: {err}")),
        Input::Tick
    ));
    lp.tick.stop();
}

#[test]
fn a_tick_while_a_frame_waits_for_history_is_acked() {
    let (mut lp, clock) = running();
    let origin = clock.origin();
    let at = origin
        .checked_add(Duration::from_millis(120))
        .expect("after the origin");
    let shared: Arc<dyn Clock> = clock.clone();
    let (tx, rx) = mpsc::channel();
    lp.tick.start(shared, tx.clone());
    assert_eq!(step(&mut lp, Input::Resize), None);
    assert!(
        clock.await_parked(at, DEADLINE),
        "waited {DEADLINE:?} for the ticker to park on the clock"
    );
    clock.advance(Duration::from_millis(120));
    // The tick the thread sent waits in the channel behind the answer.
    tx.send(Input::Hub(session_line(
        "command_accepted",
        serde_json::json!({"command_id": "c_1", "result": {"lines": []}}),
        None,
    )))
    .unwrap_or_else(|err| panic!("send: {err}"));
    let lines = lp.answer(&rx, "c_1");
    assert_eq!(lines, Ok(Vec::new()));
    // Without the ack the next armed tick would never come: a deadline
    // already past sends at once.
    lp.tick.arm(Some(origin));
    assert!(matches!(
        rx.recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the next tick: {err}")),
        Input::Tick
    ));
    lp.tick.stop();
}

/// A hub `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A `session_status` for the attached session in `state`.
fn status(state: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({
        "name": "fix the parser", "workspace": "/w",
        "project": "-w", "state": "idle", "since": 1_700_000_000_000u64,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    session_line("session_status", payload, None)
}

#[test]
fn under_reduced_motion_a_working_line_arms_its_next_second() {
    let (mut lp, clock) = running();
    let origin = clock.origin();
    // Home, a working feed row and a busy turn: the line draws.
    lp.app.set_home(crate::home::Launch {
        workspace: std::path::PathBuf::from("/w"),
        project: "-w".to_owned(),
        ..Default::default()
    });
    lp.app.on_line(hello());
    lp.app
        .on_line(status(serde_json::json!({"state": "streaming"})));
    lp.app.set_reduced_motion(true);
    assert_eq!(step(&mut lp, Input::Resize), None);
    // The wall second is armed, not the next frame boundary.
    assert_eq!(
        lp.tick.armed(),
        origin.checked_add(Duration::from_millis(1_000))
    );
}

#[test]
fn quitting_stops_the_ticker() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let clock = FakeClock::new();
    let shared: Arc<dyn Clock> = clock.clone();
    lp.clock = clock;
    let (tx, rx) = mpsc::channel();
    lp.tick.start(shared, tx.clone());
    drop(tx);
    // Quitting first ends the thread, dropping its sender; the loop then
    // sees every sender gone and returns.
    lp.tick.stop();
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("motion-quit".to_owned())
        .spawn(move || done.send(lp.run(&rx)).unwrap_or(()))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    assert_eq!(
        finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the loop to quit: {err}")),
        0
    );
}
