//! Tests for `main::model_host` (`docs/extensions.md`, "Host calls"): the
//! session's routing and credentials behind `host.model`, with the budget
//! gate serializing one session's calls.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::events::{CacheLifetime, Event, TextCompleted, UsageRecorded};
use contract::model::{
    Bound, HostMessage, HostModel as _, HostModelReply, HostModelRequest, HostRole,
};
use contract::provider::{
    CallError, CallUsage, Finish, Input, InputSize, ModelCall, ModelRequest, Provider,
    Reply, ReplyAction,
};
use contract::shapes::{ContentPart, Failure, Tokens};
use contract::{ErrorCode, GenerationId, SessionId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Scripted, ScriptedProvider, TempDir};
use r#loop::{Model, Retry};
use serde_json::{Map, Value};

use crate::model_host::ModelHost;

/// A clock the test moves by hand; `now` starts at its origin.
fn test_clock() -> Arc<FakeClock> {
    FakeClock::new()
}

/// The host under test, holding the session's provider, model, budget,
/// retry policy, directory, id and clock.
#[allow(
    clippy::too_many_arguments,
    reason = "one test host per case, mirroring ModelHost::new"
)]
fn test_host(
    provider: Arc<dyn Provider>,
    dir: PathBuf,
    id: &str,
    clock: Arc<FakeClock>,
    budget: Option<f64>,
    retry: Retry,
) -> ModelHost {
    ModelHost::new(
        provider,
        Model {
            reference: "scripted/s.json".to_owned(),
            cost: None,
            subscription: false,
        },
        budget,
        retry,
        dir,
        SessionId(id.to_owned()),
        clock,
    )
}

/// A retry policy that waits no time between attempts, so retry tests park
/// nowhere: the injected clock still bounds every wait.
fn immediate_retry() -> Retry {
    Retry {
        attempts: 3,
        initial: Duration::ZERO,
        max: Duration::from_millis(60_000),
    }
}

/// A `host.model` request for `model` with a user and an assistant message.
fn request(model: &str) -> HostModelRequest {
    HostModelRequest {
        model: model.to_owned(),
        system: Some("be brief".to_owned()),
        messages: vec![
            HostMessage {
                role: HostRole::User,
                content: vec![ContentPart::Text {
                    text: "hi".to_owned(),
                }],
            },
            HostMessage {
                role: HostRole::Assistant,
                content: vec![ContentPart::Text {
                    text: "hello".to_owned(),
                }],
            },
        ],
        max_tokens: 16,
    }
}

/// The callback's bound: its deadline on `clock`, a fresh cancel signal and
/// a line sink the test reads.
fn bound(
    clock: Arc<FakeClock>,
    deadline: Option<Instant>,
    held: Arc<Mutex<Vec<UsageRecorded>>>,
) -> Bound {
    let sink = Arc::clone(&held);
    Bound {
        deadline,
        clock,
        cancel: Arc::new(CancelToken::new()),
        lines: Arc::new(move |line| {
            sink.lock().unwrap().push(line);
        }),
    }
}

/// A fresh line sink and the lines it collected.

/// One `usage_recorded` line for `generation` with `cost` dollars.
fn usage_line(generation: &str, cost: f64) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(generation.to_owned()),
        model: "scripted/s.json".to_owned(),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 3,
        },
        input_bytes: 100,
        input_media: None,
        web_searches: None,
        cost: Some(cost),
        subscription: None,
        extension: None,
        origin_session_id: None,
        reviewer: None,
    }
}

/// A session directory holding `costs`, one `usage_recorded` line per
/// entry, ready for the host to fold.
fn seed(
    sessions: &std::path::Path,
    id: &str,
    clock: &Arc<FakeClock>,
    costs: &[(&str, f64)],
) -> PathBuf {
    let log = log::Log::create(sessions, SessionId(id.to_owned()), Arc::clone(clock) as Arc<dyn Clock>)
        .unwrap();
    for (generation, cost) in costs {
        log.append(&Event::UsageRecorded(usage_line(generation, *cost)), None, None)
            .unwrap();
    }
    sessions.join(id)
}

/// Unwraps a reply, failing the test with the failure it got instead.
fn ok(outcome: Result<HostModelReply, Failure>) -> HostModelReply {
    match outcome {
        Ok(reply) => reply,
        Err(failure) => panic!("expected a reply, got {failure:?}"),
    }
}

