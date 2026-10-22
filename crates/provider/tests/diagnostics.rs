//! Native provider requests observed: the four protocols' success table,
//! the delta latch paths, the open/run lifecycle, the omissions and the
//! `info` no-op. Binary behaviour lives in `main`'s `provider_diagnostics`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

#[path = "support/harness.rs"]
mod harness;

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use contract::diag::{DebugLog, ProviderRequest};
use contract::events::CacheLifetime;
use contract::provider::{ModelCall, ModelRequest};
use fakes::{Deadline, ProviderServer, Response};
use provider::Endpoint;
use serde_json::json;

/// Captures the lines records write.
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

    fn serialized(&self) -> Vec<String> {
        self.lines()
            .iter()
            .map(|line| String::from_utf8(serde_json::to_vec(line).unwrap()).unwrap())
            .collect()
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

/// A clock that ticks forward on every read, so milestone order is
/// observable across threads without advancing a fake.
struct StepClock {
    origin: Instant,
    step: Duration,
    next: std::sync::atomic::AtomicU64,
    reads: std::sync::atomic::AtomicU64,
}

impl StepClock {
    #[allow(
        clippy::disallowed_methods,
        reason = "the stepping clock's origin: every later read is a step, never the process clock"
    )]
    fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            step: Duration::from_millis(5),
            next: std::sync::atomic::AtomicU64::new(0),
            reads: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn reads(&self) -> u64 {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl contract::clock::Clock for StepClock {
    fn now(&self) -> Instant {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let n = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let ticks = u32::try_from(n).unwrap_or(u32::MAX);
        self.origin + self.step * ticks
    }

    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn sleep(&self, _: Duration) {}

    fn wait_until(&self, _: Option<Instant>, _: &mut dyn FnMut(Option<Duration>)) {}

    fn subscribe(&self, _: std::sync::Weak<dyn contract::clock::Wake>) {}
}

fn observation(
    log: &Arc<Capture>,
    clock: &Arc<StepClock>,
    attempt: u32,
) -> provider::RequestObservation {
    provider::RequestObservation::new(
        Arc::clone(log) as Arc<dyn DebugLog>,
        Arc::clone(clock) as Arc<dyn contract::clock::Clock>,
        NonZeroU32::new(attempt).unwrap(),
    )
}

/// One observed call on `protocol`: the protocol's own factory over its
/// request builder, so all four share the one implementation.
fn observed(
    protocol: &str,
    endpoint: &Endpoint,
    request: &ModelRequest,
    observation: provider::RequestObservation,
) -> Box<dyn ModelCall> {
    match protocol {
        "anthropic" => provider::anthropic_messages::Messages::new(endpoint.clone())
            .call_observed(request, observation),
        "opencode" => provider::openai_responses::Responses::new(endpoint.clone())
            .call_observed(request, observation),
        "openrouter" => provider::openai_completions::Completions::new(endpoint.clone())
            .call_observed(request, observation),
        "gemini" => provider::google_generative_ai::Gemini::new(endpoint.clone())
            .call_observed(request, observation),
        _ => unreachable!("unknown protocol {protocol}"),
    }
}

fn completed(protocol: &str) -> Response {
    match protocol {
        "anthropic" => harness::anthropic_completed(),
        "opencode" => Response::stream(harness::responses_sse(&[json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1", "status": "completed",
                "usage": {"input_tokens": 10,
                    "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3},
            },
        })])),
        "openrouter" => harness::completions_completed(),
        "gemini" => harness::gemini_completed(),
        _ => unreachable!(),
    }
}

fn expected_path(protocol: &str) -> &'static str {
    match protocol {
        "anthropic" => "/v1/messages",
        "opencode" => "/v1/responses",
        "openrouter" => "/v1/chat/completions",
        "gemini" => "/v1/models/m:streamGenerateContent",
        _ => unreachable!(),
    }
}

fn observation_flagged(
    log: &Arc<Capture>,
    clock: &Arc<StepClock>,
    attempt: u32,
) -> provider::RequestObservation {
    observation(log, clock, attempt).with_cache_diagnostics(true)
}

