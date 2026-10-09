//! Tests for the running group's spinner: the frame it draws, and the
//! marks that never spin, read from the drawn buffer.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Drawn, spin};
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

/// The wall time every test sets: 2023-11-14T22:13:20Z.
const WALL: u64 = 1_700_000_000_000;

/// An app with home state at 80x24, attached, connected, its feed row
/// working and its turn running since `started`: the working line draws.
fn home_app(started: u64) -> (App, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_zone(crate::local_time::new_york());
    app.set_size(80, 24);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.on_line(status("streaming"));
    app.on_line(session_line_at(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
        started,
    ));
    (app, clock)
}

/// A `session_status` for the attached session in `state`.
fn status(state: &str) -> Line {
    session_line(
        "session_status",
        serde_json::json!({
            "name": "work", "workspace": "/w", "project": "-w",
            "state": state, "since": WALL,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0,
            "clients": 0}),
        None,
    )
}

/// One session envelope at wall time `ts` milliseconds.
fn session_line_at(kind: &str, payload: serde_json::Value, action: Option<&str>, ts: u64) -> Line {
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

/// A `turn_started` envelope with one message.
fn prompt(text: String) -> Line {
    session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
        None,
    )
}

/// A `session_status` naming the attached session as its parent: a
/// delegate's row, in `state`.
fn delegate(session: &str, state: &str) -> Line {
    let mut line = status(state);
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId(session.to_owned());
        envelope
            .payload
            .insert("parent".to_owned(), serde_json::json!(SESSION));
    }
    line
}

/// Renders `app` at `width` by `height`: the screen's text and the click
/// targets.
fn rendered(app: &App, width: u16, height: u16) -> (String, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, None);
    (text(&buf), targets)
}

/// Renders `app` at `width` by `height`: the screen's buffer.
fn buffer_sized(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    buf
}

#[test]
fn working_line_80x24() {
    let (mut app, clock) = home_app(WALL - 674_000);
    app.set_now(clock.origin(), WALL);
    let (shown, _) = rendered(&app, 80, 24);
    insta::assert_snapshot!("working_line_80x24", shown);
}

#[test]
fn working_line_glimmer_frame_2() {
    let (mut app, clock) = home_app(WALL - 674_000);
    let origin = clock.origin();
    app.set_now(origin, WALL);
    app.set_now(
        origin
            .checked_add(Duration::from_millis(240))
            .expect("after the origin"),
        WALL,
    );
    let (shown, _) = rendered(&app, 80, 24);
    insta::assert_snapshot!("working_line_glimmer_frame_2", shown);
    // Frame 2's band is the word's first three cells, in the spinner's
    // colour; the word past it holds the muted line style.
    let buf = buffer_sized(&app, 80, 24);
    let row = working_row(&buf, 80);
    for x in 0..3 {
        assert_eq!(
            buf.cell((x, row)).and_then(|cell| cell.style().fg),
            Some(crate::theme::Role::Accent.color()),
            "cell {x}"
        );
    }
    assert_eq!(
        buf.cell((3, row)).and_then(|cell| cell.style().fg),
        Some(crate::theme::Role::Muted.color()),
    );
}

/// The working line's screen row in a buffer `width` wide.
fn working_row(buf: &Buffer, width: u16) -> u16 {
    (0..buf.area.height)
        .find(|y| {
            let row: String = (0..width)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.starts_with("Working")
        })
        .unwrap_or_else(|| panic!("no working line"))
}

#[test]
fn working_line_reduced_has_no_glimmer() {
    let (mut app, clock) = home_app(WALL - 674_000);
    app.set_reduced_motion(true);
    app.set_now(clock.origin(), WALL);
    let (shown, _) = rendered(&app, 80, 24);
    insta::assert_snapshot!("working_line_reduced_has_no_glimmer", shown);
    // No cell of the line carries the spinner's colour.
    let buf = buffer_sized(&app, 80, 24);
    let row = working_row(&buf, 80);
    for x in 0..80 {
        assert_ne!(
            buf.cell((x, row)).and_then(|cell| cell.style().fg),
            Some(crate::theme::Role::Accent.color()),
            "cell {x}"
        );
    }
}

#[test]
fn working_line_retrying() {
    let (mut app, clock) = home_app(WALL);
    app.on_line(session_line("step_started", serde_json::json!({}), None));
    app.on_line(session_line(
        "assistant_message_started",
        serde_json::json!({}),
        Some("a_m"),
    ));
    // The schedule's wall time is now: four seconds show.
    app.on_line(session_line_at(
        "retry_scheduled",
        serde_json::json!({"code": "rate_limited", "attempt": 1,
            "delay_ms": 4_000, "last_attempt": 4}),
        Some("a_m"),
        WALL,
    ));
    app.set_now(clock.origin(), WALL);
    let (shown, _) = rendered(&app, 80, 24);
    insta::assert_snapshot!("working_line_retrying", shown);
}

