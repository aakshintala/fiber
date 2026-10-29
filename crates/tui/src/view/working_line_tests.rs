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
use crate::theme::Role;
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
            "state": state, "started": WALL, "calls": 0, "since": WALL,
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
    // Frame 2's band is the word's first three cells, its centre bold in
    // the spinner's colour and its sides in it; the word past it holds
    // the muted line style. The word starts past the indent and spinner.
    let buf = buffer_sized(&app, 80, 24);
    let row = working_row(&buf, 80);
    for x in [5, 7] {
        assert_eq!(
            buf.cell((x, row)).and_then(|cell| cell.style().fg),
            Some(Role::Attention.color()),
            "cell {x}"
        );
        assert!(
            !buf.cell((x, row))
                .map(|cell| cell.style().add_modifier)
                .is_some_and(|modifier| modifier.contains(ratatui::style::Modifier::BOLD)),
            "cell {x}"
        );
    }
    assert_eq!(
        buf.cell((6, row)).and_then(|cell| cell.style().fg),
        Some(Role::Attention.color()),
    );
    assert!(
        buf.cell((6, row))
            .map(|cell| cell.style().add_modifier)
            .is_some_and(|modifier| modifier.contains(ratatui::style::Modifier::BOLD)),
    );
    assert_eq!(
        buf.cell((8, row)).and_then(|cell| cell.style().fg),
        Some(Role::Muted.color()),
    );
}

#[test]
fn the_spinner_steps_on_the_tick_in_attention() {
    let (mut app, clock) = home_app(WALL - 674_000);
    let origin = clock.origin();
    // Frame 0's spinner is the first braille cell, frame 1's the second,
    // both in the attention colour.
    for (frame, want) in [(0, SPINNER[0]), (1, SPINNER[1])] {
        app.set_now(
            origin
                .checked_add(Duration::from_millis(frame * 120))
                .expect("after the origin"),
            WALL,
        );
        let buf = buffer_sized(&app, 80, 24);
        let row = working_row(&buf, 80);
        assert_eq!(buf.cell((3, row)).map(|cell| cell.symbol()), Some(want));
        assert_eq!(
            buf.cell((3, row)).and_then(|cell| cell.style().fg),
            Some(Role::Attention.color()),
        );
    }
}

#[test]
fn the_bands_edge_cells_carry_the_bold_centre() {
    let (mut app, clock) = home_app(WALL - 674_000);
    let origin = clock.origin();
    // Frame 0's band is the word's first cell alone, frame 8's its last
    // cell alone: a one-cell band is its own bold centre.
    for (frame, x) in [(0, 5), (8, 11)] {
        app.set_now(
            origin
                .checked_add(Duration::from_millis(frame * 120))
                .expect("after the origin"),
            WALL,
        );
        let buf = buffer_sized(&app, 80, 24);
        let row = working_row(&buf, 80);
        assert_eq!(
            buf.cell((x, row)).and_then(|cell| cell.style().fg),
            Some(Role::Attention.color()),
            "frame {frame}"
        );
        assert!(
            buf.cell((x, row))
                .map(|cell| cell.style().add_modifier)
                .is_some_and(|modifier| modifier.contains(ratatui::style::Modifier::BOLD)),
            "frame {frame}"
        );
    }
}

#[test]
fn the_elapsed_time_and_tail_stay_dim() {
    let (mut app, clock) = home_app(WALL - 674_000);
    app.set_now(clock.origin(), WALL);
    let buf = buffer_sized(&app, 80, 24);
    let row = working_row(&buf, 80);
    // Past the word the line is muted throughout: the elapsed time and
    // the interrupt tail.
    for x in 11..38 {
        assert_eq!(
            buf.cell((x, row)).and_then(|cell| cell.style().fg),
            Some(Role::Muted.color()),
            "cell {x}"
        );
    }
}

