//! Observation state tests: the record's milestones count from the send,
//! the first token latches once, the clock never runs backwards and every
//! conversion saturates. Transport milestones and the recorded target are
//! covered in `http_tests.rs`; the wire, in `tests/diagnostics.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::diag::{DebugLog, HttpRequest, Identity};

use super::*;
use fakes::Deadline;

/// Captures the lines a record writes.
struct Capture {
    debugging: bool,
    lines: Mutex<Vec<HttpRequest>>,
}

impl Capture {
    fn new(debugging: bool) -> Arc<Self> {
        Arc::new(Self {
            debugging,
            lines: Mutex::new(Vec::new()),
        })
    }

    fn lines(&self) -> Vec<HttpRequest> {
        self.lines.lock().unwrap().clone()
    }
}

impl DebugLog for Capture {
    fn debugging(&self) -> bool {
        self.debugging
    }

    fn http_request(&self, request: &HttpRequest) {
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
    // No code under test waits on the clock here; it stamps pre-send work outside any duration (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(30));
    let mut finish = record(&log, &clock, clock.now());
    // No code under test waits on the clock here; it stamps the send (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(5));
    finish.sent(clock.now(), 200);
    // No code under test waits on the clock here; it stamps the first token (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(7));
    finish.latch_first_token();
    // A later delta never overwrites the first stamp.
    // No code under test waits on the clock here; it stamps a later token (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(17));
    finish.latch_first_token();
    finish.set_bytes(91);
    // No code under test waits on the clock here; it stamps the finish (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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

/// A finished model call carries the provider and no other identity:
/// the constructor fixes the wire identity, so no producer can leave a
/// provider purpose without one.
#[test]
fn finish_carries_provider_and_no_extension() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.finish();
    let lines = log.lines();
    // docs/state.md, "Diagnostic logs": one line when the call ends; a retried call writes one line per attempt.
    // docs/state.md, "Diagnostic logs": one line when the call ends; a retried call writes one line per attempt.
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].identity,
        Some(Identity::Provider("fake".to_owned()))
    );
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
    // No code under test waits on the clock here; it stamps the earlier instant the backwards subtraction reads (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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
    // No code under test waits on the clock here; it stamps the latch (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(4));
    finish.latch_first_token();
    // No code under test waits on the clock here; it stamps a second latch (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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
    // No code under test waits on the clock here; it stamps a second finish (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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
fn an_open_body_finishes_at_eof_while_still_alive() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(Secrets::default());
    let body = std::io::Cursor::new(b"hello".to_vec());
    let mut open = OpenBody::observed(Box::new(body), finish);
    let mut text = String::new();
    std::io::Read::read_to_string(&mut open, &mut text).unwrap();
    assert_eq!(text, "hello");
    assert_eq!(
        log.lines().len(),
        1,
        "the end of the stream finishes the line while the reader is still alive"
    );
    assert_eq!(log.lines()[0].response_bytes, 5);
    drop(open);
    assert_eq!(
        log.lines().len(),
        1,
        "the drop after the end writes nothing more"
    );
}

#[test]
fn an_empty_buffer_read_never_ends_the_record() {
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let mut finish = record(&log, &clock, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(Secrets::default());
    let body = std::io::Cursor::new(b"hi".to_vec());
    let mut open = OpenBody::observed(Box::new(body), finish);
    // A zero-length read without end-of-stream never ends the record.
    assert_eq!(std::io::Read::read(&mut open, &mut []).unwrap(), 0);
    assert!(log.lines().is_empty(), "no line on a zero-length read");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut open, &mut text).unwrap();
    assert_eq!(text, "hi");
    assert_eq!(log.lines().len(), 1);
    assert_eq!(log.lines()[0].response_bytes, 2);
    drop(open);
    assert_eq!(
        log.lines().len(),
        1,
        "the drop after the end writes nothing more"
    );
}

#[test]
fn a_zero_length_read_with_a_nonempty_buffer_ends_the_record() {
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
    // `Ok(0)` with a nonempty buffer is the end of the stream: it
    // finishes the line while the reader is still alive.
    assert_eq!(std::io::Read::read(&mut open, &mut one).unwrap(), 0);
    assert_eq!(
        log.lines().len(),
        1,
        "the end finishes the line while the reader is still alive"
    );
    assert_eq!(log.lines()[0].response_bytes, 0);
    drop(open);
    assert_eq!(
        log.lines().len(),
        1,
        "the drop after the end writes nothing more"
    );
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

/// How long a test waits for one transport call to return: the server
/// answers at once, so the wait only bounds a hang.
const CALL_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// One minimal request: a user turn with no images, tools or rewound state.
fn test_request() -> contract::provider::ModelRequest {
    contract::provider::ModelRequest {
        system_prompt: "You are terse.".to_owned(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: contract::events::CacheLifetime::OneHour,
        cache_key: "session_1".to_owned(),
        conversation: (vec![contract::provider::Input::User {
            text: "hi".to_owned(),
            images: Vec::new(),
        }])
        .into(),
        sent_tools: None,
        previous_end: None,
        max_output_tokens: None,
        media: Default::default(),
    }
}

/// A direct endpoint for `server` holding no key and no signer.
fn test_endpoint(server: &fakes::ProviderServer) -> crate::Endpoint {
    crate::Endpoint {
        provider: "fake".to_owned(),
        model: "m".to_owned(),
        base_url: server.url(),
        direct: true,
        ..crate::Endpoint::default()
    }
}

/// Reads the body, forwards one text delta and answers done: a decoder with
/// no protocol framing, so `Call` tests pin observation, not decoding.
#[allow(
    clippy::result_large_err,
    reason = "the decode carries its partial alongside the error for the call's usage"
)]
fn stub_decode(
    stream: &mut dyn std::io::BufRead,
    _: contract::events::CacheLifetime,
    sink: &mut dyn FnMut(contract::provider::Delta),
) -> Result<contract::provider::Reply, (crate::Error, Option<contract::provider::CallUsage>)> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut *stream, &mut text).unwrap();
    sink(contract::provider::Delta::Text(
        contract::events::TextDelta { text },
    ));
    Ok(contract::provider::Reply {
        actions: Vec::new(),
        finish: contract::provider::Finish::Completed,
        generation_id: None,
        tokens: contract::shapes::Tokens {
            input: 0,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 0,
        },
        web_searches: None,
        cost: None,
        input_size: contract::provider::InputSize {
            bytes: 0,
            media: false,
        },
    })
}