/// A 64-character lowercase hex digest, as the line holds them.
fn hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One observed call on `protocol` with the setting on.
fn observed_flagged(
    protocol: &str,
    endpoint: &Endpoint,
    request: &ModelRequest,
    log: &Arc<Capture>,
    clock: &Arc<StepClock>,
    attempt: u32,
) -> Box<dyn ModelCall> {
    observed(
        protocol,
        endpoint,
        request,
        observation_flagged(log, clock, attempt),
    )
}

#[test]
fn with_the_setting_off_no_diagnostic_field_is_written() {
    for protocol in ["anthropic", "opencode", "openrouter", "gemini"] {
        let server = ProviderServer::start([completed(protocol)]).unwrap();
        let endpoint = harness::endpoint(protocol, &server);
        let log = Capture::new(true);
        let clock = StepClock::new();
        let call = observed(
            protocol,
            &endpoint,
            &harness::request(),
            observation(&log, &clock, 1),
        );
        let (reply, _) = harness::run(call);
        assert!(reply.is_ok(), "the completed stream reads on {protocol}");
        assert!(server.await_requests(1, Deadline::after(harness::DEADLINE)));
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "one call writes one line on {protocol}");
        assert_eq!(lines[0].prefix_hashes, None, "nothing new on {protocol}");
        assert_eq!(lines[0].cache_read, None, "nothing new on {protocol}");
        assert_eq!(lines[0].cache_write, None, "nothing new on {protocol}");
    }
}

#[test]
fn each_protocol_writes_one_complete_record() {
    for protocol in ["anthropic", "opencode", "openrouter", "gemini"] {
        let fixture = completed(protocol);
        let body_len = fixture.body.len() as u64;
        let server = ProviderServer::start([fixture]).unwrap();
        let endpoint = harness::endpoint(protocol, &server);
        let request = harness::request();
        let log = Capture::new(true);
        let clock = StepClock::new();
        let call = observed(protocol, &endpoint, &request, observation(&log, &clock, 2));
        let (reply, _) = harness::run(call);
        let reply = reply.expect("the completed stream reads");
        assert!(server.await_requests(1, Deadline::after(harness::DEADLINE)));
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "one call writes one line on {protocol}");
        let line = &lines[0];
        assert_eq!(line.provider, protocol);
        assert_eq!(line.model.as_deref(), Some("m"));
        assert_eq!(line.purpose, contract::diag::Purpose::ModelCall);
        let host = server.url().split_once("://").unwrap().1.to_owned();
        assert_eq!(line.host.as_deref(), Some(host.as_str()), "on {protocol}");
        assert_eq!(line.path.as_deref(), Some(expected_path(protocol)));
        assert_eq!(line.status, Some(200));
        assert_eq!(line.attempt, 2);
        assert_eq!(
            line.request_bytes, reply.input_size.bytes,
            "the posted body length on {protocol}"
        );
        assert!(line.request_bytes > 0);
        assert_eq!(
            line.response_bytes, body_len,
            "every byte read on {protocol}"
        );
        let headers_ms = line.headers_ms.expect("headers came");
        assert!(line.total_ms >= headers_ms, "the total covers the headers");
        if protocol == "opencode" {
            // The completed-only stream carries no delta.
            assert_eq!(line.first_token_ms, None, "no delta, no first token");
        } else {
            // Each completed fixture streams one text fragment first.
            let first = line.first_token_ms.expect("a delta latched on {protocol}");
            assert!(first >= headers_ms, "the token follows the headers");
            assert!(line.total_ms >= first, "the total covers the token");
        }
    }
}

