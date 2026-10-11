//! Tests for the working line's text: the elapsed time, the shedding,
//! the retry countdown and the moment each next changes, all pure.

use std::time::Duration;

use contract::events::RetryScheduled;
use fakes::clock::FakeClock;

use super::{Working, lay};
use crate::motion::Motion;
use crate::turn::PendingRetry;

/// A working line running since `started_ms`.
fn working(started_ms: Option<u64>) -> Working {
    Working {
        started_ms,
        retry: None,
    }
}

/// A pending retry: scheduled at `ts`, waiting `delay_ms`, attempt
/// `attempt` of `last_attempt`.
fn retry(ts: u64, attempt: u32, delay_ms: u64, last_attempt: u32) -> Working {
    Working {
        started_ms: None,
        retry: Some(PendingRetry {
            retry: RetryScheduled {
                code: contract::ErrorCode::RateLimited,
                attempt: 1,
                last_attempt,
                delay_ms,
            },
            attempt,
            ts,
        }),
    }
}

#[test]
fn text_is_working_elapsed_and_the_interrupt() {
    let started = 1_000_000;
    for (elapsed_ms, elapsed) in [
        (0, "0s"),
        (59_000, "59s"),
        (60_000, "1m 00s"),
        (3_600_000, "1h 00m"),
    ] {
        let laid = lay(&working(Some(started)), Some(started + elapsed_ms), "⠋", 80);
        assert_eq!(
            laid.text,
            format!("  ⠋ Working {elapsed} · Esc to interrupt"),
            "{elapsed_ms}ms"
        );
        assert!(!laid.retrying);
        assert_eq!(laid.word, Some(4..11));
        assert_eq!(laid.spinner, Some(2));
    }
}

#[test]
fn the_spinner_travels_with_the_line() {
    let started = 1_000_000;
    // The spinner's cell rides in the laid text: the word, the spinner
    // cell and the interrupt move with it, whatever it shows.
    let live = lay(&working(Some(started)), Some(started + 5_000), "⠋", 80);
    let still = lay(&working(Some(started)), Some(started + 5_000), "●", 80);
    assert_eq!(live.text, "  ⠋ Working 5s · Esc to interrupt");
    assert_eq!(still.text, "  ● Working 5s · Esc to interrupt");
    assert_eq!(live.word, still.word);
    assert_eq!(live.spinner, still.spinner);
    assert_eq!(live.interrupt, still.interrupt);
}

#[test]
fn without_now_or_start_there_is_no_time() {
    let started = 1_000_000;
    for now in [None, Some(started + 5_000)] {
        let laid = lay(&working(None), now, "⠋", 80);
        assert_eq!(laid.text, "  ⠋ Working · Esc to interrupt");
        assert_eq!(laid.next_ms, None);
    }
    let laid = lay(&working(Some(started)), None, "⠋", 80);
    assert_eq!(laid.text, "  ⠋ Working · Esc to interrupt");
    assert_eq!(laid.next_ms, None);
    // The interrupt target covers its cells while it shows.
    assert_eq!(laid.interrupt, Some(14..30));
}

#[test]
fn detail_sheds_interrupt_then_time_then_cuts_the_word() {
    // Elapsed 11m 14s: the full line is 38 columns.
    let started = 1_000_000;
    let now = started + 674_000;
    let laid = lay(&working(Some(started)), Some(now), "⠋", 80);
    assert_eq!(laid.text, "  ⠋ Working 11m 14s · Esc to interrupt");
    // One below the full line sheds the interrupt with its target.
    for width in [38, 37, 36] {
        let laid = lay(&working(Some(started)), Some(now), "⠋", width);
        if width == 38 {
            assert_eq!(laid.text, "  ⠋ Working 11m 14s · Esc to interrupt");
            assert_eq!(laid.interrupt, Some(22..38));
        } else {
            assert_eq!(laid.text, "  ⠋ Working 11m 14s", "{width}");
            assert_eq!(laid.interrupt, None, "{width}");
        }
    }
    // The line without the tail is 19 wide: at it the time shows, one
    // below sheds it.
    for (width, text) in [
        (19, "  ⠋ Working 11m 14s"),
        (18, "  ⠋ Working"),
        (11, "  ⠋ Working"),
    ] {
        let laid = lay(&working(Some(started)), Some(now), "⠋", width);
        assert_eq!(laid.text, text, "{width}");
        assert_eq!(laid.interrupt, None, "{width}");
    }
    // The bare line is 11 wide; below it the word is cut last, keeping
    // the indent and the spinner while they fit.
    for (width, text) in [(11, "  ⠋ Working"), (10, "  ⠋ Workin"), (4, "  ⠋ ")] {
        let laid = lay(&working(Some(started)), Some(now), "⠋", width);
        assert_eq!(laid.text, text, "{width}");
    }
}