#[test]
fn working_line_narrow_above_the_delegates_rows() {
    let (mut app, clock) = home_app(WALL - 674_000);
    for (job, session) in [("j_b", "s_bbbbbbbbbbbbbbbb"), ("j_c", "s_cccccccccccccccc")] {
        app.on_line(session_line(
            "job_started",
            serde_json::json!({"job_id": job, "description": "build",
                "output_path": "/tmp/out"}),
            None,
        ));
        app.on_line(session_line(
            "delegate_started",
            serde_json::json!({"job_id": job, "delegate_session_id": session,
                "harness": "fiber", "model": "test/model", "workspace": "/w"}),
            None,
        ));
        app.on_line(delegate(session, "streaming"));
    }
    app.set_now(clock.origin(), WALL);
    let (shown, _) = rendered(&app, 100, 30);
    insta::assert_snapshot!("working_line_narrow_above_the_delegates_rows", shown);
}

#[test]
fn working_line_shed_at_20_columns() {
    let (app, clock) = home_app(WALL - 674_000);
    // The draw lays the line at its column: twenty columns shed the
    // interrupt with its target. A full screen this narrow shows the
    // floor instead, so the draw is read on its own row.
    let mut app = app;
    app.set_now(clock.origin(), WALL);
    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw(&app, area, &mut buf, &mut bottom, &mut targets);
    insta::assert_snapshot!("working_line_shed_at_20_columns", text(&buf));
    assert!(
        !targets
            .iter()
            .any(|target| target.id == crate::mouse::TargetId::Interrupt),
        "{targets:?}"
    );
}

#[test]
fn the_interrupt_target_covers_its_cells() {
    let (mut app, clock) = home_app(WALL - 674_000);
    app.set_now(clock.origin(), WALL);
    let (shown, targets) = rendered(&app, 80, 24);
    let interrupt = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Interrupt)
        .unwrap_or_else(|| panic!("no interrupt target\n{shown}"));
    // The sixteen cells of `esc to interrupt` on the working line's row.
    assert_eq!((interrupt.rect.width, interrupt.rect.height), (16, 1));
    let buf = buffer_sized(&app, 80, 24);
    let mut text = String::new();
    for x in interrupt.rect.x..interrupt.rect.x + interrupt.rect.width {
        text.push_str(
            buf.cell((x, interrupt.rect.y))
                .map(|cell| cell.symbol())
                .unwrap_or("?"),
        );
    }
    assert_eq!(text, "esc to interrupt");
}

#[test]
fn the_interrupt_target_goes_with_its_text() {
    let (mut app, clock) = home_app(WALL - 674_000);
    app.set_now(clock.origin(), WALL);
    // Twenty columns shed the interrupt: the draw pushes no target.
    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw(&app, area, &mut buf, &mut bottom, &mut targets);
    assert!(
        !targets
            .iter()
            .any(|target| target.id == crate::mouse::TargetId::Interrupt),
        "{targets:?}"
    );
    assert_eq!(text(&buf), "Working 11m 14s\n");
}

#[test]
fn a_marked_line_under_new_messages_below_does_not_spin() {
    // One prompt, the running turn, one prompt: the group line sits on
    // the bottom row, so one page up from following stops at the top
    // with the group line under the overlay, and following shows it.
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
    // The covered mark schedules nothing: it is the only mark on
    // screen, and with no home there is no working line to ask.
    assert_eq!(app.take_wake(), None);
    // One row higher the same line spins: following again draws it.
    app.on_key(Key::End, now);
    let followed = screen(&app);
    assert!(!followed.contains("New messages below"), "{followed}");
    assert!(followed.contains(SPINNER[0]), "{followed}");
    assert!(app.take_wake().is_some());
}

/// An app with one prompt, then the running turn, then one prompt: the
/// group line sits on the conversation's bottom row.
fn running_first() -> (App, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app.attach(contract::SessionId(SESSION.to_owned()));
    for n in 1..=1 {
        app.on_line(prompt(format!("before {n}")));
    }
    app.on_line(prompt("go".to_owned()));
    app.on_line(session_line("step_started", serde_json::json!({}), None));
    app.on_line(session_line(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "a.rs"}}),
        Some("a_1"),
    ));
    // One prompt below: thirteen rows, so one page up from following
    // stops at the top, and following shows the group line again.
    for n in 1..=1 {
        app.on_line(prompt(format!("after {n}")));
    }
    (app, clock)
}

