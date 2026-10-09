//! Tests for the frame clock: frames, the spinner, the asks and the one
//! still cell, on fake origins (no process clock reads).

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Motion, SPINNER, STILL};

/// A motion whose epoch is `origin`: set once, so the frame counts from
/// it.
fn motion(origin: std::time::Instant) -> Motion {
    let mut motion = Motion::default();
    motion.set_now(origin, 0);
    motion
}

#[test]
fn frame_counts_120_ms_steps_from_the_epoch() {
    let origin = fakes::clock::FakeClock::new().origin();
    let at = |millis: u64| {
        let mut motion = motion(origin);
        motion.set_now(
            origin
                .checked_add(Duration::from_millis(millis))
                .expect("after the origin"),
            millis,
        );
        motion.spinner()
    };
    // Frames 0, 0, 1, 1, 1, 2, 2: the 119/120 and 239/240 edges, and past
    // the epoch only (later sets never move it).
    assert_eq!(at(0), SPINNER[0]);
    assert_eq!(at(119), SPINNER[0]);
    assert_eq!(at(120), SPINNER[1]);
    assert_eq!(at(122), SPINNER[1]);
    assert_eq!(at(239), SPINNER[1]);
    assert_eq!(at(240), SPINNER[2]);
    assert_eq!(at(242), SPINNER[2]);
}

#[test]
fn spinner_cycles_ten_frames() {
    let origin = fakes::clock::FakeClock::new().origin();
    let at = |frame: u64| {
        let mut motion = motion(origin);
        motion.set_now(
            origin
                .checked_add(Duration::from_millis(frame * 120))
                .expect("after the origin"),
            0,
        );
        motion.spinner().to_owned()
    };
    // `% 10`: frame 10 is frame 0 again, frame 12 is frame 2.
    assert_eq!(at(0), SPINNER[0]);
    assert_eq!(at(9), SPINNER[9]);
    assert_eq!(at(10), SPINNER[0]);
    assert_eq!(at(12), SPINNER[2]);
}

#[test]
fn reduced_or_no_now_is_still_and_asks_nothing() {
    let origin = fakes::clock::FakeClock::new().origin();
    // No `now` at all: still, and no ask.
    let still = Motion::default();
    assert_eq!(still.spinner(), STILL);
    still.ask_frame();
    assert_eq!(still.take_wake(), None);
    // Reduced: still, and no ask, even with time set.
    let mut reduced = motion(origin);
    reduced.set_reduced(true);
    assert_eq!(reduced.spinner(), STILL);
    reduced.ask_frame();
    assert_eq!(reduced.take_wake(), None);
    // A spin under reduced motion leaves the cell as drawn.
    let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
    buf.set_string(0, 0, "• ok", ratatui::style::Style::default());
    reduced.spin(&mut buf, 0, 0);
    assert_eq!(crate::view::text(&buf), "• ok\n");
    assert_eq!(reduced.take_wake(), None);
    // A spin with no time leaves the cell as drawn.
    still.spin(&mut buf, 0, 0);
    assert_eq!(crate::view::text(&buf), "• ok\n");
}

#[test]
fn ask_frame_asks_for_the_next_boundary() {
    let origin = fakes::clock::FakeClock::new().origin();
    // Frame 0 asks for epoch + 120 ms exactly.
    let live = motion(origin);
    live.ask_frame();
    assert_eq!(
        live.take_wake(),
        origin.checked_add(Duration::from_millis(120))
    );
    // Frame 3 asks for epoch + 480 ms exactly.
    let mut third = motion(origin);
    third.set_now(
        origin
            .checked_add(Duration::from_millis(361))
            .expect("after the origin"),
        0,
    );
    third.ask_frame();
    assert_eq!(
        third.take_wake(),
        origin.checked_add(Duration::from_millis(480))
    );
}

#[test]
fn the_earliest_ask_wins_and_take_clears_it() {
    let origin = fakes::clock::FakeClock::new().origin();
    let mut live = motion(origin);
    live.ask_frame();
    // Repeating the same ask has no observable effect.
    live.ask_frame();
    // Time moves on: the next ask is later, and the earlier one wins.
    live.set_now(
        origin
            .checked_add(Duration::from_millis(120))
            .expect("after the origin"),
        0,
    );
    live.ask_frame();
    assert_eq!(
        live.take_wake(),
        origin.checked_add(Duration::from_millis(120))
    );
    // Taking leaves none behind.
    assert_eq!(live.take_wake(), None);
}

#[test]
fn clear_wake_drops_an_ask() {
    let origin = fakes::clock::FakeClock::new().origin();
    let live = motion(origin);
    live.ask_frame();
    assert!(live.take_wake().is_some());
    live.ask_frame();
    live.clear_wake();
    assert_eq!(live.take_wake(), None);
}