/// Unwraps a failure, failing the test with a reply it got instead.
fn err(outcome: Result<HostModelReply, Failure>) -> Failure {
    match outcome {
        Ok(_) => panic!("expected a failure, got a reply"),
        Err(failure) => failure,
    }
}

/// A failure the scripted provider returns.
fn fail(code: ErrorCode, retry_after_ms: Option<u64>) -> Failure {
    Failure {
        message: format!("fake {code:?}"),
        code,
        retry_after_ms,
        provider: None,
    }
}

/// What a failed attempt had seen: generation `generation`, no vendor figure.
fn seen(generation: &str) -> CallUsage {
    CallUsage {
        generation_id: Some(GenerationId(generation.to_owned())),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 3,
        },
        web_searches: None,
        input_size: InputSize {
            bytes: 1000,
            media: false,
        },
    }
}

/// A one-text reply with the vendor's figure `cost`, for the "second call"
/// test whose first line's cost prices nothing.
fn reply_with_cost(text: &str, generation: &str, cost: f64) -> Scripted {
    Scripted {
        deltas: Vec::new(),
        end: Ok(Reply {
            actions: vec![ReplyAction::Text(TextCompleted {
                text: text.to_owned(),
                provider_item: None,
            })],
            finish: Finish::Completed,
            generation_id: Some(GenerationId(generation.to_owned())),
            tokens: Tokens {
                input: 10,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 3,
            },
            web_searches: None,
            cost: Some(cost),
            input_size: InputSize {
                bytes: 1000,
                media: false,
            },
        }),
    }
}

/// The cache key names the session and the extension, never the reviewer
/// form, and the request carries the `host.model` fields.
#[test]
fn key_names_the_session_and_the_extension() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "f_9x8y", &clock, &[]);
    let provider = Arc::new(ScriptedProvider::new([Scripted::text("hi")]));
    let host = test_host(
        provider.clone(),
        dir,
        "f_9x8y",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let reply = ok(host
        .call("reviewer", request("scripted/s.json"), bound(clock, None, held)));
    assert_eq!(reply.text, "hi");
    let sent = provider.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].cache_key, "f_9x8y:extension:reviewer");
    assert_eq!(sent[0].tools, Vec::new());
    assert_eq!(sent[0].tool_choice, "none");
    assert_eq!(sent[0].cache_lifetime, CacheLifetime::FiveMinutes);
    assert_eq!(sent[0].thinking, None);
    assert_eq!(sent[0].sent_tools, None);
    assert_eq!(sent[0].previous_end, None);
    assert_eq!(sent[0].session_dir, PathBuf::new());
    assert_eq!(sent[0].max_output_tokens, Some(16));
    assert_eq!(sent[0].system_prompt, "be brief");
    assert_eq!(
        sent[0].conversation,
        vec![
            Input::User {
                text: "hi".to_owned(),
                images: Vec::new(),
            },
            Input::Assistant {
                model: "scripted/s.json".to_owned(),
                text: "hello".to_owned(),
                provider_item: None,
            },
        ]
    );
}

/// Below the cap the call sends; at the cap and above it raises
/// `budget_exceeded` without sending.
#[test]
fn budget_below_cap_sends_at_and_above_cap_fails() {
    for (spend, sends) in [(0.99, true), (1.00, false), (1.01, false)] {
        let root = TempDir::new("fiber-model-host");
        let sessions = root.path().join("sessions");
        let clock = test_clock();
        let dir = seed(&sessions, "s_1", &clock, &[("g_spent", spend)]);
        let provider = Arc::new(ScriptedProvider::new([Scripted::text("hi")]));
        let host = test_host(
            provider.clone(),
            dir,
            "s_1",
            Arc::clone(&clock),
            Some(1.00),
            immediate_retry(),
        );
        let held = Arc::new(Mutex::new(Vec::new()));
        let outcome = host.call("tag", request("scripted/s.json"), bound(clock, None, held));
        if sends {
            ok(outcome);
            assert_eq!(provider.requests().len(), 1);
        } else {
            let failure = err(outcome);
            assert_eq!(failure.code, ErrorCode::BudgetExceeded);
            assert_eq!(
                failure.message,
                "host.model: the budget of 1.00 USD is reached"
            );
            assert!(provider.requests().is_empty());
        }
    }
}