#[test]
fn each_delta_kind_latches_the_first_token_once() {
    let text = harness::responses_sse(&[
        json!({"type": "response.output_text.delta", "delta": "hi"}),
        json!({"type": "response.completed", "response":
            {"id": "r", "status": "completed", "usage": {}}}),
    ]);
    let reasoning = harness::responses_sse(&[
        json!({"type": "response.reasoning_summary_text.delta", "delta": "hmm"}),
        json!({"type": "response.completed", "response":
            {"id": "r", "status": "completed", "usage": {}}}),
    ]);
    let tool_args = harness::responses_sse(&[
        json!({"type": "response.output_item.added",
            "item": {"type": "function_call", "id": "fc_1", "name": "f"}}),
        json!({"type": "response.function_call_arguments.delta",
            "item_id": "fc_1", "delta": "{}"}),
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "c_1",
            "name": "f", "arguments": "{}"}}),
        json!({"type": "response.completed", "response":
            {"id": "r", "status": "completed", "usage": {}}}),
    ]);
    for (name, body) in [
        ("text", text),
        ("reasoning", reasoning),
        ("tool-args", tool_args),
    ] {
        let server = ProviderServer::start([Response::stream(body)]).unwrap();
        let endpoint = harness::endpoint("opencode", &server);
        let log = Capture::new(true);
        let clock = StepClock::new();
        let call = observed(
            "opencode",
            &endpoint,
            &harness::request(),
            observation(&log, &clock, 1),
        );
        let (reply, deltas) = harness::run(call);
        assert!(reply.is_ok(), "the {name} stream reads");
        assert!(!deltas.is_empty(), "the {name} stream emits deltas");
        let lines = log.lines();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].first_token_ms.is_some(),
            "the first {name} delta latches the token"
        );
    }
    // Text then reasoning: the first delta's stamp stands.
    let both = harness::responses_sse(&[
        json!({"type": "response.output_text.delta", "delta": "hi"}),
        json!({"type": "response.reasoning_summary_text.delta", "delta": "hmm"}),
        json!({"type": "response.completed", "response":
            {"id": "r", "status": "completed", "usage": {}}}),
    ]);
    let server = ProviderServer::start([Response::stream(both)]).unwrap();
    let endpoint = harness::endpoint("opencode", &server);
    let log = Capture::new(true);
    let clock = StepClock::new();
    let call = observed(
        "opencode",
        &endpoint,
        &harness::request(),
        observation(&log, &clock, 1),
    );
    let (_, deltas) = harness::run(call);
    assert_eq!(deltas.len(), 2);
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    // The send stamps two ticks (send, headers); the first delta latches
    // the next one, and the second delta never overwrites it.
    assert_eq!(lines[0].headers_ms, Some(5));
    assert_eq!(lines[0].first_token_ms, Some(10));
}

#[test]
fn an_open_reads_and_finishes_once() {
    let fixture = harness::completions_completed();
    let body_len = fixture.body.len() as u64;
    let server = ProviderServer::start([fixture]).unwrap();
    let endpoint = harness::endpoint("openrouter", &server);
    let log = Capture::new(true);
    let clock = StepClock::new();
    let call = provider::openai_completions::Completions::new(endpoint)
        .request(&harness::request())
        .observed(observation(&log, &clock, 1).with_model("m".to_owned()));
    let text = fakes::within(
        "the open read",
        Deadline::after(harness::DEADLINE),
        move || {
            let mut body = call.open().expect("the open sends");
            let mut text = String::new();
            std::io::Read::read_to_string(&mut body, &mut text).unwrap();
            text
        },
    );
    assert!(!text.is_empty());
    assert!(server.await_requests(1, Deadline::after(harness::DEADLINE)));
    let lines = log.lines();
    assert_eq!(lines.len(), 1, "the read and the drop write one line");
    assert_eq!(lines[0].response_bytes, body_len);
    assert_eq!(lines[0].status, Some(200));
    assert_eq!(lines[0].first_token_ms, None, "an open has no deltas");
}

#[test]
fn a_request_never_sent_writes_no_line() {
    let server = ProviderServer::start([harness::completions_completed()]).unwrap();
    let endpoint = harness::endpoint("openrouter", &server);
    let request = harness::request();
    // Cancelled before it starts.
    let log = Capture::new(true);
    let clock = StepClock::new();
    let call = observed(
        "openrouter",
        &endpoint,
        &request,
        observation(&log, &clock, 1),
    );
    call.cancel();
    let (reply, _) = harness::run(call);
    assert!(reply.is_err(), "a cancelled call returns no reply");
    assert!(log.lines().is_empty(), "no send, no line");
    // The cancel is checked before the send, so nothing went out.
    assert!(server.requests().is_empty());
}

