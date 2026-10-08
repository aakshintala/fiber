//! The `scripted` protocol (`docs/model-routing.md`, "The scripted
//! provider"): the script file's steps, and each request served the next
//! step in order on the injected clock.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "test code; a failure is the test's")]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::ErrorCode;
use contract::clock::{Clock, Wake};
use contract::events::CacheLifetime;
use contract::provider::{
    CallError, Delta, Finish, InputSize, ModelCall, ModelRequest, Provider, Reply, ReplyAction,
};
use contract::shapes::Tokens;
use fakes::clock::FakeClock;
use serde_json::json;

use super::{Script, ScriptError, Scripted};

/// The wall-clock bound on every receive and every wait for a park.
const WITHIN: Duration = Duration::from_secs(5);

/// The upper bound on `Clock::wait_until` entries while one pause absorbs a
/// wake short of its deadline: the wake re-parks, plus at most a stray
/// spurious wake or two. The `guard.seq != seen` -> `==` mutant returns from
/// the waker closure at once, so it re-enters without bound and trips this.
const MAX_PARKS: usize = 8;

/// Wall time meaning "no breach arrived, so the pause stayed under the
/// bound": the mutant's busy loop trips the bound in microseconds, while a
/// correct pause never trips it, so the wait runs out.
const STAYED_UNDER: Duration = Duration::from_secs(2);

/// Counts `Clock::wait_until` entries, delegating everything else to the
/// `FakeClock`, and signals when the count passes [`MAX_PARKS`].
struct CountingClock {
    inner: Arc<FakeClock>,
    state: Mutex<CountState>,
    breached: Condvar,
}

struct CountState {
    entries: usize,
    breached: bool,
}

impl CountingClock {
    fn new(inner: Arc<FakeClock>) -> Self {
        Self {
            inner,
            state: Mutex::new(CountState {
                entries: 0,
                breached: false,
            }),
            breached: Condvar::new(),
        }
    }

    fn entries(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
    }

    /// True once entries pass [`MAX_PARKS`]; false when `within` runs out.
    fn await_breach(&self, within: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = self
            .breached
            .wait_timeout_while(state, within, |state| !state.breached)
            .unwrap_or_else(PoisonError::into_inner);
        guard.breached
    }
}

impl Clock for CountingClock {
    fn now(&self) -> Instant {
        self.inner.now()
    }

    fn wall(&self) -> SystemTime {
        self.inner.wall()
    }

    fn sleep(&self, d: Duration) {
        self.inner.sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        let trip = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.entries += 1;
            if state.entries > MAX_PARKS && !state.breached {
                state.breached = true;
                true
            } else {
                false
            }
        };
        if trip {
            self.breached.notify_all();
        }
        self.inner.wait_until(until, wait);
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        self.inner.subscribe(waker);
    }
}

fn request(text: &str) -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: String::new(),
        conversation: vec![contract::provider::Input::User {
            text: text.into(),
            images: Vec::new(),
        }],
        previous_end: None,
        max_output_tokens: None,
        session_dir: PathBuf::new(),
    }
}

fn parse(value: serde_json::Value) -> Result<Script, ScriptError> {
    Script::parse(Path::new("s.json"), value.to_string().as_bytes())
}

/// The malformed step's index and reason; panics on any other result.
fn malformed(value: serde_json::Value) -> (Option<usize>, String) {
    match parse(value) {
        Err(ScriptError::Malformed { step, reason, .. }) => (step, reason),
        other => panic!("not malformed: {other:?}"),
    }
}

fn provider(steps: serde_json::Value, clock: &Arc<FakeClock>) -> Scripted {
    let script = parse(json!({ "steps": steps })).unwrap();
    Scripted::new(
        PathBuf::from("s.json"),
        script,
        Arc::clone(clock) as Arc<dyn Clock>,
    )
}

/// Runs one call to completion on this thread, collecting its fragments.
#[allow(clippy::result_large_err, reason = "the seam's own result")]
fn run(provider: &Scripted, text: &str) -> (Vec<Delta>, Result<Reply, CallError>) {
    let call = provider.call(&request(text));
    let mut deltas = Vec::new();
    let end = call.run(&mut |delta| deltas.push(delta));
    (deltas, end)
}

fn text_delta(text: &str) -> Delta {
    Delta::Text(contract::events::TextDelta { text: text.into() })
}

fn tokens(input: u64, output: u64, cache_read: u64) -> Tokens {
    Tokens {
        input,
        cache_read,
        cache_write: Default::default(),
        output,
    }
}