#[test]
fn a_mark_at_or_past_the_width_does_not_spin() {
    let area = Rect::new(0, 0, 4, 2);
    for col in [area.width, area.width + 1] {
        let (mut app, clock) = running();
        let origin = clock.origin();
        app.set_now(origin, 0);
        let mut buf = Buffer::empty(area);
        spin(
            &app,
            &mut buf,
            area,
            Drawn {
                y: 0,
                col,
                first_row_shown: true,
                rows: 1,
                count: 1,
            },
        );
        assert!(
            !text(&buf).contains(SPINNER[0]),
            "col {col}: {}",
            text(&buf)
        );
        assert_eq!(app.take_wake(), None, "col {col}");
    }
}

#[test]
fn a_mark_at_column_zero_can_spin_when_its_line_wraps() {
    let (mut app, clock) = running();
    let origin = clock.origin();
    app.set_now(origin, 0);
    let area = Rect::new(0, 0, 4, 2);
    let mut buf = Buffer::empty(area);
    // At column zero, a two-row line still has its mark on the first row.
    // `col > 0` must be false, and the conjunction must not hide the spin.
    spin(
        &app,
        &mut buf,
        area,
        Drawn {
            y: 0,
            col: 0,
            first_row_shown: true,
            rows: 2,
            count: 1,
        },
    );
    assert_eq!(buf.cell((0, 0)).map(|cell| cell.symbol()), Some(SPINNER[0]));
    assert_eq!(
        app.take_wake(),
        origin.checked_add(Duration::from_millis(120))
    );
}

#[test]
fn a_zero_height_area_does_not_reserve_a_new_messages_row() {
    let (mut app, clock) = running_first();
    let origin = clock.origin();
    app.on_key(Key::PageUp, clock.now());
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({"text": "streamed"}),
        Some("a_2"),
    ));
    assert!(app.has_new());
    app.set_now(origin, 0);
    let area = Rect::new(0, 1, 4, 0);
    let mut buf = Buffer::empty(area);
    // The zero-height area has no row to reserve; the mark at y=0 remains
    // before its bottom at 1 and asks for the next frame.
    spin(
        &app,
        &mut buf,
        area,
        Drawn {
            y: 0,
            col: 0,
            first_row_shown: true,
            rows: 1,
            count: 1,
        },
    );
    assert_eq!(
        app.take_wake(),
        origin.checked_add(Duration::from_millis(120))
    );
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
    // Ten calls in flight: the summary wraps over four rows.
    app.on_line(prompt("go".to_owned()));
    app.on_line(session_line("step_started", serde_json::json!({}), None));
    for n in 0..10 {
        app.on_line(session_line(
            "tool_call_requested",
            serde_json::json!({"name": "read", "arguments": {"path": format!("src/file{n:02}.rs")}}),
            Some(format!("a_{n}").as_str()),
        ));
    }
    // Two prompts below: sixteen rows, so following hides only the
    // summary's first row.
    for n in 1..=2 {
        app.on_line(prompt(format!("prompt {n}")));
    }
    app.set_now(clock.origin(), 0);
    let shown = screen(&app);
    // The later rows still show, without the spinner.
    assert!(shown.contains("file09.rs"), "{shown}");
    assert!(!shown.contains(SPINNER[0]), "{shown}");
    assert_eq!(app.take_wake(), None);
}

#[test]
fn handoff_band_writing_spinner() {
    let (mut app, clock) = writing_band(60, 12);
    app.set_now(clock.origin(), 0);
    insta::assert_snapshot!("handoff_band_writing_spinner", screen(&app));
}

/// An app with a writing handoff band at `width` by `height`.
fn writing_band(width: u16, height: u16) -> (App, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(session_line(
        "preamble_built",
        serde_json::json!({"reason": "start", "model": "fake/m",
            "context_window": 1_000_000, "tool_choice": "auto",
            "cache_lifetime": "5m", "system_prompt": "", "tools": [],
            "trigger_at": 400_000}),
        None,
    ));
    app.on_line(session_line(
        "handoff_started",
        serde_json::json!({"trigger": "auto"}),
        None,
    ));
    (app, clock)
}

#[test]
fn a_hidden_writing_band_asks_nothing() {
    let (mut app, clock) = writing_band(60, 12);
    // Fourteen prompts below: the band scrolls wholly above.
    for n in 1..=14 {
        app.on_line(prompt(format!("prompt {n}")));
    }
    app.set_now(clock.origin(), 0);
    let shown = screen(&app);
    assert!(!shown.contains(SPINNER[0]), "{shown}");
    assert_eq!(app.take_wake(), None);
}

#[test]
fn a_wrapped_writing_band_keeps_its_still_form() {
    let (mut app, clock) = writing_band(30, 12);
    app.set_now(clock.origin(), 0);
    // The band wraps so its dot lands on the second row past the
    // width: the mark never spins a row it does not start.
    let area = Rect::new(0, 0, 30, 12);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    assert!(text(&buf).contains('●'), "{}", text(&buf));
    assert!(!text(&buf).contains(SPINNER[0]), "{}", text(&buf));
    assert_eq!(app.take_wake(), None);
}