#[test]
fn ask_wall_converts_and_asks_under_reduced_motion() {
    let origin = fakes::clock::FakeClock::new().origin();
    let wall = 1_700_000_000_000;
    let at = |motion: &Motion| motion.take_wake();
    // A moment past converts against the frame's wall time.
    let mut live = Motion::default();
    live.set_now(origin, wall);
    live.ask_wall(wall + 2);
    assert_eq!(at(&live), origin.checked_add(Duration::from_millis(2)));
    // At and before the wall ask at once.
    live.ask_wall(wall);
    assert_eq!(at(&live), Some(origin));
    live.ask_wall(wall.saturating_sub(1));
    assert_eq!(at(&live), Some(origin));
    // Reduced motion still asks: the countdown keeps its wake.
    let mut reduced = Motion::default();
    reduced.set_reduced(true);
    reduced.set_now(origin, wall);
    reduced.ask_wall(wall + 1_000);
    assert_eq!(
        at(&reduced),
        origin.checked_add(Duration::from_millis(1_000))
    );
    // With no time there is nothing to convert against.
    let bare = Motion::default();
    bare.ask_wall(wall + 1_000);
    assert_eq!(at(&bare), None);
}

#[test]
fn spin_writes_one_cell_and_keeps_its_style() {
    let origin = fakes::clock::FakeClock::new().origin();
    let live = motion(origin);
    let style = ratatui::style::Style::new().fg(ratatui::style::Color::Red);
    let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
    buf.set_string(0, 0, "• ok", style);
    live.spin(&mut buf, 0, 0);
    // Frame 0's glyph, in the cell's own style; the rest untouched.
    assert_eq!(buf.cell((0, 0)).map(|cell| cell.symbol()), Some(SPINNER[0]));
    assert_eq!(
        buf.cell((0, 0)).map(|cell| cell.fg),
        Some(ratatui::style::Color::Red)
    );
    assert_eq!(crate::view::text(&buf), format!("{} ok\n", SPINNER[0]));
    // The write asked for the next boundary.
    assert_eq!(
        live.take_wake(),
        origin.checked_add(Duration::from_millis(120))
    );
}