#[test]
fn text_is_one_fragment_or_fragments_in_order() {
    let clock = FakeClock::new();
    let one = provider(json!([{ "text": "Hello." }]), &clock);
    let (deltas, end) = run(&one, "hi");
    assert_eq!(deltas, [text_delta("Hello.")]);
    assert_eq!(end.unwrap().text(), "Hello.");

    let many = provider(json!([{ "text": ["Hel", "lo", "."] }]), &clock);
    let (deltas, end) = run(&many, "hi");
    assert_eq!(
        deltas,
        [text_delta("Hel"), text_delta("lo"), text_delta(".")]
    );
    let reply = end.unwrap();
    assert_eq!(reply.finish, Finish::Completed);
    assert_eq!(
        reply.actions,
        [ReplyAction::Text(contract::events::TextCompleted {
            text: "Hello.".into(),
            provider_item: None,
        })]
    );
}

#[test]
fn a_tool_call_step_yields_calls_with_stable_ids() {
    let clock = FakeClock::new();
    let scripted = provider(
        json!([
            { "text": "x" },
            { "tool_calls": [
                { "name": "read", "arguments": { "path": "a.md" } },
                { "name": "shell", "arguments": { "command": "ls" } }
            ] }
        ]),
        &clock,
    );
    assert_eq!(run(&scripted, "first").1.unwrap().text(), "x");
    let reply = run(&scripted, "second").1.unwrap();
    let calls: Vec<(String, serde_json::Value, String)> = reply
        .actions
        .iter()
        .map(|action| match action {
            ReplyAction::ToolCall(call) => (
                call.name.clone(),
                call.arguments.clone(),
                call.provider_id.clone().unwrap().0,
            ),
            other @ (ReplyAction::Text(_) | ReplyAction::Reasoning(_) | ReplyAction::Hosted(_)) => {
                panic!("not a tool call: {other:?}")
            }
        })
        .collect();
    assert_eq!(
        calls,
        [
            ("read".into(), json!({ "path": "a.md" }), "call_2_1".into()),
            (
                "shell".into(),
                json!({ "command": "ls" }),
                "call_2_2".into()
            ),
        ]
    );
}

#[test]
fn reasoning_comes_before_text_and_tool_calls() {
    let clock = FakeClock::new();
    let scripted = provider(
        json!([{ "reasoning": "Think.", "text": "Say.", "tool_calls": [{ "name": "read", "arguments": {} }] }]),
        &clock,
    );
    let (deltas, end) = run(&scripted, "hi");
    assert_eq!(
        deltas,
        [
            Delta::Reasoning(contract::events::TextDelta {
                text: "Think.".into()
            }),
            text_delta("Say."),
        ]
    );
    let kinds: Vec<&str> = end
        .unwrap()
        .actions
        .iter()
        .map(|action| match action {
            ReplyAction::Reasoning(r) => {
                assert_eq!(r.text, "Think.");
                "reasoning"
            }
            ReplyAction::Text(_) => "text",
            ReplyAction::ToolCall(_) => "tool_call",
            ReplyAction::Hosted(_) => "hosted",
        })
        .collect();
    assert_eq!(kinds, ["reasoning", "text", "tool_call"]);
}

#[test]
fn usage_tokens_default_to_zero_and_a_reply_names_no_generation_or_cost() {
    let clock = FakeClock::new();
    let scripted = provider(
        json!([
            { "text": "a" },
            { "text": "b", "usage": { "input": 120, "output": 4, "cache_read": 7 } },
            { "text": "c", "usage": { "output": 2 } }
        ]),
        &clock,
    );
    let expected = [tokens(0, 0, 0), tokens(120, 4, 7), tokens(0, 2, 0)];
    for want in expected {
        let reply = run(&scripted, "hi").1.unwrap();
        assert_eq!(reply.tokens, want);
        assert_eq!(reply.generation_id, None);
        assert_eq!(reply.cost, None);
        assert_eq!(reply.web_searches, None);
        assert_eq!(
            reply.input_size,
            InputSize {
                bytes: 0,
                media: false
            }
        );
    }
}