#[test]
fn a_mark_past_the_width_draws_nothing() {
    // The band's dot sits at column 34: at widths 34 and 32 it is past
    // the row, so nothing draws and nothing asks.
    for width in [34, 32] {
        let (mut app, clock) = writing_band(width, 12);
        app.set_now(clock.origin(), 0);
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        render(&app, area, &mut buf, None);
        assert!(!text(&buf).contains(SPINNER[0]), "{width}: {}", text(&buf));
        assert_eq!(app.take_wake(), None, "{width}");
    }
}

#[test]
fn a_reply_quoting_the_band_text_does_not_spin() {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    // A reply quoting the band's eventual text carries no mark: text
    // while a note is being written is the note's, not a reply, so the
    // quote comes first.
    app.on_line(session_line(
        "assistant_message_started",
        serde_json::json!({}),
        Some("a_m"),
    ));
    app.on_line(session_line(
        "text_completed",
        serde_json::json!({"text": "⇄ Handoff · automatic at 400.0k · ● writing the note…"}),
        Some("a_m"),
    ));
    app.on_line(session_line(
        "preamble_built",
        serde_json::json!({"reason": "start", "model": "fake/m",
            "context_window": 1_000_000, "tool_choice": "auto",
            "cache_lifetime": "5m", "system_prompt": "", "tools": [],
            "trigger_at": 400_000}),
        None,
    ));
    app.on_line(session_line(
        "handoff_started",
        serde_json::json!({"trigger": "auto"}),
        None,
    ));
    app.set_now(clock.origin(), 0);
    let shown = screen(&app);
    // Exactly one spinner: the band's. The quote keeps its dot.
    assert_eq!(shown.matches(SPINNER[0]).count(), 1, "{shown}");
    assert!(shown.contains('●'), "{shown}");
}

#[test]
fn rest_frames_keep_asking_for_the_next_frame() {
    let (mut app, clock) = home_app(WALL - 674_000);
    let origin = clock.origin();
    app.set_now(origin, WALL);
    // Frame 10 rests: no band draws, but the next boundary is asked, not
    // the wall second two seconds out.
    app.set_now(
        origin
            .checked_add(Duration::from_millis(1_200))
            .expect("after the origin"),
        WALL + 1_200,
    );
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    assert_eq!(
        app.take_wake(),
        origin.checked_add(Duration::from_millis(1_320))
    );
    // Frame 17 sweeps again from the word's first cell.
    app.set_now(
        origin
            .checked_add(Duration::from_millis(2_040))
            .expect("after the origin"),
        WALL + 2_040,
    );
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let row = working_row(&buf, 80);
    assert_eq!(
        buf.cell((0, row)).and_then(|cell| cell.style().fg),
        Some(crate::theme::Role::Accent.color())
    );
    assert_eq!(
        app.take_wake(),
        origin.checked_add(Duration::from_millis(2_160))
    );
}

#[test]
fn a_band_clipped_at_the_bottom_row_keeps_its_still_form() {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    // Forty columns: the band wraps in two, its dot on the first row in
    // column 34, inside the width, so only the single-row rule holds it.
    app.set_size(40, 12);
    app.attach(contract::SessionId(SESSION.to_owned()));
    for n in 1..=5 {
        app.on_line(prompt(format!("before {n}")));
    }
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(session_line(
        "preamble_built",
        serde_json::json!({"reason": "start", "model": "fake/m",
            "context_window": 1_000_000, "tool_choice": "auto",
            "cache_lifetime": "5m", "system_prompt": "", "tools": [],
            "trigger_at": 400_000}),
        None,
    ));
    app.on_line(session_line(
        "handoff_started",
        serde_json::json!({"trigger": "auto"}),
        None,
    ));
    // Two prompts below: thirty-four rows, so three wheel steps up from
    // following lands the band's first row on the bottom row, its second
    // row below the area.
    for n in 1..=2 {
        app.on_line(prompt(format!("after {n}")));
    }
    for _ in 0..3 {
        app.on_wheel(&Mouse {
            kind: MouseKind::WheelUp,
            col: 15,
            row: 5,
        });
    }
    app.set_now(clock.origin(), 0);
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    // The first band row shows on the conversation's bottom row with
    // its dot still: a wrapped mark never spins.
    assert!(
        shown
            .lines()
            .nth(8)
            .is_some_and(|row| row.starts_with("⇄ Handoff")),
        "{shown}"
    );
    assert!(
        shown.lines().nth(8).is_some_and(|row| row.contains('●')),
        "{shown}"
    );
    assert!(!shown.contains(SPINNER[0]), "{shown}");
    assert_eq!(app.take_wake(), None);
}