#[test]
fn a_signing_failure_writes_no_line() {
    struct Refuses;
    impl contract::signing::Signer for Refuses {
        fn sign(
            &self,
            _: &contract::signing::SignRequest<'_>,
        ) -> Result<Vec<(String, String)>, contract::signing::Error> {
            Err(contract::signing::Error::Failed("no key".into()))
        }
    }
    let server = ProviderServer::start([harness::completions_completed()]).unwrap();
    let mut endpoint = harness::endpoint("openrouter", &server);
    endpoint.signer = Some(Arc::new(Refuses));
    let log = Capture::new(true);
    let clock = StepClock::new();
    let call = observed(
        "openrouter",
        &endpoint,
        &harness::request(),
        observation(&log, &clock, 1),
    );
    let (reply, _) = harness::run(call);
    assert!(reply.is_err());
    assert!(log.lines().is_empty(), "no send, no line");
    assert!(server.requests().is_empty(), "nothing was sent");
}

#[test]
fn at_info_the_call_runs_unchanged_with_no_clock_read_and_no_line() {
    let server = ProviderServer::start([
        harness::completions_completed(),
        harness::completions_completed(),
    ])
    .unwrap();
    let endpoint = harness::endpoint("openrouter", &server);
    let request = harness::request();
    let plain = provider::openai_completions::Completions::new(endpoint.clone()).request(&request);
    let (plain_reply, plain_deltas) = harness::run(Box::new(plain));
    let log = Capture::new(false);
    let clock = StepClock::new();
    let info = observed(
        "openrouter",
        &endpoint,
        &request,
        observation(&log, &clock, 1),
    );
    let (info_reply, info_deltas) = harness::run(info);
    assert_eq!(
        format!("{plain_reply:?}"),
        format!("{info_reply:?}"),
        "the same reply"
    );
    assert_eq!(plain_deltas, info_deltas, "the same deltas");
    assert_eq!(clock.reads(), 0, "no diagnostic clock read");
    assert!(log.lines().is_empty(), "no line at info");
}

#[test]
fn no_credential_header_value_or_body_reaches_a_line() {
    let secret = "sk-live-secret";
    let signed_value = "signed-secret-value";
    struct Signs {
        value: String,
    }
    impl contract::signing::Signer for Signs {
        fn sign(
            &self,
            _: &contract::signing::SignRequest<'_>,
        ) -> Result<Vec<(String, String)>, contract::signing::Error> {
            Ok(vec![("x-signature".to_owned(), self.value.clone())])
        }
    }
    let server = ProviderServer::start([harness::completions_completed()]).unwrap();
    let mut endpoint = harness::endpoint("openrouter", &server);
    endpoint.key = Some(contract::Secret::new(secret.to_owned()));
    endpoint.signer = Some(Arc::new(Signs {
        value: signed_value.to_owned(),
    }));
    let log = Capture::new(true);
    let clock = StepClock::new();
    let call = observed(
        "openrouter",
        &endpoint,
        &harness::request(),
        observation(&log, &clock, 1),
    );
    let (reply, _) = harness::run(call);
    assert!(reply.is_ok());
    let lines = log.serialized();
    assert_eq!(lines.len(), 1);
    for secret in [secret, signed_value, "session_1", "You are terse.", "hi"] {
        assert!(
            !lines[0].contains(secret),
            "no content in the line: {secret}"
        );
    }
    // The transport itself is unchanged: the key and the signed header
    // went out as built.
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("x-signature"), Some(signed_value));
}

/// The expected reported read per protocol's completed fixture: the three
/// fixtures that report cached tokens read four, Gemini reports none and
/// reads zero.
fn expected_read(protocol: &str) -> u64 {
    match protocol {
        "anthropic" | "opencode" | "openrouter" => 4,
        _ => 0,
    }
}

