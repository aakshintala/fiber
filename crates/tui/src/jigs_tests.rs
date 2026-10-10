//! Tests for the `open` jig: replaying an events log through the
//! terminal's own fold and draw, split into stages (`measure_open`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use contract::Envelope;
use serde_json::json;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// One session line of `kind`, serialized as the jig reads it.
fn event(kind: &str) -> String {
    let envelope = Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::Map::new(),
    };
    serde_json::to_string(&envelope).unwrap_or_default()
}

/// `n` session lines, one envelope per line.
fn events(n: usize) -> String {
    (0..n)
        .map(|_| event("turn_started"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A clock that ticks one millisecond per `now()`, so the stages hold
/// nonzero durations. The origin is the fake clock's, since reading the
/// process clock is banned in tests.
struct TickClock {
    origin: std::time::Instant,
    ticks: AtomicU64,
}

impl TickClock {
    fn clock(origin: std::time::Instant) -> Arc<Self> {
        Arc::new(Self {
            origin,
            ticks: AtomicU64::new(0),
        })
    }
}

impl contract::clock::Clock for TickClock {
    fn now(&self) -> std::time::Instant {
        let ticks = self.ticks.fetch_add(1, Ordering::SeqCst);
        self.origin
            .checked_add(Duration::from_millis(ticks))
            .unwrap_or(self.origin)
    }

    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _d: Duration) {}

    fn wait_until(
        &self,
        _until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        wait(Some(Duration::ZERO));
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

#[test]
fn measure_open_counts_the_lines_and_draws_one_frame_for_usize_max() {
    let clock = fakes::clock::FakeClock::new();
    let stages = crate::measure_open(&events(5), 60, 12, usize::MAX, clock)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(stages.lines, 5);
    assert_eq!(stages.frames, 1);
    assert!(stages.parse.is_zero());
    assert!(stages.fold.is_zero());
    assert!(stages.frame_time.is_zero());
    // One line is one frame with the same setting.
    let clock = fakes::clock::FakeClock::new();
    let one = crate::measure_open(&events(1), 60, 12, usize::MAX, clock)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!((one.lines, one.frames), (1, 1));
}

#[test]
fn measure_open_draws_after_every_k_lines_and_once_at_the_end() {
    // K = 3: K-1 lines draw only the final frame, K lines draw the one
    // periodic frame as the final one, K+1 lines draw one periodic frame
    // and the final one.
    for (lines, frames) in [(2, 1), (3, 1), (4, 2)] {
        let clock = fakes::clock::FakeClock::new();
        let stages = crate::measure_open(&events(lines), 60, 12, 3, clock)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(stages.lines, lines);
        assert_eq!(stages.frames, frames, "for {lines} lines at every 3");
    }
    // Every line draws at 1, and an empty log still draws its final frame.
    let clock = fakes::clock::FakeClock::new();
    let every =
        crate::measure_open(&events(4), 60, 12, 1, clock).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(every.frames, 4);
    assert_ne!(every.frames, 5);
    let clock = fakes::clock::FakeClock::new();
    let empty = crate::measure_open("", 60, 12, 3, clock).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!((empty.lines, empty.frames), (0, 1));
}

#[test]
fn measure_open_skips_blank_lines_without_counting_them() {
    let clock = fakes::clock::FakeClock::new();
    let stages = crate::measure_open(
        &format!(
            "{}\n\n   \n{}",
            event("turn_started"),
            event("turn_started")
        ),
        60,
        12,
        usize::MAX,
        clock,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!((stages.lines, stages.frames), (2, 1));
}

#[test]
fn measure_open_names_the_unreadable_line() {
    let clock = fakes::clock::FakeClock::new();
    let error = crate::measure_open(
        &format!("{}\nnot json", events(1)),
        60,
        12,
        usize::MAX,
        clock,
    );
    assert!(
        matches!(&error, Err(error) if error.starts_with("line 2:")),
        "{error:?}"
    );
    // The first line failing names line 1, not line 0.
    let clock = fakes::clock::FakeClock::new();
    let error = crate::measure_open("not json", 60, 12, usize::MAX, clock);
    assert!(
        matches!(&error, Err(error) if error.starts_with("line 1:")),
        "{error:?}"
    );
}

#[test]
fn measure_open_times_each_stage_on_a_ticking_clock() {
    let origin = fakes::clock::FakeClock::new().origin();
    // Two lines at every 1: two periodic frames, no extra final one. Each
    // stage reads the clock twice per step, one tick apart, so each holds
    // one millisecond per step.
    let stages = crate::measure_open(&events(2), 60, 12, 1, TickClock::clock(origin))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(stages.lines, 2);
    assert_eq!(stages.frames, 2);
    assert_eq!(stages.parse, Duration::from_millis(2));
    assert_eq!(stages.fold, Duration::from_millis(2));
    assert_eq!(stages.frame_time, Duration::from_millis(2));
    // A frozen clock holds zeros, not the tick counts above.
    let clock = fakes::clock::FakeClock::new();
    let frozen =
        crate::measure_open(&events(2), 60, 12, 1, clock).unwrap_or_else(|error| panic!("{error}"));
    assert_ne!(frozen.parse, Duration::from_millis(2));
    assert!(frozen.frame_time.is_zero());
}

#[test]
fn measure_open_reports_no_stages_for_an_empty_log_but_still_draws() {
    let origin = fakes::clock::FakeClock::new().origin();
    let stages = crate::measure_open("", 60, 12, usize::MAX, TickClock::clock(origin))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(stages.lines, 0);
    assert_eq!(stages.frames, 1);
    assert!(stages.parse.is_zero());
    assert!(stages.fold.is_zero());
    assert_eq!(stages.frame_time, Duration::from_millis(1));
    assert_eq!(
        json!({"lines": stages.lines, "frames": stages.frames}),
        json!({"lines": 0, "frames": 1})
    );
}
