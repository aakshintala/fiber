//! Tests for the running group's spinner: the frame it draws, and the
//! marks that never spin, read from the drawn buffer.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::keys::{Key, Mouse, MouseKind};
use crate::link::Line;
use crate::motion::SPINNER;
use crate::view::{render, text};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const WIDTH: u16 = 60;
const HEIGHT: u16 = 12;

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

/// An app with a running group: a turn, its step and its requested call.
fn running() -> (App, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(session_line("step_started", serde_json::json!({}), None));
    app.on_line(session_line(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "a.rs"}}),
        Some("a_1"),
    ));
    (app, clock)
}

/// The drawn screen as text.
fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    text(&buf)
}

/// The drawn screen's buffer.
fn buffer(app: &App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    buf
}

#[test]
fn group_line_spinner_frame_0() {
    let (mut app, clock) = running();
    app.set_now(clock.origin(), 0);
    insta::assert_snapshot!("group_line_spinner_frame_0", screen(&app));
}

#[test]
fn group_line_spinner_frame_3() {
    let (mut app, clock) = running();
    let origin = clock.origin();
    app.set_now(origin, 0);
    app.set_now(
        origin
            .checked_add(Duration::from_millis(360))
            .expect("after the origin"),
        0,
    );
    insta::assert_snapshot!("group_line_spinner_frame_3", screen(&app));
}

#[test]
fn group_line_reduced_keeps_its_bullet() {
    let (mut app, clock) = running();
    app.set_reduced_motion(true);
    app.set_now(clock.origin(), 0);
    insta::assert_snapshot!("group_line_reduced_keeps_its_bullet", screen(&app));
}

#[test]
fn keyless_group_line_spinner() {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    // Arguments streaming with no requested call: the group has no key
    // and no target, and still moves.
    app.on_line(session_line(
        "tool_call_arguments_delta",
        serde_json::json!({"index": 0, "text": "{\"pa"}),
        Some("a_m"),
    ));
    app.set_now(clock.origin(), 0);
    insta::assert_snapshot!("keyless_group_line_spinner", screen(&app));
}

/// A `turn_started` envelope with one message.
fn prompt(text: String) -> Line {
    session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
        None,
    )
}

#[test]
fn a_marked_line_under_new_messages_below_does_not_spin() {
    // Four prompts, the running turn, three prompts: seventeen rows with
    // the group line eleventh, so one page up from following stops at
    // the top with the group line on the bottom row.
    let (mut app, clock) = running_first();
    let origin = clock.origin();
    let now = clock.now();
    app.set_now(origin, 0);
    app.on_key(Key::PageUp, now);
    // New output while scrolled up: the overlay takes the bottom row.
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({"text": "streamed"}),
        Some("a_2"),
    ));
    let shown = screen(&app);
    assert!(shown.contains("New messages below"), "{shown}");
    assert_eq!(app.take_wake(), None);
    // One row higher the same line spins: following again draws it.
    app.on_key(Key::End, now);
    let followed = screen(&app);
    assert!(!followed.contains("New messages below"), "{followed}");
    assert!(followed.contains(SPINNER[0]), "{followed}");
    assert!(app.take_wake().is_some());
}

/// An app with four prompts, then the running turn, then three prompts:
/// seventeen rows with the group line eleventh.
fn running_first() -> (App, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app.attach(contract::SessionId(SESSION.to_owned()));
    for n in 1..=4 {
        app.on_line(prompt(format!("before {n}")));
    }
    app.on_line(prompt("go".to_owned()));
    app.on_line(session_line("step_started", serde_json::json!({}), None));
    app.on_line(session_line(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "a.rs"}}),
        Some("a_1"),
    ));
    for n in 1..=3 {
        app.on_line(prompt(format!("after {n}")));
    }
    (app, clock)
}

#[test]
fn untargeted_lines_never_spin() {
    let (mut app, clock) = running();
    // A steering message and thinking: lines with no mark and no target,
    // and the group still runs.
    app.on_line(session_line(
        "steering_applied",
        serde_json::json!({"content": [{"type": "text", "text": "use x"}], "source": "driver"}),
        None,
    ));
    app.on_line(session_line(
        "reasoning_started",
        serde_json::json!({}),
        Some("a_t"),
    ));
    app.on_line(session_line(
        "reasoning_completed",
        serde_json::json!({"text": "# Plan"}),
        Some("a_t"),
    ));
    // Without time nothing spins: the baseline buffer.
    let base = buffer(&app);
    app.set_now(clock.origin(), 0);
    let live = buffer(&app);
    // Exactly one cell changed: the group line's spinner.
    let mut diffs = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (before, after) = (&base[(x, y)], &live[(x, y)]);
            if before.symbol() != after.symbol() {
                diffs.push((x, y, before.symbol().to_owned(), after.symbol().to_owned()));
            }
        }
    }
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    let (x, _y, before, after) = &diffs[0];
    assert_eq!((*x, before.as_str()), (0, "•"));
    assert_eq!(after.as_str(), SPINNER[0]);
}

#[test]
fn a_scrolled_off_group_line_asks_nothing() {
    let (mut app, clock) = running();
    // Fourteen prompts below: the group line scrolls wholly above.
    for n in 1..=14 {
        app.on_line(prompt(format!("prompt {n}")));
    }
    app.set_now(clock.origin(), 0);
    let shown = screen(&app);
    assert!(!shown.contains(SPINNER[0]), "{shown}");
    assert_eq!(app.take_wake(), None);
}

#[test]
fn a_group_line_whose_first_row_is_hidden_asks_nothing() {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app.attach(contract::SessionId(SESSION.to_owned()));
    // Ten calls in flight: the summary wraps over two rows.
    app.on_line(prompt("go".to_owned()));
    app.on_line(session_line("step_started", serde_json::json!({}), None));
    for n in 0..10 {
        app.on_line(session_line(
            "tool_call_requested",
            serde_json::json!({"name": "read", "arguments": {"path": format!("src/file{n:02}.rs")}}),
            Some(format!("a_{n}").as_str()),
        ));
    }
    // Six prompts and a steering message below: fifteen rows, so one
    // wheel step up from following hides only the line's first row.
    for n in 1..=6 {
        app.on_line(prompt(format!("prompt {n}")));
    }
    app.on_line(session_line(
        "steering_applied",
        serde_json::json!({"content": [{"type": "text", "text": "use x"}], "source": "driver"}),
        None,
    ));
    app.set_now(clock.origin(), 0);
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelUp,
        col: 30,
        row: 5,
    });
    let shown = screen(&app);
    // The second row still shows, without the spinner.
    assert!(shown.contains("file09.rs"), "{shown}");
    assert!(!shown.contains(SPINNER[0]), "{shown}");
    assert_eq!(app.take_wake(), None);
}