#[test]
fn the_spinner_sheds_with_its_cell() {
    let started = 1_000_000;
    let now = started + 674_000;
    // At three columns the spinner drew whole; at two it is cut, and the
    // word's range is empty either way.
    let laid = lay(&working(Some(started)), Some(now), "⠋", 3);
    assert_eq!(laid.text, "  ⠋");
    assert_eq!(laid.spinner, Some(2));
    assert_eq!(laid.word, Some(4..4));
    let laid = lay(&working(Some(started)), Some(now), "⠋", 2);
    assert_eq!(laid.text, "  ");
    assert_eq!(laid.spinner, None);
    assert_eq!(laid.word, Some(4..4));
}

#[test]
fn the_retry_form_counts_down_and_floors_at_zero() {
    let ts = 1_000_000;
    for (at_ms, secs) in [
        (0, "4s"),
        (1, "3s"),
        (1_000, "3s"),
        (1_001, "2s"),
        (2_001, "1s"),
        (3_000, "1s"),
        (3_001, "0s"),
        (3_003, "0s"),
        (9_000, "0s"),
    ] {
        let laid = lay(&retry(ts, 2, 3_001, 4), Some(ts + at_ms), "⠋", 80);
        assert_eq!(
            laid.text,
            format!("  ↻ Retrying in {secs} · rate_limited · attempt 2 of 4"),
            "+{at_ms}ms"
        );
        assert!(laid.retrying);
        assert_eq!(laid.word, None);
        assert_eq!(laid.spinner, None);
        assert_eq!(laid.interrupt, None);
    }
    // With no time the countdown reads the delay alone.
    let laid = lay(&retry(ts, 2, 3_001, 4), None, "⠋", 80);
    assert_eq!(
        laid.text,
        "  ↻ Retrying in 4s · rate_limited · attempt 2 of 4"
    );
    assert_eq!(laid.next_ms, None);
}

#[test]
fn the_retry_form_keeps_the_indent_when_cut() {
    let ts = 1_000_000;
    // Cut at ten columns the form keeps its indent: the cut never eats
    // the line's start.
    let laid = lay(&retry(ts, 2, 3_001, 4), Some(ts), "⠋", 10);
    assert_eq!(laid.text, "  ↻ Retryi");
    assert!(laid.retrying);
    assert_eq!(laid.word, None);
    assert_eq!(laid.spinner, None);
}

#[test]
fn the_line_asks_for_the_next_second() {
    let started = 1_000_000;
    // Elapsed 11 999 ms: the text next changes at started + 12 000.
    let laid = lay(&working(Some(started)), Some(started + 11_999), "⠋", 80);
    assert_eq!(laid.next_ms, Some(started + 12_000));
    // On the second exactly, the next change is a second later.
    let laid = lay(&working(Some(started)), Some(started + 12_000), "⠋", 80);
    assert_eq!(laid.next_ms, Some(started + 13_000));
}

#[test]
fn the_retry_form_asks_when_its_seconds_drop() {
    let ts = 1_000_000;
    // At 4s the next change is when 3s shows: ts + delay − 3 000.
    let laid = lay(&retry(ts, 2, 3_001, 4), Some(ts), "⠋", 80);
    assert_eq!(laid.next_ms, Some(ts + 1));
    // At 0s nothing is asked for.
    let laid = lay(&retry(ts, 2, 3_001, 4), Some(ts + 3_001), "⠋", 80);
    assert_eq!(laid.next_ms, None);
}

#[test]
fn the_glimmer_band_sweeps_then_rests() {
    let origin = FakeClock::new().origin();
    let at_frame = |frame: u64| {
        let mut motion = Motion::default();
        motion.set_now(origin, 0);
        motion.set_now(
            origin
                .checked_add(Duration::from_millis(frame * 120))
                .expect("after the origin"),
            0,
        );
        motion.glimmer(7)
    };
    // The first cell runs from −2 to 6, one cell a frame, clipped to the
    // seven-cell word; then eight frames rest.
    let want: [(u64, Option<(usize, usize)>); 20] = [
        (0, Some((0, 1))),
        (1, Some((0, 2))),
        (2, Some((0, 3))),
        (3, Some((1, 4))),
        (4, Some((2, 5))),
        (5, Some((3, 6))),
        (6, Some((4, 7))),
        (7, Some((5, 7))),
        (8, Some((6, 7))),
        (9, None),
        (10, None),
        (11, None),
        (12, None),
        (13, None),
        (14, None),
        (15, None),
        (16, None),
        (17, Some((0, 1))),
        (18, Some((0, 2))),
        (19, Some((0, 3))),
    ];
    for (frame, band) in want {
        assert_eq!(
            at_frame(frame).map(|range| (range.start, range.end)),
            band,
            "frame {frame}"
        );
    }
    // Reduced motion draws no glimmer.
    let mut reduced = Motion::default();
    reduced.set_reduced(true);
    reduced.set_now(origin, 0);
    assert_eq!(reduced.glimmer(7), None);
}