/// Runs `call` on its own thread, so a call that never returns fails the
/// test at the deadline instead of hanging it.
fn run_call(
    call: Call,
) -> (
    Result<contract::provider::Reply, contract::provider::CallError>,
    Vec<contract::provider::Delta>,
) {
    use contract::provider::ModelCall as _;
    fakes::within(
        "the call to return",
        Deadline::after(CALL_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = call.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    )
}

/// A clock that counts its `now()` reads, delegating the time itself.
struct SpyClock {
    inner: Arc<fakes::clock::FakeClock>,
    reads: std::sync::atomic::AtomicU64,
}

impl SpyClock {
    fn wrap(clock: &Arc<fakes::clock::FakeClock>) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::clone(clock),
            reads: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn reads(&self) -> u64 {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl contract::clock::Clock for SpyClock {
    fn now(&self) -> std::time::Instant {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.now()
    }

    fn wall(&self) -> std::time::SystemTime {
        self.inner.wall()
    }

    fn sleep(&self, duration: Duration) {
        self.inner.sleep(duration);
    }

    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        on_wake: &mut dyn FnMut(Option<Duration>),
    ) {
        self.inner.wait_until(until, on_wake);
    }

    fn subscribe(&self, wake: std::sync::Weak<dyn contract::clock::Wake>) {
        self.inner.subscribe(wake);
    }
}

#[test]
fn a_non_2xx_through_run_keeps_its_status_and_partial_bytes() {
    // The failure reaches `finish` only through `dispatched`: no `sent`
    // head ever arrives on this path, so without it the line loses the
    // status, the header time and the bytes read so far.
    let body = r#"{"error":"boom"}"#;
    let server = fakes::ProviderServer::start([fakes::Response::status(500, body)]).unwrap();
    let endpoint = test_endpoint(&server);
    let request = test_request();
    let url = format!("{}/v1", server.url());
    let log = Capture::new(true);
    let clock = fakes::clock::FakeClock::new();
    let call = Call::new(
        &endpoint,
        &request,
        url,
        Vec::new(),
        b"{}".to_vec(),
        stub_decode,
    )
    .observed(observation(&log, &clock, 1));
    let (reply, _) = run_call(call);
    let Err(contract::provider::CallError::Failed { .. }) = reply else {
        panic!("a 500 through run is a failure");
    };
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert_eq!(line.status, Some(500));
    assert_eq!(line.response_bytes, body.len() as u64);
    assert!(line.headers_ms.is_some(), "the error keeps its header time");
}

#[test]
fn a_failed_read_finishes_before_the_drop() {
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
    assert_eq!(
        log.lines().len(),
        1,
        "the failed read finishes the line while the reader is still alive"
    );
    drop(open);
    assert_eq!(
        log.lines().len(),
        1,
        "the drop after the error writes nothing more"
    );
}

#[test]
fn at_info_an_observed_call_runs_unobserved() {
    let server = fakes::ProviderServer::start([
        fakes::Response::status(200, "hi"),
        fakes::Response::status(200, "hi"),
    ])
    .unwrap();
    let endpoint = test_endpoint(&server);
    let request = test_request();
    let plain = Call::new(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        stub_decode,
    );
    let (plain_reply, plain_deltas) = run_call(plain);
    let log = Capture::new(false);
    let clock = fakes::clock::FakeClock::new();
    let spy = SpyClock::wrap(&clock);
    let info = RequestObservation::new(
        Arc::clone(&log) as Arc<dyn DebugLog>,
        Arc::clone(&spy) as Arc<dyn contract::clock::Clock>,
        NonZeroU32::MIN,
    )
    .with_model("m".to_owned());
    let observed = Call::new(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        stub_decode,
    )
    .observed(info);
    let (info_reply, info_deltas) = run_call(observed);
    assert_eq!(
        format!("{plain_reply:?}"),
        format!("{info_reply:?}"),
        "the same reply"
    );
    assert_eq!(plain_deltas, info_deltas, "the same deltas");
    assert_eq!(spy.reads(), 0, "no diagnostic clock read");
    assert!(log.lines().is_empty(), "no line at info");
}