/// A repeated `generation_id` in the log counts once: two 0.60 lines stay
/// below the 1.00 cap.
#[test]
fn repeated_generation_in_the_log_counts_once() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "s_1", &clock, &[("g_same", 0.60), ("g_same", 0.60)]);
    let provider = Arc::new(ScriptedProvider::new([Scripted::text("hi")]));
    let host = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        Some(1.00),
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    ok(host.call("tag", request("scripted/s.json"), bound(clock, None, held)));
    assert_eq!(provider.requests().len(), 1);
}

/// The second of two calls sees the first call's line, although the log
/// holds no extension line: only the first sends.
#[test]
fn second_call_sees_the_first_lines_cost() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "s_1", &clock, &[("g_spent", 0.99)]);
    let provider = Arc::new(ScriptedProvider::new([
        reply_with_cost("one", "g_first", 0.01),
        Scripted::text("two"),
    ]));
    let host = test_host(
        provider.clone(),
        dir.clone(),
        "s_1",
        Arc::clone(&clock),
        Some(1.00),
        immediate_retry(),
    );
    let first_lines = Arc::new(Mutex::new(Vec::new()));
    ok(host.call(
        "tag",
        request("scripted/s.json"),
        bound(Arc::clone(&clock), None, Arc::clone(&first_lines)),
    ));
    assert_eq!(provider.requests().len(), 1);
    // The first line reached the callback but was never written to the log.
    assert!(log::read(&dir).unwrap().is_empty());
    let second_lines = Arc::new(Mutex::new(Vec::new()));
    let failure = err(host.call(
        "tag",
        request("scripted/s.json"),
        bound(clock, None, Arc::clone(&second_lines)),
    ));
    assert_eq!(failure.code, ErrorCode::BudgetExceeded);
    assert_eq!(provider.requests().len(), 1);
    assert!(second_lines.lock().unwrap().is_empty());
}

/// Two failures then a success pass three lines to `bound.lines`, in order.
#[test]
fn failed_attempts_each_pass_a_line_in_order() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let provider = Arc::new(ScriptedProvider::new([
        Scripted::failed_after(fail(ErrorCode::RateLimited, None), seen("g_1")),
        Scripted::failed_after(fail(ErrorCode::RateLimited, None), seen("g_2")),
        Scripted::text("ok"),
    ]));
    let host = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let reply = ok(host.call("tag", request("scripted/s.json"), bound(clock, None, Arc::clone(&held))));
    assert_eq!(reply.text, "ok");
    assert_eq!(provider.requests().len(), 3);
    let held = held.lock().unwrap();
    assert_eq!(held.len(), 3);
    let generations: Vec<String> = held
        .iter()
        .map(|line| line.generation_id.0.clone())
        .collect();
    assert_eq!(generations, vec!["g_1", "g_2", "gen_1"]);
    for line in held.iter() {
        assert_eq!(line.extension, Some("tag".to_owned()));
    }
}

/// Three retries then the failure: four sends, and the failure comes back.
#[test]
fn three_retries_then_fail_sends_four_times() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let provider = Arc::new(ScriptedProvider::new([
        Scripted::failed_after(fail(ErrorCode::RateLimited, None), seen("g_1")),
        Scripted::failed_after(fail(ErrorCode::RateLimited, None), seen("g_2")),
        Scripted::failed_after(fail(ErrorCode::RateLimited, None), seen("g_3")),
        Scripted::failed_after(fail(ErrorCode::RateLimited, None), seen("g_4")),
    ]));
    let host = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let failure = err(host
        .call("tag", request("scripted/s.json"), bound(clock, None, Arc::clone(&held))));
    assert_eq!(failure.code, ErrorCode::RateLimited);
    assert_eq!(provider.requests().len(), 4);
    assert_eq!(held.lock().unwrap().len(), 4);
}

/// `paused_too_long` is not retried: one send.
#[test]
fn paused_too_long_is_not_retried() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let provider = Arc::new(ScriptedProvider::new([Scripted::failed_after(
        fail(ErrorCode::PausedTooLong, None),
        seen("g_1"),
    )]));
    let host = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let failure = err(host
        .call("tag", request("scripted/s.json"), bound(clock, None, Arc::clone(&held))));
    assert_eq!(failure.code, ErrorCode::PausedTooLong);
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(held.lock().unwrap().len(), 1);
}