/// A live feed row in `state` since `since_ms`.
fn waiting_row(since_ms: u64) -> crate::home::Row {
    let payload = serde_json::json!({
        "name": "work", "workspace": "/w", "project": "-w",
        "state": "waiting",
        "waiting": {"request_id": "r_1", "kind": "approval", "summary": "shell"},
        "since": since_ms,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    crate::home::from_status(&contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId("s_x".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

#[test]
fn glyph_spins_only_live_working_and_retrying_rows() {
    use crate::home::{Left, State};
    use crate::motion::Motion;
    let origin = fakes::clock::FakeClock::new().origin();
    let mut live = Motion::default();
    live.set_now(origin, 0);
    let row = |state: State, left: Option<Left>| {
        let mut row = waiting_row(0);
        row.state = state;
        row.left = left;
        row
    };
    // Live working rows spin; every other live row holds its glyph, and
    // so does a working row that left.
    assert_eq!(live.glyph(&row(State::Working, None)), SPINNER[0]);
    assert_eq!(live.glyph(&row(State::Retrying, None)), SPINNER[0]);
    assert!(Motion::spins(&row(State::Working, None)));
    assert!(Motion::spins(&row(State::Retrying, None)));
    for (state, glyph) in [
        (State::Waiting, "!"),
        (State::Jobs, "●"),
        (State::Idle, "✓"),
    ] {
        assert_eq!(live.glyph(&row(state, None)), glyph);
        assert!(!Motion::spins(&row(state, None)));
    }
    for left in [Some(Left::Exited), Some(Left::Crashed)] {
        for state in [State::Working, State::Retrying, State::Waiting] {
            assert!(!Motion::spins(&row(state, left)));
        }
    }
    assert_eq!(live.glyph(&row(State::Working, Some(Left::Exited))), "○");
    assert_eq!(live.glyph(&row(State::Working, Some(Left::Crashed))), "✗");
    // Under reduced motion and with no time the spinner is still.
    let mut reduced = Motion::default();
    reduced.set_reduced(true);
    reduced.set_now(origin, 0);
    assert_eq!(reduced.glyph(&row(State::Working, None)), STILL);
    assert_eq!(Motion::default().glyph(&row(State::Working, None)), STILL);
}

#[test]
fn pulse_runs_ten_seconds_from_since() {
    use crate::motion::PULSE_MS;
    let origin = fakes::clock::FakeClock::new().origin();
    let wall = 1_700_000_000_000;
    let at = |wall_ms: u64, frame_ms: u64| {
        let mut motion = Motion::default();
        motion.set_now(origin, wall);
        motion.set_now(
            origin
                .checked_add(Duration::from_millis(frame_ms))
                .expect("after the origin"),
            wall_ms,
        );
        motion.pulse(wall)
    };
    // Bright while `(frame / 4) % 2 == 0`: frames 3 and 4 differ.
    assert_eq!(at(wall, 0), Some(true));
    assert_eq!(at(wall + 479, 479), Some(true));
    assert_eq!(at(wall + 480, 480), Some(false));
    assert_eq!(at(wall + 9_999, 9_999), Some(true));
    // Ten seconds and past hold still.
    assert_eq!(at(wall + 10_000, 10_000), None);
    assert_eq!(at(wall + 10_002, 10_002), None);
    // A `since` after now counts as no wait.
    let mut motion = Motion::default();
    motion.set_now(origin, wall);
    assert_eq!(motion.pulse(wall + 5_000), Some(true));
    assert_eq!(PULSE_MS, 10_000);
}

#[test]
fn ask_pulse_asks_the_next_four_tick_boundary_then_the_end() {
    let origin = fakes::clock::FakeClock::new().origin();
    let wall = 1_700_000_000_000;
    // Waiting since the wall time: the next four-frame boundary is
    // asked, the end ten seconds out is later.
    let mut motion = Motion::default();
    motion.set_now(origin, wall);
    motion.ask_pulse(&waiting_row(wall));
    assert_eq!(
        motion.take_wake(),
        origin.checked_add(Duration::from_millis(480))
    );
    // At 9 990 ms the end is nearer: `since + 10 000`.
    let mut motion = Motion::default();
    motion.set_now(origin, wall);
    motion.set_now(origin, wall + 9_990);
    motion.ask_pulse(&waiting_row(wall));
    assert_eq!(
        motion.take_wake(),
        origin.checked_add(Duration::from_millis(10))
    );
    // Past the end nothing is asked.
    let mut motion = Motion::default();
    motion.set_now(origin, wall);
    motion.set_now(origin, wall + 10_000);
    motion.ask_pulse(&waiting_row(wall));
    assert_eq!(motion.take_wake(), None);
}

#[test]
fn ask_pulse_uses_tick_count_not_elapsed_milliseconds_as_frame() {
    let origin = fakes::clock::FakeClock::new().origin();
    let wall = 1_700_000_000_000;
    let mut motion = Motion::default();
    motion.set_now(origin, wall);
    // At 120 ms, elapsed time is one frame. Dividing by TICK asks at
    // frame 4 (480 ms); multiplying would defer the ask to the pulse end.
    motion.set_now(
        origin
            .checked_add(Duration::from_millis(120))
            .expect("after the origin"),
        wall + 120,
    );
    motion.ask_pulse(&waiting_row(wall));
    assert_eq!(
        motion.take_wake(),
        origin.checked_add(Duration::from_millis(480))
    );
}

#[test]
fn ask_pulse_rounds_up_to_the_next_four_frame_boundary() {
    let origin = fakes::clock::FakeClock::new().origin();
    let wall = 1_700_000_000_000;
    let mut motion = Motion::default();
    motion.set_now(origin, wall);
    // At frame 1, the next four-frame boundary is frame 4 (480 ms).
    // Multiplying by four would ask at frame 20 (2,400 ms).
    motion.set_now(
        origin
            .checked_add(Duration::from_millis(120))
            .expect("after the origin"),
        wall + 120,
    );
    motion.ask_pulse(&waiting_row(wall));
    assert_eq!(
        motion.take_wake(),
        origin.checked_add(Duration::from_millis(480))
    );
}

#[test]
fn glimmer_clipped_at_its_start_returns_no_empty_band() {
    let origin = fakes::clock::FakeClock::new().origin();
    let mut motion = motion(origin);
    // Frame 4 starts the band at cell 2. A two-cell word clamps its end
    // to cell 2, so the empty intersection is None, not Some(2..2).
    motion.set_now(
        origin
            .checked_add(Duration::from_millis(480))
            .expect("after the origin"),
        0,
    );
    assert_eq!(motion.glimmer(2), None);
}

#[test]
fn reduced_never_pulses() {
    let origin = fakes::clock::FakeClock::new().origin();
    let wall = 1_700_000_000_000;
    let mut reduced = Motion::default();
    reduced.set_reduced(true);
    reduced.set_now(origin, wall);
    assert_eq!(reduced.pulse(wall), None);
    reduced.ask_pulse(&waiting_row(wall));
    assert_eq!(reduced.take_wake(), None);
}