/// The working line's screen row in a buffer `width` wide: the row
/// holding the word.
fn working_row(buf: &Buffer, width: u16) -> u16 {
    (0..buf.area.height)
        .find(|y| {
            let row: String = (0..width)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.contains("Working")
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
    // The spinner is still and the word is plain: the spinner keeps
    // its colour as a still ●, no cell of the word carries it, and none
    // is bold.
    let buf = buffer_sized(&app, 80, 24);
    let row = working_row(&buf, 80);
    assert_eq!(buf.cell((3, row)).map(|cell| cell.symbol()), Some("●"));
    assert_eq!(
        buf.cell((3, row)).and_then(|cell| cell.style().fg),
        Some(Role::Attention.color()),
    );
    for x in 0..80 {
        if x != 3 {
            assert_ne!(
                buf.cell((x, row)).and_then(|cell| cell.style().fg),
                Some(Role::Attention.color()),
                "cell {x}"
            );
        }
        assert!(
            !buf.cell((x, row))
                .map(|cell| cell.style().add_modifier)
                .is_some_and(|modifier| modifier.contains(ratatui::style::Modifier::BOLD)),
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
    // The sixteen cells of `Esc to interrupt` on the working line's row.
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
    assert_eq!(text, "Esc to interrupt");
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
    assert_eq!(text(&buf), "  ⠋ Working 11m 14s\n");
}

#[test]
fn a_marked_line_under_new_messages_below_does_not_spin() {
    // One prompt, the running turn, one prompt: at sixteen rows the
    // group line sits on the conversation's bottom row from the top,
    // so new output there covers its mark with the overlay, and
    // following shows it spinning.
    let (mut app, clock) = running_first();
    let origin = clock.origin();
    let now = clock.now();
    app.set_now(origin, 0);
    app.jump(0);
    // New output at the top: the overlay takes the bottom row,
    // covering the group line's mark.
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({"text": "streamed"}),
        Some("a_2"),
    ));
    let area = Rect::new(0, 0, WIDTH, 16);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    assert!(shown.contains("New messages below"), "{shown}");
    // The covered mark schedules nothing: it is the only mark on
    // screen, and with no home there is no working line to ask.
    assert_eq!(app.take_wake(), None);
    // Following again draws it: the mark spins.
    app.on_key(Key::End, now);
    let mut followed = Buffer::empty(area);
    render(&app, area, &mut followed, None);
    let followed = text(&followed);
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
fn a_mark_with_no_drawn_rows_does_not_spin() {
    let (mut app, clock) = running();
    app.set_now(clock.origin(), 0);
    let area = Rect::new(0, 0, 4, 2);
    let mut buf = Buffer::empty(area);
    // Keep the other guards clear so the zero-row comparison is exercised.
    spin(
        &app,
        &mut buf,
        area,
        Drawn {
            y: 0,
            col: 0,
            first_row_shown: true,
            rows: 1,
            count: 0,
        },
    );
    assert!(!text(&buf).contains(SPINNER[0]), "{}", text(&buf));
    assert_eq!(app.take_wake(), None);
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
fn a_mark_at_or_two_past_the_overlay_clamp_does_not_spin() {
    let area = Rect::new(0, 0, 4, 2);
    let last = area.bottom().saturating_sub(1);
    for y in [last, last.saturating_add(2)] {
        let (mut app, clock) = running_first();
        app.on_key(Key::PageUp, clock.now());
        app.on_line(session_line(
            "assistant_message_delta",
            serde_json::json!({"text": "streamed"}),
            Some("a_2"),
        ));
        assert!(app.has_new());
        app.set_now(clock.origin(), 0);
        let mut buf = Buffer::empty(area);
        spin(
            &app,
            &mut buf,
            area,
            Drawn {
                y,
                col: 0,
                first_row_shown: true,
                rows: 1,
                count: 1,
            },
        );
        assert!(!text(&buf).contains(SPINNER[0]), "y={y}: {}", text(&buf));
        assert_eq!(app.take_wake(), None, "y={y}");
    }
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
    assert_eq!((*x, before.as_str()), (1, "●"));
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
fn a_clipped_group_line_off_screen_asks_nothing() {
    let clock = FakeClock::new();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app.attach(contract::SessionId(SESSION.to_owned()));
    // Ten calls in flight: the summary is one row, cut with "…".
    app.on_line(prompt("go".to_owned()));
    app.on_line(session_line("step_started", serde_json::json!({}), None));
    for n in 0..10 {
        app.on_line(session_line(
            "tool_call_requested",
            serde_json::json!({"name": "read", "arguments": {"path": format!("src/file{n:02}.rs")}}),
            Some(format!("a_{n}").as_str()),
        ));
    }
    // Two prompts make the summary scrollable. Find its drawn row so
    // this keeps testing the hidden-line case when card edges add rows.
    for n in 1..=2 {
        app.on_line(prompt(format!("prompt {n}")));
    }
    let all = app.shown(0, usize::MAX);
    let summary = all
        .lines
        .iter()
        .position(|(line, _, _)| line.to_string().starts_with(" ● Read"))
        .expect("a group summary");
    // One row, cut with "…": the tail never draws, while the duration
    // and the toggle keep their columns.
    let text = all
        .lines
        .get(summary)
        .map(|(line, _, _)| line.to_string())
        .expect("a group summary");
    assert!(text.contains('…'), "{text}");
    assert!(text.ends_with("0s ▸"), "{text}");
    assert!(!text.contains("file09.rs"), "{text}");
    let top = all.first.saturating_add(
        all.lines
            .iter()
            .take(summary)
            .map(|(_, rows, _)| *rows)
            .sum::<usize>(),
    );
    app.jump(top.saturating_add(1));
    app.set_now(clock.origin(), 0);
    let shown = screen(&app);
    // The one row scrolled wholly above: nothing spins, nothing asks.
    assert!(!shown.contains(SPINNER[0]), "{shown}");
    assert_eq!(app.take_wake(), None);
    // Its row back on screen spins again.
    app.jump(top);
    let followed = screen(&app);
    assert!(followed.contains(SPINNER[0]), "{followed}");
    assert!(app.take_wake().is_some());
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
fn a_writing_band_spins_its_dot_while_it_survives() {
    // At 60 and at 40 the right part keeps its columns, so the dot
    // survives the cut and spins on the tick: one row, never wrapped.
    for width in [60, 40] {
        let (mut app, clock) = writing_band(width, 12);
        let origin = clock.origin();
        app.set_now(origin, 0);
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        render(&app, area, &mut buf, None);
        let shown = text(&buf);
        let row = shown
            .lines()
            .position(|row| row.contains('⇄'))
            .expect("a band row");
        let row = u16::try_from(row).expect("a row");
        assert!(
            (0..width).any(|col| buf
                .cell((col, row))
                .is_some_and(|cell| cell.symbol() == SPINNER[0])),
            "{width}: {shown}"
        );
        assert!(!shown.contains('●'), "{width}: {shown}");
        assert_eq!(
            app.take_wake(),
            origin.checked_add(Duration::from_millis(120)),
            "{width}"
        );
    }
}

#[test]
fn a_writing_band_cut_past_its_dot_asks_nothing() {
    // At 30, 20 and 12 the right part alone cannot fit, so the whole
    // row is cut past its dot: nothing spins and nothing asks.
    for width in [30, 20, 12] {
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
    // Frame 17 sweeps again from the word's first cell, past the indent
    // and the spinner.
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
        buf.cell((5, row)).and_then(|cell| cell.style().fg),
        Some(crate::theme::Role::Attention.color())
    );
    assert_eq!(
        app.take_wake(),
        origin.checked_add(Duration::from_millis(2_160))
    );
}

#[test]
fn a_cut_band_mark_spins_while_its_dot_survives() {
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
    // One prompt below: two wheel steps up from following shows both
    // band rows at the bottom.
    for n in 1..=1 {
        app.on_line(prompt(format!("after {n}")));
    }
    for _ in 0..2 {
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
    // The band is one row, cut with the right part's columns intact,
    // so the dot survives and spins: no second band row, no still dot.
    let row = shown
        .lines()
        .position(|row| row.contains('⇄'))
        .expect("a band row");
    assert!(
        shown.lines().nth(row).is_some_and(|row| row.contains('…')),
        "{shown}"
    );
    assert!(!shown.contains('●'), "{shown}");
    assert!(shown.contains(SPINNER[0]), "{shown}");
    assert_eq!(
        app.take_wake(),
        clock.origin().checked_add(Duration::from_millis(120))
    );
}

/// A `working_line` renderer recording its inputs: the folded lines it
/// returns with the frame each animated call carries.
#[derive(Debug)]
struct RecordingRenderer {
    lines: Vec<Vec<crate::ext::RawSpan>>,
    interval: Option<Duration>,
    inputs: std::cell::RefCell<Vec<crate::ext::WorkingLine>>,
    frames: std::cell::RefCell<Vec<u64>>,
    nows: std::cell::RefCell<Vec<u64>>,
}

impl RecordingRenderer {
    fn lines(texts: &[&str], interval: Option<Duration>) -> Self {
        Self {
            lines: texts
                .iter()
                .map(|text| {
                    vec![crate::ext::RawSpan {
                        text: (*text).to_owned(),
                        fg: None,
                        bg: None,
                        bold: false,
                        dim: false,
                        italic: false,
                        underline: false,
                        link: None,
                        click: None,
                    }]
                })
                .collect(),
            interval,
            inputs: std::cell::RefCell::new(Vec::new()),
            frames: std::cell::RefCell::new(Vec::new()),
            nows: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn record(&self, input: &crate::ext::Input<'_>) {
        let crate::ext::Input::WorkingLine(line) = input else {
            panic!("a working-line input, got {input:?}");
        };
        self.inputs.borrow_mut().push(line.clone());
    }
}

impl crate::ext::Renderer for RecordingRenderer {
    fn name(&self) -> &str {
        "test"
    }

    fn fills(&self, slot: &str) -> bool {
        slot == "working_line"
    }

    fn animate_interval(&self, _ctx: &crate::ext::SessionCtx, _slot: &str) -> Option<Duration> {
        self.interval
    }

    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        input: &crate::ext::Input<'_>,
        _width: u16,
        _builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> crate::ext::Decision {
        self.record(input);
        crate::ext::Decision::Draw {
            extension: "test".into(),
            lines: self.lines.clone(),
        }
    }

    fn render_animated(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        input: &crate::ext::Input<'_>,
        _width: u16,
        frame: u64,
        now: u64,
        _builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> (crate::ext::Decision, bool) {
        self.record(input);
        self.frames.borrow_mut().push(frame);
        self.nows.borrow_mut().push(now);
        (
            crate::ext::Decision::Draw {
                extension: "test".into(),
                lines: self.lines.clone(),
            },
            true,
        )
    }
}

/// Draws `app`'s working line alone in `area`: the rows' text and the
/// click targets it leaves.
fn draw_line(app: &App, width: u16, height: u16) -> (String, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw(app, area, &mut buf, &mut bottom, &mut targets);
    (text(&buf), targets)
}

#[test]
fn a_replacement_keeps_an_interrupt_target_on_every_line() {
    let (mut app, clock) = home_app(WALL - 674_000);
    app.set_now(clock.origin(), WALL);
    let fake = std::rc::Rc::new(RecordingRenderer::lines(
        &["custom one", "custom two"],
        None,
    ));
    app.install_renderer(fake.clone());
    let (shown, targets) = draw_line(&app, 80, 2);
    assert!(shown.contains("custom one"), "{shown}");
    assert!(shown.contains("custom two"), "{shown}");
    let interrupts: Vec<_> = targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::Interrupt)
        .collect();
    // One interrupt target over each replacement line's full width.
    // docs/tui.md:2880-2883: a replacement keeps one interrupt target per line.
    assert_eq!(interrupts.len(), 2, "{targets:?}"); // docs/tui.md:2880-2883.
    for (dy, target) in interrupts.iter().enumerate() {
        assert_eq!(
            (target.rect.x, target.rect.width, target.rect.height),
            (0, 80, 1),
            "{target:?}"
        );
        assert_eq!(usize::from(target.rect.y), dy, "{target:?}");
    }
}

#[test]
fn an_empty_replacement_hides_the_line() {
    let (mut app, clock) = home_app(WALL - 674_000);
    app.set_now(clock.origin(), WALL);
    let fake = std::rc::Rc::new(RecordingRenderer::lines(&[], None));
    app.install_renderer(fake);
    // No rows reserved and none drawn, and no target left behind.
    // docs/tui.md:2880: a replacement that returns no lines hides the line.
    assert_eq!(super::count(&app, 80), 0); // docs/tui.md:2880.
    let (shown, targets) = draw_line(&app, 80, 1);
    assert!(!shown.contains("Working"), "{shown}");
    assert!(
        targets
            .iter()
            .all(|target| target.id != crate::mouse::TargetId::Interrupt),
        "{targets:?}"
    );
    // The layout drops the hidden line's row: without a renderer the
    // same app reserves one row more.
    let (mut plain, plain_clock) = home_app(WALL - 674_000);
    plain.set_now(plain_clock.origin(), WALL);
    // docs/tui.md:2880: the hidden line reserves no row.
    assert_eq!(app.below_rows() + 1, plain.below_rows()); // docs/tui.md:2880.
}

#[test]
fn the_fold_matches_the_turn_cards_prompts_when_narrowed() {
    for width in [80u16, 20] {
        let (mut app, clock) = home_app(WALL - 674_000);
        app.set_now(clock.origin(), WALL);
        let fake = std::rc::Rc::new(RecordingRenderer::lines(&["custom"], None));
        app.install_renderer(fake.clone());
        let (shown, _) = draw_line(&app, width, 1);
        assert!(shown.contains("custom"), "{shown}");
        // The input carries the turn card's start, however narrow.
        // docs/tui.md:2872-2878: working_line draws the working line with started.
        let inputs = fake.inputs.borrow();
        assert_eq!(inputs.len(), 1, "one folded input"); // docs/tui.md:3044: one call on the miss; 2872-2878: working_line fields.
        assert_eq!(inputs[0].started, Some(WALL - 674_000));
        assert!(inputs[0].retry.is_none());
    }
}

#[test]
fn the_fold_maps_the_retry_schedule() {
    let (mut app, clock) = home_app(WALL);
    app.on_line(session_line("step_started", serde_json::json!({}), None));
    app.on_line(session_line(
        "assistant_message_started",
        serde_json::json!({}),
        Some("a_m"),
    ));
    app.on_line(session_line_at(
        "retry_scheduled",
        serde_json::json!({"code": "rate_limited", "attempt": 1,
            "delay_ms": 4_000, "last_attempt": 4}),
        Some("a_m"),
        WALL,
    ));
    app.set_now(clock.origin(), WALL);
    let fake = std::rc::Rc::new(RecordingRenderer::lines(&["custom"], None));
    app.install_renderer(fake.clone());
    let (shown, _) = draw_line(&app, 80, 1);
    assert!(shown.contains("custom"), "{shown}");
    let inputs = fake.inputs.borrow();
    // docs/tui.md:2872-2878: the retry input carries code, attempt and schedule.
    assert_eq!(inputs.len(), 1, "one folded input"); // docs/tui.md:3044: one call on the miss.
    assert_eq!(inputs[0].started, Some(WALL));
    let retry = inputs[0].retry.as_ref().expect("a pending retry");
    assert!(matches!(retry.code, contract::ErrorCode::RateLimited));
    // The attempt about to be made counts the call that failed, as the
    // built-in's `attempt 2 of 4` does.
    assert_eq!(retry.attempt, 2);
    assert_eq!(retry.last_attempt, 4);
    assert_eq!(retry.delay_ms, 4_000);
    assert_eq!(retry.ts, WALL);
}

#[test]
fn an_animated_replacement_records_frame_and_now() {
    let (mut app, clock) = home_app(WALL - 674_000);
    app.set_now(clock.origin(), WALL);
    let fake = std::rc::Rc::new(RecordingRenderer::lines(
        &["custom"],
        Some(Duration::from_millis(500)),
    ));
    app.install_renderer(fake.clone());
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    assert!(text(&buf).contains("custom"), "{}", text(&buf));
    // docs/tui.md:3121: every animated slot carries frame and now.
    assert_eq!(fake.frames.borrow().clone(), vec![0]); // docs/tui.md:3044: one call on the miss; 3121: frame in input.
    assert_eq!(fake.nows.borrow().clone(), vec![WALL]); // docs/tui.md:3044: one call on the miss; 3121: now in input.
}