/// A `Finish::Paused` reply returns its text with no resend.
#[test]
fn paused_reply_returns_its_text_without_resend() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let paused = Scripted {
        deltas: Vec::new(),
        end: Ok(Reply {
            actions: vec![ReplyAction::Text(TextCompleted {
                text: "held".to_owned(),
                provider_item: None,
            })],
            finish: Finish::Paused {
                content: vec![Value::Null],
            },
            generation_id: Some(GenerationId("g_paused".to_owned())),
            tokens: Tokens {
                input: 10,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 3,
            },
            web_searches: None,
            cost: None,
            input_size: InputSize {
                bytes: 1000,
                media: false,
            },
        }),
    };
    let provider = Arc::new(ScriptedProvider::new([paused]));
    let host = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let reply = ok(host
        .call("tag", request("scripted/s.json"), bound(clock, None, Arc::clone(&held))));
    assert_eq!(reply.text, "held");
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(held.lock().unwrap().len(), 1);
}

/// A `retry_after_ms` of 60000 retries after the clock moves; 60001 fails
/// `rate_limited` at once.
#[test]
fn retry_after_at_the_cap_retries_past_it_fails() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let origin = clock.now();
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let provider = Arc::new(ScriptedProvider::new([
        Scripted::failed_after(fail(ErrorCode::RateLimited, Some(60_000)), seen("g_1")),
        Scripted::text("ok"),
    ]));
    let host = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let thread_clock = Arc::clone(&clock);
    let moved = std::thread::spawn({
        let held = Arc::clone(&held);
        move || host.call("tag", request("scripted/s.json"), bound(thread_clock, None, held))
    });
    assert!(clock.await_parked(origin + Duration::from_secs(60), Duration::from_secs(5)));
    clock.advance(Duration::from_secs(60));
    let reply = ok(moved.join().unwrap());
    assert_eq!(reply.text, "ok");
    assert_eq!(provider.requests().len(), 2);

    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let provider = Arc::new(ScriptedProvider::new([Scripted::failed_after(
        fail(ErrorCode::RateLimited, Some(60_001)),
        seen("g_1"),
    )]));
    let host = test_host(
        provider,
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let failure = err(host.call("tag", request("scripted/s.json"), bound(clock, None, held)));
    assert_eq!(failure.code, ErrorCode::RateLimited);
}

/// A retry wait that would pass the deadline fails `timeout` without
/// another send.
#[test]
fn retry_wait_past_the_deadline_fails_timeout() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let deadline = clock.now() + Duration::from_millis(100);
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let provider = Arc::new(ScriptedProvider::new([Scripted::failed_after(
        fail(ErrorCode::RateLimited, Some(60_000)),
        seen("g_1"),
    )]));
    let host = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let failure = err(host.call(
        "tag",
        request("scripted/s.json"),
        bound(clock, Some(deadline), Arc::clone(&held)),
    ));
    assert_eq!(failure.code, ErrorCode::Timeout);
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(held.lock().unwrap().len(), 1);
}

/// An unparseable events line fails `log_corrupt` with no send.
#[test]
fn unparseable_log_fails_log_corrupt() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(sessions.join("s_1")).unwrap();
    std::fs::write(sessions.join("s_1").join("events.jsonl"), "not json\n").unwrap();
    let clock = test_clock();
    let provider = Arc::new(ScriptedProvider::new([Scripted::text("hi")]));
    let host = test_host(
        provider.clone(),
        sessions.join("s_1"),
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let failure = err(host.call("tag", request("scripted/s.json"), bound(clock, None, held)));
    assert_eq!(failure.code, ErrorCode::LogCorrupt);
    assert!(provider.requests().is_empty());
}

/// A directory with no log fails closed with the `NotFound` failure and no
/// send: a running session has a log.
#[test]
fn missing_log_fails_not_found() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let clock = test_clock();
    let provider = Arc::new(ScriptedProvider::new([Scripted::text("hi")]));
    let host = test_host(
        provider.clone(),
        sessions.join("s_absent"),
        "s_absent",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let failure = err(host
        .call("tag", request("scripted/s.json"), bound(clock, None, held)));
    assert_eq!(failure.code, ErrorCode::SessionNotFound);
    assert!(provider.requests().is_empty());
}

/// A model of another provider fails `model_unconfigured` with no send.
#[test]
fn other_provider_fails_model_unconfigured() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let provider = Arc::new(ScriptedProvider::new([Scripted::text("hi")]));
    let host = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let held = Arc::new(Mutex::new(Vec::new()));
    let failure = err(host
        .call("tag", request("other/e.json"), bound(clock, None, held)));
    assert_eq!(failure.code, ErrorCode::ModelUnconfigured);
    assert_eq!(
        failure.message,
        "host.model: only the session's provider is reachable"
    );
    assert!(provider.requests().is_empty());
}

