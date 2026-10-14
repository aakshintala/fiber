//! Observation state tests: the record's milestones count from the send,
//! the first token latches once, the clock never runs backwards and every
//! conversion saturates. Transport milestones and the recorded target are
//! covered in `http_tests.rs`; the wire, in `tests/diagnostics.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::diag::{DebugLog, ProviderRequest};

use super::*;

/// Captures the lines a record writes.
struct Capture {
    debugging: bool,
    lines: Mutex<Vec<ProviderRequest>>,
}

impl Capture {
    fn new(debugging: bool) -> Arc<Self> {
        Arc::new(Self {
            debugging,
            lines: Mutex::new(Vec::new()),
        })
    }

    fn lines(&self) -> Vec<ProviderRequest> {
        self.lines.lock().unwrap().clone()
    }
}

impl DebugLog for Capture {
    fn debugging(&self) -> bool {
        self.debugging
    }

    fn provider_request(&self, request: &ProviderRequest) {
        self.lines.lock().unwrap().push(request.clone());
    }
}

fn observation(
    log: &Arc<Capture>,
    clock: &Arc<fakes::clock::FakeClock>,
    attempt: u32,
) -> RequestObservation {
    RequestObservation::new(
        Arc::clone(log) as Arc<dyn DebugLog>,
        Arc::clone(clock) as Arc<dyn contract::clock::Clock>,
        NonZeroU32::new(attempt).unwrap(),
    )
    .with_model("m".to_owned())
}

fn record(
    log: &Arc<Capture>,
    clock: &Arc<fakes::clock::FakeClock>,
    start: std::time::Instant,
) -> ObservationFinish {
    ObservationFinish::new(
        &observation(log, clock, 2),
        "fake",
        "http://host/v1",
        123,
        start,
    )
}

#[test]
fn milestones_count_from_the_send_not_from_pre_send_work() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    // Work before the send, such as signing, is in no duration.
    clock.advance(Duration::from_millis(30));
    let mut finish = record(&log, &clock, clock.now());
    clock.advance(Duration::from_millis(5));
    finish.sent(clock.now(), 200);
    clock.advance(Duration::from_millis(7));
    finish.latch_first_token();
    // A later delta never overwrites the first stamp.
    clock.advance(Duration::from_millis(17));
    finish.latch_first_token();
    finish.set_bytes(91);
    clock.advance(Duration::from_millis(20));
    finish.finish();
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert_eq!(line.headers_ms, Some(5), "not 35");
    assert_eq!(line.first_token_ms, Some(12), "not 42, 50 or 17");
    assert_eq!(line.total_ms, 49, "5 + 7 + 17 + 20 from the send");
    assert_eq!(line.attempt, 2);
    assert_eq!(line.request_bytes, 123);
    assert_eq!(line.response_bytes, 91);
    assert_eq!(line.status, Some(200));
}

#[test]
fn zero_ms_values_are_present_never_omitted() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.latch_first_token();
    finish.finish();
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].headers_ms, Some(0));
    assert_eq!(lines[0].first_token_ms, Some(0));
    assert_eq!(lines[0].total_ms, 0);
}

#[test]
fn absent_milestones_stay_omitted() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    // No head, no delta: a send that failed before the headers.
    let mut finish = record(&log, &clock, clock.now());
    finish.set_bytes(0);
    finish.finish();
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].status, None);
    assert_eq!(lines[0].headers_ms, None);
    assert_eq!(lines[0].first_token_ms, None);
    assert_eq!(lines[0].response_bytes, 0);
}

#[test]
fn a_backwards_clock_contributes_zero() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let earlier = clock.now();
    clock.advance(Duration::from_millis(9));
    // The stamps predate the send: no panic, no fabricated duration.
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(earlier, 200);
    finish.finish();
    assert_eq!(log.lines()[0].headers_ms, Some(0));
}

#[test]
fn millisecond_conversion_saturates() {
    assert_eq!(duration_ms(Duration::from_millis(5)), 5);
    assert_eq!(duration_ms(Duration::ZERO), 0);
    assert_eq!(duration_ms(Duration::from_secs(u64::MAX)), u64::MAX);
    // Just over the edge still saturates.
    assert_eq!(duration_ms(Duration::new(u64::MAX, 999_999_999)), u64::MAX);
}