#[test]
fn with_the_setting_on_the_line_carries_hashes_and_reported_use() {
    for protocol in ["anthropic", "opencode", "openrouter", "gemini"] {
        let server = ProviderServer::start([completed(protocol)]).unwrap();
        let endpoint = harness::endpoint(protocol, &server);
        let log = Capture::new(true);
        let clock = StepClock::new();
        let call = observed_flagged(protocol, &endpoint, &harness::request(), &log, &clock, 1);
        let (reply, _) = harness::run(call);
        assert!(reply.is_ok(), "the completed stream reads on {protocol}");
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "one call writes one line on {protocol}");
        let hashes = lines[0]
            .prefix_hashes
            .as_ref()
            .unwrap_or_else(|| panic!("hashes are set on {protocol}"));
        assert!(
            hashes.system.as_ref().is_some_and(|digest| hex64(digest)),
            "the system segment hashes on {protocol}"
        );
        // A request with no tools sends no tool segment except on
        // Responses, whose body always holds a tools list.
        match protocol {
            "opencode" => assert!(
                hashes.tools.as_ref().is_some_and(|digest| hex64(digest)),
                "the always-sent tools list hashes on {protocol}"
            ),
            _ => assert_eq!(hashes.tools, None, "no tool segment on {protocol}"),
        }
        assert!(
            !hashes.messages.is_empty(),
            "the conversation hashes on {protocol}"
        );
        for digest in &hashes.messages {
            assert!(hex64(digest), "each message boundary hashes on {protocol}");
        }
        assert_eq!(
            lines[0].cache_read,
            Some(expected_read(protocol)),
            "the reported read on {protocol}"
        );
        assert!(
            lines[0].cache_write.is_some(),
            "the reported write, empty here, is present on {protocol}"
        );
        // The flagged line stays redacted: no prompt, message, key or
        // session text reaches it.
        for secret in ["You are terse.", "hi", "session_1"] {
            assert!(
                !log.serialized()[0].contains(secret),
                "no content in the flagged line on {protocol}: {secret}"
            );
        }
    }
}

#[test]
fn the_reported_write_is_keyed_by_the_request_lifetime() {
    for (lifetime, key) in [
        (CacheLifetime::OneHour, "1h"),
        (CacheLifetime::FiveMinutes, "5m"),
    ] {
        let body = harness::responses_sse(&[serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1",
                "status": "completed",
                "usage": {
                    "input_tokens": 20,
                    "input_tokens_details": {
                        "cached_tokens": 4,
                        "cache_write_tokens": 7
                    },
                    "output_tokens": 3
                }
            }
        })]);
        let server = ProviderServer::start([Response::stream(body)]).unwrap();
        let endpoint = harness::endpoint("opencode", &server);
        let log = Capture::new(true);
        let clock = StepClock::new();
        let mut request = harness::request();
        request.cache_lifetime = lifetime;
        let call = observed_flagged("opencode", &endpoint, &request, &log, &clock, 1);
        let (reply, _) = harness::run(call);
        assert!(reply.is_ok(), "the completed stream reads for {key}");
        let lines = log.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].cache_read, Some(4));
        assert_eq!(
            lines[0].cache_write,
            Some(std::collections::BTreeMap::from([(key.to_owned(), 7)])),
            "the write is keyed {key}"
        );
    }
}

#[test]
fn an_empty_system_prompt_leaves_the_system_segment_out() {
    let server = ProviderServer::start([completed("anthropic")]).unwrap();
    let endpoint = harness::endpoint("anthropic", &server);
    let log = Capture::new(true);
    let clock = StepClock::new();
    let mut request = harness::request();
    request.system_prompt = String::new();
    let call = observed_flagged("anthropic", &endpoint, &request, &log, &clock, 1);
    let (reply, _) = harness::run(call);
    assert!(reply.is_ok());
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    let hashes = lines[0]
        .prefix_hashes
        .as_ref()
        .expect("the other segments still hash");
    assert_eq!(hashes.system, None, "no system segment hashes");
    assert_eq!(hashes.tools, None, "no tool segment hashes");
    assert!(!hashes.messages.is_empty(), "the messages still hash");
}

