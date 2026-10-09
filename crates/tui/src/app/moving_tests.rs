//! Tests for the working line's state: when it shows, what it says
//! across a page cut, and what its click sends.

use std::path::PathBuf;
use std::time::Duration;

use contract::Seq;
use fakes::clock::FakeClock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::super::{App, Effect};
use crate::home::Launch;
use crate::link::Line;
use crate::mouse::TargetId;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const WALL: u64 = 1_700_000_000_000;

/// An app with home state, attached to [`SESSION`], at 80x24.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(80, 24);
    app
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

/// One session envelope.
fn session_line(kind: &str, payload: Value, action: Option<&str>, ts: u64) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `session_status` for the attached session in `state`.
fn status(state: Value) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": "/w",
        "project": "-w", "state": "idle", "since": WALL,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    session_line("session_status", payload, None, 0)
}

/// A turn running since `ts` milliseconds.
fn running(app: &mut App, ts: u64) {
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
        ts,
    ));
}

/// The attached session's feed row reads `state`.
fn fed(app: &mut App, state: Value) {
    app.on_line(hello());
    app.on_line(status(state));
}

#[test]
fn the_line_shows_only_with_home_a_busy_turn_and_no_wait() {
    // No home: attached and busy, but no home state.
    let mut homeless = App::new(PathBuf::from("/w"));
    homeless.attach(contract::SessionId(SESSION.to_owned()));
    running(&mut homeless, 0);
    assert!(homeless.working_line().is_none());
    // Idle: home and attached, but no turn runs.
    let mut idle = app();
    fed(&mut idle, json!({"state": "idle"}));
    assert!(idle.working_line().is_none());
    // Busy: home, a running turn, and a working feed row.
    let mut busy = app();
    fed(&mut busy, json!({"state": "streaming"}));
    running(&mut busy, 0);
    let line = busy.working_line().expect("the working line");
    assert_eq!(line.started_ms, Some(0));
    assert!(line.retry.is_none());
    // Busy with a waiting feed row: the wait's own line shows instead.
    let mut waiting = app();
    fed(
        &mut waiting,
        json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "shell cargo test"}}),
    );
    running(&mut waiting, 0);
    assert!(waiting.working_line().is_none());
    // Busy with a waiting row that left: a stale waiting state with an
    // exited row still shows the line while the phase stays busy.
    let mut left = app();
    fed(
        &mut left,
        json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "shell cargo test"}}),
    );
    running(&mut left, 0);
    left.on_line(Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"session_id": SESSION, "how": "exited"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
    assert!(left.working_line().is_some());
    // Busy with the banner: the banner wins the row.
    let mut bannered = app();
    fed(&mut bannered, json!({"state": "streaming"}));
    running(&mut bannered, 0);
    bannered.connect_failed("Could not reach the hub: refused".to_owned());
    bannered.next_retry();
    assert!(bannered.working_line().is_none());
    assert!(bannered.working_row_shown());
}

#[test]
fn under_reduced_motion_the_line_asks_its_next_wall_second() {
    let clock = FakeClock::new();
    let origin = clock.origin();
    let started = WALL;
    let mut app = app();
    fed(&mut app, json!({"state": "streaming"}));
    running(&mut app, started);
    app.set_reduced_motion(true);
    // 400 ms into the turn: the next whole second is 600 ms out, not the
    // next 120 ms frame boundary.
    app.set_now(origin, started + 400);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    assert_eq!(
        app.take_wake(),
        origin.checked_add(Duration::from_millis(600))
    );
}

#[test]
fn working_row_shown_counts_the_banner_or_the_line() {
    let mut app = app();
    fed(&mut app, json!({"state": "idle"}));
    assert!(!app.working_row_shown());
    running(&mut app, 0);
    // Idle feed row with a busy turn: the line shows.
    fed(&mut app, json!({"state": "streaming"}));
    assert!(app.working_row_shown());
    // The banner shows without the line.
    app.connect_failed("Could not reach the hub: refused".to_owned());
    app.next_retry();
    assert!(app.working_row_shown());
    assert!(app.working_line().is_none());
}

#[test]
fn conversation_height_loses_one_row_while_the_line_shows() {
    let mut app = app();
    fed(&mut app, json!({"state": "idle"}));
    let idle = app.conversation_height();
    fed(&mut app, json!({"state": "streaming"}));
    running(&mut app, 0);
    assert!(app.working_line().is_some());
    assert_eq!(app.conversation_height(), idle.saturating_sub(1));
}