#[test]
fn the_byte_counter_saturates_at_the_edge() {
    let mut counted = crate::http::CountingReader::new(std::io::empty());
    counted.add(u64::MAX - 1);
    assert_eq!(counted.count(), u64::MAX - 1, "limit minus one holds");
    counted.add(1);
    assert_eq!(counted.count(), u64::MAX, "the limit holds");
    counted.add(1);
    assert_eq!(counted.count(), u64::MAX, "past the limit saturates");
    counted.add(u64::MAX);
    assert_eq!(counted.count(), u64::MAX, "a huge addition saturates");
}

#[test]
fn the_attempt_ordinal_caps_at_u32_max() {
    assert_eq!(NonZeroU32::MIN.saturating_add(0).get(), 1);
    assert_eq!(NonZeroU32::MIN.saturating_add(1).get(), 2);
    assert_eq!(NonZeroU32::MIN.saturating_add(u32::MAX - 1).get(), u32::MAX);
    assert_eq!(NonZeroU32::MIN.saturating_add(u32::MAX).get(), u32::MAX);
}

#[test]
fn the_first_latch_wins() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    clock.advance(Duration::from_millis(4));
    finish.latch_first_token();
    clock.advance(Duration::from_millis(40));
    finish.latch_first_token();
    finish.finish();
    assert_eq!(log.lines()[0].first_token_ms, Some(4));
}

#[test]
fn finish_writes_once() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.finish();
    clock.advance(Duration::from_millis(100));
    finish.finish();
    let lines = log.lines();
    assert_eq!(
        lines.len(),
        1,
        "the drop after an error writes nothing more"
    );
    assert_eq!(lines[0].total_ms, 0, "the first finish stands");
}

#[test]
fn an_open_body_counts_and_finishes_once() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(Secrets::default());
    let body = std::io::Cursor::new(b"hello, world".to_vec());
    let mut open = OpenBody::observed(Box::new(body), finish);
    let mut text = String::new();
    std::io::Read::read_to_string(&mut open, &mut text).unwrap();
    assert_eq!(text, "hello, world");
    drop(open);
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].response_bytes, 12);
    assert_eq!(lines[0].first_token_ms, None, "an open has no deltas");
}

#[test]
fn an_open_body_dropped_unread_records_zero_bytes() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(Secrets::default());
    let body = std::io::Cursor::new(b"hello".to_vec());
    drop(OpenBody::observed(Box::new(body), finish));
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].response_bytes, 0);
}

#[test]
fn an_open_body_ending_in_error_writes_once() {
    struct Fails;
    impl std::io::Read for Fails {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let _ = buf;
            Err(std::io::Error::other("gone"))
        }
    }
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(Secrets::default());
    let mut open = OpenBody::observed(Box::new(Fails), finish);
    let error = std::io::Read::read_to_string(&mut open, &mut String::new()).unwrap_err();
    assert_eq!(error.to_string(), "gone");
    drop(open);
    assert_eq!(
        log.lines().len(),
        1,
        "the drop after the error writes nothing more"
    );
}

#[test]
fn a_zero_length_read_without_eof_extends_the_record() {
    struct EmptyOnce {
        empty: bool,
    }
    impl std::io::Read for EmptyOnce {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.empty {
                self.empty = false;
                return Ok(0);
            }
            if buf.is_empty() {
                return Ok(0);
            }
            buf[0] = b'x';
            Ok(1)
        }
    }
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(Secrets::default());
    let mut open = OpenBody::observed(Box::new(EmptyOnce { empty: true }), finish);
    let mut one = [0u8; 1];
    // A zero-length read that is not the end extends the record.
    assert_eq!(std::io::Read::read(&mut open, &mut one).unwrap(), 0);
    assert!(log.lines().is_empty(), "no line before the end");
    assert_eq!(std::io::Read::read(&mut open, &mut one).unwrap(), 1);
    drop(open);
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].response_bytes, 1);
}

#[test]
fn an_interrupted_read_never_ends_the_record() {
    struct InterruptedOnce {
        interrupted: bool,
    }
    impl std::io::Read for InterruptedOnce {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupted {
                self.interrupted = false;
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            }
            if buf.is_empty() {
                return Ok(0);
            }
            buf[0] = b'y';
            Ok(1)
        }
    }
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(Secrets::default());
    let mut open = OpenBody::observed(Box::new(InterruptedOnce { interrupted: true }), finish);
    let mut one = [0u8; 1];
    let error = std::io::Read::read(&mut open, &mut one).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(log.lines().is_empty(), "no line on an interrupted read");
    assert_eq!(std::io::Read::read(&mut open, &mut one).unwrap(), 1);
    drop(open);
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].response_bytes, 1);
}