#[test]
fn an_error_step_fails_with_its_code_message_and_wait() {
    let clock = FakeClock::new();
    let rows = [
        ("rate_limited", ErrorCode::RateLimited, Some(2000)),
        ("rate_limited", ErrorCode::RateLimited, None),
        ("provider_unavailable", ErrorCode::ProviderUnavailable, None),
        ("quota_exceeded", ErrorCode::QuotaExceeded, Some(5)),
    ];
    for (name, code, wait) in rows {
        let mut error = json!({ "code": name, "message": "Slow down." });
        if let Some(ms) = wait {
            error["retry_after_ms"] = json!(ms);
        }
        let scripted = provider(json!([{ "error": error }]), &clock);
        let (deltas, end) = run(&scripted, "hi");
        assert!(deltas.is_empty(), "{name}");
        match end {
            Err(CallError::Failed {
                failure,
                should_retry,
                usage,
            }) => {
                assert_eq!(failure.code, code, "{name}");
                assert_eq!(failure.message, "Slow down.");
                assert_eq!(failure.retry_after_ms, wait, "{name}");
                assert_eq!(should_retry, None);
                assert_eq!(usage.tokens, tokens(0, 0, 0));
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[test]
fn a_request_past_the_last_step_fails_invalid_request() {
    let clock = FakeClock::new();
    let scripted = provider(json!([{ "text": "one" }, { "text": "two" }]), &clock);
    assert_eq!(run(&scripted, "a").1.unwrap().text(), "one");
    assert_eq!(run(&scripted, "b").1.unwrap().text(), "two");
    match run(&scripted, "c").1 {
        Err(CallError::Failed { failure, .. }) => {
            assert_eq!(failure.code, ErrorCode::InvalidRequest);
            assert_eq!(
                failure.message,
                "The script `s.json` has no step for request 3; it has 2 steps."
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_exhausted_one_step_script_says_step() {
    let clock = FakeClock::new();
    let scripted = provider(json!([{ "text": "one" }]), &clock);
    assert_eq!(run(&scripted, "a").1.unwrap().text(), "one");
    match run(&scripted, "b").1 {
        Err(CallError::Failed { failure, .. }) => assert_eq!(
            failure.message,
            "The script `s.json` has no step for request 2; it has 1 step."
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn requests_are_never_matched_against_steps() {
    let clock = FakeClock::new();
    let scripted = provider(json!([{ "text": "one" }, { "text": "two" }]), &clock);
    assert_eq!(run(&scripted, "two").1.unwrap().text(), "one");
    assert_eq!(run(&scripted, "one").1.unwrap().text(), "two");
}

#[test]
fn a_slow_reply_waits_on_the_clock_before_every_fragment_after_the_first() {
    let clock = FakeClock::new();
    let scripted = provider(
        json!([{ "text": ["a", "b", "c"], "every_ms": 200 }]),
        &clock,
    );
    let call: Arc<dyn ModelCall> = Arc::from(scripted.call(&request("hi")));
    let (fragments, received) = mpsc::channel();
    let (ended, end) = mpsc::channel();
    let running = Arc::clone(&call);
    thread::spawn(move || {
        let result = running.run(&mut |delta| {
            fragments.send(delta).unwrap();
        });
        ended.send(result).unwrap();
    });

    let pause = Duration::from_millis(200);
    assert_eq!(received.recv_timeout(WITHIN).unwrap(), text_delta("a"));
    let first = clock.origin() + pause;
    let mark = clock.mark_parked(first, WITHIN).unwrap();
    assert!(received.try_recv().is_err());
    clock.advance(Duration::from_millis(199));
    assert!(clock.await_parked_since(&mark, Some(first), WITHIN));
    assert!(received.try_recv().is_err());
    clock.advance(Duration::from_millis(1));
    assert_eq!(received.recv_timeout(WITHIN).unwrap(), text_delta("b"));

    assert!(clock.await_parked(first + pause, WITHIN));
    assert!(received.try_recv().is_err());
    clock.advance(pause);
    assert_eq!(received.recv_timeout(WITHIN).unwrap(), text_delta("c"));
    assert_eq!(end.recv_timeout(WITHIN).unwrap().unwrap().text(), "abc");
}

#[test]
fn every_ms_zero_streams_without_waiting() {
    let clock = FakeClock::new();
    let scripted = provider(json!([{ "text": ["a", "b"], "every_ms": 0 }]), &clock);
    let (deltas, end) = run(&scripted, "hi");
    assert_eq!(deltas, [text_delta("a"), text_delta("b")]);
    assert_eq!(end.unwrap().text(), "ab");
}

#[test]
fn a_cancel_before_run_returns_cancelled_with_no_fragment() {
    let clock = FakeClock::new();
    let scripted = provider(json!([{ "text": "a" }, { "text": "b" }]), &clock);
    let call = scripted.call(&request("hi"));
    call.cancel();
    let mut deltas = Vec::new();
    let end = call.run(&mut |delta| deltas.push(delta));
    assert!(deltas.is_empty());
    assert!(matches!(end, Err(CallError::Cancelled { .. })), "{end:?}");
    // The cancelled request consumed its step.
    assert_eq!(run(&scripted, "again").1.unwrap().text(), "b");
}

#[test]
fn a_cancel_during_a_pause_wakes_it_and_returns_cancelled() {
    let clock = FakeClock::new();
    let scripted = provider(
        json!([{ "text": ["a", "b"], "every_ms": 200, "usage": { "input": 9 } }]),
        &clock,
    );
    let call: Arc<dyn ModelCall> = Arc::from(scripted.call(&request("hi")));
    let (ended, end) = mpsc::channel();
    let running = Arc::clone(&call);
    thread::spawn(move || {
        let mut deltas = Vec::new();
        let result = running.run(&mut |delta| deltas.push(delta));
        ended.send((deltas, result)).unwrap();
    });
    assert!(clock.await_parked(clock.origin() + Duration::from_millis(200), WITHIN));
    call.cancel();
    let (deltas, result) = end.recv_timeout(WITHIN).unwrap();
    assert_eq!(deltas, [text_delta("a")]);
    match result {
        Err(CallError::Cancelled { usage }) => assert_eq!(usage.tokens, tokens(9, 0, 0)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_wake_before_the_deadline_keeps_the_pause_until_every_ms_passes() {
    let fake = FakeClock::new();
    let clock = Arc::new(CountingClock::new(Arc::clone(&fake)));
    let script = parse(json!({ "steps": [{ "text": ["a", "b", "c"], "every_ms": 200 }] })).unwrap();
    let scripted = Scripted::new(
        PathBuf::from("s.json"),
        script,
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    let call: Arc<dyn ModelCall> = Arc::from(scripted.call(&request("hi")));
    let (fragments, received) = mpsc::channel();
    let (ended, end) = mpsc::channel();
    let running = Arc::clone(&call);
    thread::spawn(move || {
        let result = running.run(&mut |delta| {
            fragments.send(delta).unwrap();
        });
        ended.send(result).unwrap();
    });

    let first = fake.origin() + Duration::from_millis(200);
    assert_eq!(received.recv_timeout(WITHIN).unwrap(), text_delta("a"));
    let parked = fake.mark_parked(first, WITHIN).unwrap();
    // Time moves short of the deadline: the pause wakes, and parks again.
    fake.advance(Duration::from_millis(100));
    assert!(fake.await_parked_since(&parked, Some(first), WITHIN));
    // A correct pause re-enters `wait_until` a bounded number of times; the
    // `==` mutant returns from the waker closure at once and spins past the
    // bound, tripping the breach. A spurious wake of the pause's condvar
    // only adds one re-entry, so it cannot trip the bound.
    if clock.await_breach(STAYED_UNDER) {
        // Let the spinning call out so the failure does not hang the suite.
        call.cancel();
        panic!(
            "wait_until re-entered without bound after a wake short of the deadline: {} entries",
            clock.entries()
        );
    }
    assert!(received.try_recv().is_err());
    let parked_again = fake.mark_parked(first, WITHIN).unwrap();
    fake.advance(Duration::from_millis(99));
    assert!(fake.await_parked_since(&parked_again, Some(first), WITHIN));
    assert!(received.try_recv().is_err());
    fake.advance(Duration::from_millis(1));
    assert_eq!(received.recv_timeout(WITHIN).unwrap(), text_delta("b"));

    // A cancel after a wake before the next deadline still ends the pause.
    let second = first + Duration::from_millis(200);
    let parked = fake.mark_parked(second, WITHIN).unwrap();
    fake.advance(Duration::from_millis(100));
    assert!(fake.await_parked_since(&parked, Some(second), WITHIN));
    call.cancel();
    match end.recv_timeout(WITHIN).unwrap() {
        Err(CallError::Cancelled { .. }) => {}
        other => panic!("{other:?}"),
    }
    assert!(received.try_recv().is_err());
}

#[test]
fn the_debug_form_names_the_path_and_the_step_count() {
    let clock = FakeClock::new();
    let scripted = provider(json!([{ "text": "one" }, { "text": "two" }]), &clock);
    let shown = format!("{scripted:?}");
    assert!(shown.contains("s.json"), "{shown}");
    assert!(shown.contains("steps: 2"), "{shown}");
}

#[test]
fn each_field_of_a_reply_step_parses() {
    let rows = [
        json!({ "text": "a" }),
        json!({ "text": ["a", "b"] }),
        json!({ "tool_calls": [{ "name": "read", "arguments": { "path": "x" } }] }),
        json!({ "text": "a", "reasoning": "r" }),
        json!({ "text": "a", "usage": { "input": 1, "output": 2, "cache_read": 3 } }),
        json!({ "text": "a", "every_ms": 0 }),
        json!({ "text": "a", "every_ms": 200 }),
        json!({ "error": { "code": "rate_limited", "message": "m" } }),
        json!({ "error": { "code": "provider_unavailable", "message": "m" } }),
        json!({ "error": { "code": "quota_exceeded", "message": "m", "retry_after_ms": 10 } }),
    ];
    for step in rows {
        assert!(parse(json!({ "steps": [step.clone()] })).is_ok(), "{step}");
    }
    assert!(parse(json!({ "steps": [] })).is_ok());
}

#[test]
fn each_malformed_step_is_refused_naming_its_index_and_reason() {
    let rows = [
        (json!({ "text": [] }), "`text`"),
        (
            json!({ "text": [], "tool_calls": [{ "name": "read", "arguments": {} }] }),
            "`text`",
        ),
        (json!({ "text": 3 }), "`text`"),
        (json!({ "text": ["a", 1] }), "`text`"),
        (
            json!({ "tool_calls": [{ "name": "read", "arguments": "x" }] }),
            "`arguments`",
        ),
        (json!({ "tool_calls": [{ "arguments": {} }] }), "`name`"),
        (
            json!({ "tool_calls": [{ "name": "r", "arguments": {}, "id": "x" }] }),
            "`id`",
        ),
        (json!({ "text": "a", "reasoning": 1 }), "`reasoning`"),
        (json!({ "text": "a", "usage": { "input": -1 } }), "`input`"),
        (
            json!({ "text": "a", "usage": { "cache_write": 1 } }),
            "`cache_write`",
        ),
        (json!({ "text": "a", "every_ms": -1 }), "`every_ms`"),
        (
            json!({ "text": "a", "error": { "code": "rate_limited", "message": "m" } }),
            "`error`",
        ),
        (
            json!({ "error": { "code": "not_a_code", "message": "m" } }),
            "`not_a_code`",
        ),
        (
            json!({ "error": { "code": "config_invalid", "message": "m" } }),
            "`config_invalid`",
        ),
        (json!({ "error": { "code": "rate_limited" } }), "`message`"),
        (
            json!({ "error": { "code": "rate_limited", "message": "m", "retry_after_ms": "x" } }),
            "`retry_after_ms`",
        ),
        (json!({ "text": "a", "colour": "red" }), "`colour`"),
        (json!({ "reasoning": "r" }), "`text` or `tool_calls`"),
        (json!("text"), "not an object"),
    ];
    for (step, reason) in rows {
        let (index, message) = malformed(json!({ "steps": [{ "text": "ok" }, step.clone()] }));
        assert_eq!(index, Some(2), "{step}");
        assert!(message.contains(reason), "{step}: {message}");
    }
}

#[test]
fn a_file_that_is_not_a_steps_object_is_refused() {
    for top in [
        json!([]),
        json!({}),
        json!({ "steps": {} }),
        json!({ "steps": [], "extra": 1 }),
    ] {
        let (index, message) = malformed(top.clone());
        assert_eq!(index, None, "{top}");
        assert!(message.contains("`steps`"), "{top}: {message}");
    }
    let err = Script::parse(Path::new("s.json"), b"{not json").unwrap_err();
    assert!(
        matches!(err, ScriptError::Malformed { step: None, .. }),
        "{err:?}"
    );
}

#[test]
fn a_malformed_script_names_the_file_and_step() {
    let err = parse(json!({ "steps": [{ "text": "ok" }, { "colour": 1 }] })).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("s.json"), "{message}");
    assert!(message.contains("step 2"), "{message}");
}

#[test]
fn a_missing_file_is_unreadable() {
    let dir = fakes::TempDir::new("fiber-scripted");
    let path = dir.path().join("gone.json");
    let err = Script::read(&path).unwrap_err();
    assert!(matches!(err, ScriptError::Unreadable { .. }), "{err:?}");
    assert!(err.to_string().contains("gone.json"), "{err}");
    std::fs::write(&path, r#"{"steps":[{"text":"a"}]}"#).unwrap();
    assert!(Script::read(&path).is_ok());
}
