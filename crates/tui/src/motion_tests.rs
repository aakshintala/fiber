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