/// A provider that ignores `cancel`, blocking on a channel the test owns.
struct GatedProvider {
    /// How many requests it was given.
    sends: AtomicUsize,
    /// Every request it was given, in order.
    requests: Mutex<Vec<ModelRequest>>,
    /// Fires once the first `run` starts.
    entered: mpsc::Sender<()>,
    /// Releases the first `run`; later runs fail `Cancelled`.
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Provider for GatedProvider {
    fn wire_tools(&self, _tools: &[contract::provider::ToolDefinition]) -> Vec<Map<String, Value>> {
        Vec::new()
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        Box::new(GatedCall {
            entered: self.entered.clone(),
            release: Mutex::new(self.release.lock().unwrap().take()),
        })
    }
}

/// One gated call: the first blocks for the test's release and ignores
/// `cancel`; any later one ends `Cancelled` at once.
struct GatedCall {
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl ModelCall for GatedCall {
    fn run(&self, _sink: &mut dyn FnMut(contract::provider::Delta)) -> Result<Reply, CallError> {
        let _ = self.entered.send(());
        match self.release.lock().unwrap().take() {
            Some(release) => {
                let _ = release.recv();
                Ok(Reply {
                    actions: vec![ReplyAction::Text(TextCompleted {
                        text: "slow".to_owned(),
                        provider_item: None,
                    })],
                    finish: Finish::Completed,
                    generation_id: Some(GenerationId("g_slow".to_owned())),
                    tokens: Tokens {
                        input: 10,
                        cache_read: 0,
                        cache_write: BTreeMap::new(),
                        output: 3,
                    },
                    web_searches: None,
                    cost: None,
                    input_size: InputSize {
                        bytes: 1000,
                        media: false,
                    },
                })
            }
            None => Err(CallError::Cancelled {
                usage: Box::new(CallUsage::unnamed(InputSize {
                    bytes: 1000,
                    media: false,
                })),
            }),
        }
    }

    fn cancel(&self) {}
}

/// The gate holds one call while another waits: the waiting call ends
/// `timeout` at its deadline with no send and no line, and the held call
/// still completes once released.
#[test]
fn gate_holds_one_call_and_times_out_the_next() {
    let root = TempDir::new("fiber-model-host");
    let sessions = root.path().join("sessions");
    let clock = test_clock();
    let origin = clock.now();
    let dir = seed(&sessions, "s_1", &clock, &[]);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let provider = Arc::new(GatedProvider {
        sends: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
        entered: entered_tx,
        release: Mutex::new(Some(release_rx)),
    });
    let first = test_host(
        provider.clone(),
        dir.clone(),
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let first_lines = Arc::new(Mutex::new(Vec::new()));
    let a_clock = Arc::clone(&clock);
    let a = std::thread::spawn({
        let first_lines = Arc::clone(&first_lines);
        move || {
            first.call(
                "a",
                request("scripted/s.json"),
                bound(a_clock, None, first_lines),
            )
        }
    });
    // Call A is inside `run` before B reaches the gate.
    entered_rx.recv().unwrap();
    let second = test_host(
        provider.clone(),
        dir,
        "s_1",
        Arc::clone(&clock),
        None,
        immediate_retry(),
    );
    let second_lines = Arc::new(Mutex::new(Vec::new()));
    let deadline = origin + Duration::from_secs(5);
    let b_clock = Arc::clone(&clock);
    let b = std::thread::spawn({
        let second_lines = Arc::clone(&second_lines);
        move || {
            second.call(
                "b",
                request("scripted/s.json"),
                bound(b_clock, Some(deadline), second_lines),
            )
        }
    });
    assert!(clock.await_parked(deadline, Duration::from_secs(5)));
    clock.advance(Duration::from_secs(6));
    let failure = err(b.join().unwrap());
    assert_eq!(failure.code, ErrorCode::Timeout);
    assert_eq!(provider.sends.load(Ordering::SeqCst), 1);
    assert!(second_lines.lock().unwrap().is_empty());
    release_tx.send(()).unwrap();
    let reply = ok(a.join().unwrap());
    assert_eq!(reply.text, "slow");
    assert_eq!(first_lines.lock().unwrap().len(), 1);
}