#[test]
fn a_tooled_request_hashes_the_tool_segment() {
    let server = ProviderServer::start([completed("anthropic")]).unwrap();
    let endpoint = harness::endpoint("anthropic", &server);
    let log = Capture::new(true);
    let clock = StepClock::new();
    let mut request = harness::request();
    request.tools = vec![contract::provider::ToolDefinition {
        name: "read".to_owned(),
        description: "reads a file".to_owned(),
        input_schema: serde_json::json!({"type": "object"}),
        deferred: false,
        hosted: None,
    }];
    let call = observed_flagged("anthropic", &endpoint, &request, &log, &clock, 1);
    let (reply, _) = harness::run(call);
    assert!(reply.is_ok());
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    let hashes = lines[0].prefix_hashes.as_ref().expect("hashes are set");
    assert!(
        hashes.tools.as_ref().is_some_and(|digest| hex64(digest)),
        "the tool definitions hash"
    );
    // The tool text hashes but never lands in the line.
    assert!(
        !log.serialized()[0].contains("reads a file"),
        "no tool text in the flagged line"
    );
}

#[test]
fn a_failed_attempt_carries_hashes_and_status_without_usage() {
    for protocol in ["anthropic", "opencode", "openrouter", "gemini"] {
        let server = ProviderServer::start([fakes::Response::status(503, "try again")]).unwrap();
        let endpoint = harness::endpoint(protocol, &server);
        let log = Capture::new(true);
        let clock = StepClock::new();
        let call = observed_flagged(protocol, &endpoint, &harness::request(), &log, &clock, 1);
        let (reply, _) = harness::run(call);
        assert!(reply.is_err(), "the 503 fails the call on {protocol}");
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "the failure writes one line on {protocol}");
        assert_eq!(lines[0].status, Some(503), "the status on {protocol}");
        assert_eq!(lines[0].attempt, 1, "the attempt on {protocol}");
        // The bytes went out, so their digests are known; the reply
        // failed, so no usage is reported.
        assert!(
            lines[0].prefix_hashes.is_some(),
            "hashes describe the posted bytes on {protocol}"
        );
        assert_eq!(lines[0].cache_read, None, "no usage on {protocol}");
        assert_eq!(lines[0].cache_write, None, "no usage on {protocol}");
    }
}

#[test]
fn a_retry_writes_one_line_per_attempt_with_usage_only_on_success() {
    for protocol in ["anthropic", "opencode", "openrouter", "gemini"] {
        let server = ProviderServer::start([
            fakes::Response::status(503, "try again"),
            completed(protocol),
        ])
        .unwrap();
        let endpoint = harness::endpoint(protocol, &server);
        let log = Capture::new(true);
        let clock = StepClock::new();
        let request = harness::request();
        let first = observed_flagged(protocol, &endpoint, &request, &log, &clock, 1);
        let (failed, _) = harness::run(first);
        assert!(failed.is_err(), "the first attempt fails on {protocol}");
        let second = observed_flagged(protocol, &endpoint, &request, &log, &clock, 2);
        let (reply, _) = harness::run(second);
        assert!(reply.is_ok(), "the second attempt reads on {protocol}");
        assert!(server.await_requests(2, Deadline::after(harness::DEADLINE)));
        let lines = log.lines();
        assert_eq!(lines.len(), 2, "one line per attempt on {protocol}");
        assert_eq!(lines[0].attempt, 1);
        assert_eq!(lines[0].status, Some(503));
        assert_eq!(lines[0].cache_read, None, "no usage on the failure");
        assert_eq!(lines[0].cache_write, None, "no usage on the failure");
        // Both attempts posted the same bytes, so both lines hash them.
        assert_eq!(
            lines[0].prefix_hashes, lines[1].prefix_hashes,
            "the retry reposts the same prefix on {protocol}"
        );
        assert_eq!(lines[1].attempt, 2);
        assert_eq!(lines[1].status, Some(200));
        assert_eq!(
            lines[1].cache_read,
            Some(expected_read(protocol)),
            "usage on the success"
        );
        assert!(lines[1].cache_write.is_some(), "usage on the success");
    }
}