#[test]
fn clicking_interrupt_sends_cancel() {
    let mut app = app();
    fed(&mut app, json!({"state": "streaming"}));
    running(&mut app, 0);
    match app.on_click(TargetId::Interrupt) {
        Effect::Send(lines) => assert!(
            lines
                .iter()
                .any(|line| line.contains("\"command\":\"cancel\"")),
            "{lines:?}"
        ),
        Effect::None
        | Effect::Quit
        | Effect::Exit(_)
        | Effect::ListFiles
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_)
        | Effect::FindPause { .. }
        | Effect::OpenLink(_)
        | Effect::ReadImage(_)
        | Effect::OpenFile(_) => panic!("interrupt sent nothing"),
    }
}

#[test]
fn clicking_interrupt_when_idle_or_disconnected_sends_nothing() {
    // Idle: nothing to interrupt.
    let mut idle = app();
    fed(&mut idle, json!({"state": "idle"}));
    assert_eq!(idle.on_click(TargetId::Interrupt), Effect::None);
    // Busy but disconnected: the cancel would go nowhere.
    let mut down = app();
    down.on_line(status(json!({"state": "streaming"})));
    running(&mut down, 0);
    assert_eq!(down.on_click(TargetId::Interrupt), Effect::None);
}

#[test]
fn elapsed_holds_across_a_page_cut_in_a_long_running_turn() {
    let started = 5_000;
    let mut app = app();
    fed(&mut app, json!({"state": "streaming"}));
    let mut seq = 0;
    let mut next = move || {
        seq += 1;
        Seq(seq - 1)
    };
    app.on_line(seq_line(
        "turn_started",
        None,
        json!({"input": [{
        "type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]}),
        started,
        next(),
    ));
    // Seventy replies: past the page's sixty-four lines, so the next
    // step cuts a page inside the running turn.
    for reply in 0..70 {
        app.on_line(seq_line(
            "text_completed",
            Some(&format!("a_m{reply}")),
            json!({"text": "line"}),
            started,
            next(),
        ));
    }
    app.on_line(seq_line("step_started", None, json!({}), started, next()));
    app.on_line(seq_line(
        "text_completed",
        Some("a_mx"),
        json!({"text": "more"}),
        started,
        next(),
    ));
    assert!(app.pages().index().pages().len() > 1, "no page cut");
    // The elapsed time reads the turn's start, not the open part's.
    let line = app.working_line().expect("the working line");
    assert_eq!(line.started_ms, Some(started));
}

/// One session envelope with its durable sequence number.
fn seq_line(kind: &str, action: Option<&str>, payload: Value, ts: u64, seq: contract::Seq) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: Some(seq),
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

#[test]
fn the_retry_clears_when_the_call_gets_through() {
    for (kind, action, payload) in [
        (
            "assistant_message_delta",
            Some("a_m"),
            json!({"text": "Hi"}),
        ),
        ("text_completed", Some("a_m"), json!({"text": "Hi"})),
        ("reasoning_started", Some("a_r"), json!({})),
        ("turn_completed", None, json!({"outcome": "completed"})),
        (
            "tool_call_requested",
            Some("a_1"),
            json!({"name": "read", "arguments": {"path": "a"}}),
        ),
    ] {
        let mut app = app();
        fed(&mut app, json!({"state": "streaming"}));
        running(&mut app, 0);
        app.on_line(session_line("step_started", json!({}), None, 0));
        app.on_line(session_line(
            "retry_scheduled",
            json!({"code": "rate_limited", "attempt": 2,
                "delay_ms": 1_000, "last_attempt": 4}),
            Some("a_m"),
            0,
        ));
        assert!(app.working_line().is_some_and(|line| line.retry.is_some()));
        app.on_line(session_line(kind, payload, action, 0));
        if kind == "turn_completed" {
            // The turn ended with the call: no line, no retry.
            assert!(app.working_line().is_none(), "{kind}");
        } else {
            assert!(
                app.working_line().is_some_and(|line| line.retry.is_none()),
                "{kind}"
            );
        }
    }
}

#[test]
fn jobs_alone_ask_nothing() {
    let clock = FakeClock::new();
    let mut app = app();
    fed(&mut app, json!({"state": "jobs"}));
    app.set_now(clock.origin(), WALL);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    // The feed row works jobs with no busy turn: no line, no wake.
    assert!(app.working_line().is_none());
    assert_eq!(app.take_wake(), None);
}

#[test]
fn the_banner_replaces_the_line() {
    let mut app = app();
    fed(&mut app, json!({"state": "streaming"}));
    running(&mut app, 0);
    app.connect_failed("Could not reach the hub: refused".to_owned());
    app.next_retry();
    app.next_retry();
    assert!(app.working_line().is_none());
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let shown = crate::view::text(&buf);
    assert!(shown.contains("reconnecting (attempt 2)"), "{shown}");
    assert!(!shown.contains("Working"), "{shown}");
}
