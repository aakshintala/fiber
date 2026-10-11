//! The call behaviour every protocol shares: one table over the four
//! protocols, so a change to the shared contract fails naming the protocol
//! that broke it. Each protocol file keeps only the wire shape that differs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

#[path = "support/harness.rs"]
mod harness;

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread;

use contract::events::TextDelta;
use contract::provider::{CallError, CallUsage, Delta, InputSize, ModelCall};
use contract::shapes::Tokens;
use contract::{ErrorCode, GenerationId};
use fakes::{Deadline, ProviderServer, Response};
use provider::Endpoint;
use serde_json::{Value, json};

use harness::{DEADLINE, Protocol};

/// One protocol and the stream prefixes its tests script: `stalled` names a
/// generation and reports usage before blocking, `failed` reports usage then
/// fails, `cut` names a generation and ends, and `error_first` fails before
/// naming one.
struct Wire {
    protocol: Protocol,
    stalled: Vec<u8>,
    stalled_usage: (Option<GenerationId>, Tokens),
    failed: Vec<u8>,
    failed_usage: (Option<GenerationId>, Tokens),
    cut: Vec<u8>,
    cut_generation: GenerationId,
    error_first: Vec<u8>,
}

fn named(id: &str) -> Option<GenerationId> {
    Some(GenerationId(id.into()))
}

fn tokens(input: u64, cache_read: u64, output: u64) -> Tokens {
    Tokens {
        input,
        cache_read,
        cache_write: Default::default(),
        output,
    }
}

fn wires() -> [Wire; 4] {
    let anthropic_started = |usage: Option<Value>| {
        let mut message = json!({"id": "msg_1"});
        if let Some(usage) = usage {
            message["usage"] = usage;
        }
        json!({"type": "message_start", "message": message})
    };
    let anthropic_usage = json!({"input_tokens": 7, "output_tokens": 2});
    let anthropic_stalled = format!(
        "data: {}\n\n\
         data: {}\n\n\
         data: {}\n\n",
        anthropic_started(Some(anthropic_usage.clone())),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hel"}}),
    )
    .into_bytes();

    let responses_created = json!({"type": "response.created",
        "response": {"id": "resp_1", "status": "in_progress"}});
    let responses_usage = json!({"input_tokens": 10,
        "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3});
    let responses_stalled = harness::responses_sse(&[
        responses_created.clone(),
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
    ]);
    let responses_failed = harness::responses_sse(&[
        responses_created.clone(),
        json!({"type": "response.failed", "response": {
            "id": "resp_1", "status": "failed",
            "error": {"code": "server_error", "message": "boom"},
            "usage": responses_usage}}),
    ]);

    let completions_chunk = |delta: Value| {
        json!({"id": "gen-1", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": delta, "finish_reason": null}]})
    };
    let completions_usage = json!({"id": "gen-1", "choices": [],
        "usage": {"prompt_tokens": 10, "completion_tokens": 3,
            "prompt_tokens_details": {"cached_tokens": 4}}});
    let completions_stalled_chunk = json!({"id": "gen-1",
        "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"content": "Hel"}, "finish_reason": null}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 3,
            "prompt_tokens_details": {"cached_tokens": 4}}});
    let completions_stalled = format!("data: {completions_stalled_chunk}\n\n").into_bytes();
    let completions_failed = {
        let mut out = format!(
            "data: {}\n\ndata: {}\n\n",
            completions_chunk(json!({"content": "Hi"})),
            completions_usage,
        );
        out.push_str(&format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"error": {"message": "boom", "code": "server_error"}})
        ));
        out.into_bytes()
    };
    let completions_cut = format!("data: {}\n\n", completions_chunk(json!({"content": "Hi"})))
        .into_bytes();

    let gemini_chunk = |text: &str, usage: Option<Value>| {
        let mut chunk = json!({"candidates": [{"content": {"role": "model",
            "parts": [{"text": text}]}, "index": 0}],
            "responseId": "r1"});
        if let Some(usage) = usage {
            chunk["usageMetadata"] = usage;
        }
        chunk
    };
    let gemini_usage = json!({"promptTokenCount": 10, "candidatesTokenCount": 3});
    let gemini_stalled = harness::gemini_sse(&[gemini_chunk("Hel", Some(gemini_usage.clone()))]);
    let gemini_failed = harness::gemini_sse(&[
        gemini_chunk("Hi", Some(gemini_usage.clone())),
        json!({"error": {"message": "boom", "status": "UNAVAILABLE"}}),
    ]);

    let protocols = harness::protocols();
    let [anthropic, responses, completions, gemini] = protocols;
    [
        Wire {
            protocol: anthropic,
            stalled: anthropic_stalled,
            stalled_usage: (named("msg_1"), tokens(7, 0, 2)),
            failed: harness::anthropic_sse(&[
                anthropic_started(Some(anthropic_usage)),
                json!({"type": "error",
                    "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            ]),
            failed_usage: (named("msg_1"), tokens(7, 0, 2)),
            cut: harness::anthropic_sse(&[anthropic_started(None)]),
            cut_generation: GenerationId("msg_1".into()),
            error_first: harness::anthropic_sse(&[json!({"type": "error",
                "error": {"type": "invalid_request_error", "message": "bad"}})]),
        },
        Wire {
            protocol: responses,
            stalled: responses_stalled,
            stalled_usage: (named("resp_1"), tokens(0, 0, 0)),
            failed: responses_failed,
            failed_usage: (named("resp_1"), tokens(6, 4, 3)),
            cut: harness::responses_sse(&[responses_created]),
            cut_generation: GenerationId("resp_1".into()),
            error_first: harness::responses_sse(&[json!({"type": "error",
                "code": "server_error", "message": "boom"})]),
        },
        Wire {
            protocol: completions,
            stalled: completions_stalled,
            stalled_usage: (named("gen-1"), tokens(6, 4, 3)),
            failed: completions_failed,
            failed_usage: (named("gen-1"), tokens(6, 4, 3)),
            cut: completions_cut,
            cut_generation: GenerationId("gen-1".into()),
            error_first: format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"error": {"message": "boom", "code": "server_error"}})
            )
            .into_bytes(),
        },
        Wire {
            protocol: gemini,
            stalled: gemini_stalled,
            stalled_usage: (named("r1"), tokens(10, 0, 3)),
            failed: gemini_failed,
            failed_usage: (named("r1"), tokens(10, 0, 3)),
            cut: harness::gemini_sse(&[gemini_chunk("Hi", None)]),
            cut_generation: GenerationId("r1".into()),
            error_first: harness::gemini_sse(&[
                json!({"error": {"message": "boom", "status": "INVALID_ARGUMENT"}})
            ]),
        },
    ]
}

fn body_len(server: &ProviderServer) -> u64 {
    u64::try_from(server.requests()[0].body.len()).unwrap()
}

/// What a call that saw no generation and no usage carries: the body it
/// sent, and nothing else.
fn unnamed(server: &ProviderServer) -> CallUsage {
    CallUsage::unnamed(InputSize {
        bytes: body_len(server),
        media: false,
    })
}

/// Every call in order under one [`DEADLINE`], each of which must fail:
/// one deadline however many calls the protocol needs.
fn failures(calls: Vec<Box<dyn ModelCall>>) -> Vec<(contract::shapes::Failure, Option<bool>)> {
    fakes::within("the calls to fail", DEADLINE, move || {
        calls
            .into_iter()
            .map(|call| {
                let mut deltas = Vec::new();
                let reply = call.run(&mut |d: Delta| deltas.push(d));
                let Err(CallError::Failed {
                    failure,
                    should_retry,
                    ..
                }) = reply
                else {
                    panic!("expected a failure");
                };
                (failure, should_retry)
            })
            .collect()
    })
}

/// An endpoint whose connection is refused: the harness endpoint with only
/// the base URL swapped, so its credentials stay the harness's. The started
/// server is never contacted.
fn refused_endpoint(provider: &str) -> Endpoint {
    let server = ProviderServer::start(Vec::<Response>::new()).unwrap();
    Endpoint {
        base_url: format!("{}/v1", fakes::refused::url()),
        ..harness::endpoint(provider, &server)
    }
}

#[test]
fn a_call_cancelled_before_it_runs_returns_without_connecting() {
    for wire in wires() {
        let server = ProviderServer::start([]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let call = (wire.protocol.call)(&endpoint, &harness::request());
        call.cancel();
        let (result, _) = harness::run(call);
        let Err(CallError::Cancelled { usage }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        // Nothing was read, so the call carries only the body it built.
        assert!(usage.input_size.bytes > 0, "{}", wire.protocol.name);
        assert_eq!(
            *usage,
            CallUsage::unnamed(usage.input_size),
            "{}",
            wire.protocol.name
        );
        assert!(server.requests().is_empty(), "{}", wire.protocol.name);
    }
}

#[test]
fn cancelling_from_another_thread_ends_a_blocked_read_and_carries_what_it_saw() {
    for wire in wires() {
        let server = ProviderServer::start([Response::stall(
            200,
            wire.stalled.clone(),
            wire.stalled.len() + 1024,
        )
        .header("content-type", "text/event-stream")])
        .unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let call: Arc<dyn ModelCall> =
            Arc::from((wire.protocol.call)(&endpoint, &harness::request()));
        let (first, seen) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let runner = Arc::clone(&call);
        thread::spawn(move || {
            let result = runner.run(&mut |d| first.send(d).unwrap());
            done.send(result).unwrap();
        });
        let delta = Deadline::after(DEADLINE)
            .recv(&seen)
            .expect("a delta in time");
        assert_eq!(
            delta,
            Delta::Text(TextDelta { text: "Hel".into() }),
            "{}",
            wire.protocol.name
        );
        // The reader is now blocked waiting for the next bytes.
        call.cancel();
        let result = Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("run returned");
        let Err(CallError::Cancelled { usage }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        assert_eq!(usage.generation_id, wire.stalled_usage.0, "{}", wire.protocol.name);
        assert_eq!(usage.tokens, wire.stalled_usage.1, "{}", wire.protocol.name);
        assert_eq!(usage.web_searches, None, "{}", wire.protocol.name);
        assert_eq!(usage.input_size.bytes, body_len(&server), "{}", wire.protocol.name);
        assert!(!usage.input_size.media, "{}", wire.protocol.name);
        assert!(
            server.await_closed(1, DEADLINE),
            "{}: waited for the server to see the client close",
            wire.protocol.name
        );
    }
}

#[test]
fn a_connection_closed_before_any_response_fails_the_call() {
    for wire in wires() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            // Accept, read the request, and close without answering.
            let (socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                line.clear();
            }
        });
        let endpoint = Endpoint {
            provider: wire.protocol.name.into(),
            base_url: url,
            direct: true,
            ..Endpoint::default()
        };
        let (result, _) =
            harness::run((wire.protocol.call)(&endpoint, &harness::request()));
        let Err(CallError::Failed { failure, .. }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        assert_eq!(failure.code, ErrorCode::ConnectionFailed, "{}", wire.protocol.name);
    }
}

#[test]
fn a_status_other_than_2xx_fails_with_its_code_and_the_providers_words() {
    for wire in wires() {
        let server = ProviderServer::start([
            Response::status(
                429,
                json!({"type": "error",
                    "error": {"type": "rate_limit_error", "message": "Slow down."}})
                .to_string(),
            )
            .header("retry-after", "7"),
            Response::status(400, json!({"detail": "Unsupported parameter: temperature"}).to_string()),
            Response::status(401, "nope"),
            Response::status(529, "{}").header("x-should-retry", "false"),
            Response::status(503, "{}"),
        ])
        .unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let mut failures = Vec::new();
        let mut should_retry = Vec::new();
        for _ in 0..5 {
            let Err(CallError::Failed {
                failure,
                should_retry: header,
                ..
            }) = harness::run((wire.protocol.call)(&endpoint, &harness::request())).0
            else {
                panic!("{}: expected a failure", wire.protocol.name);
            };
            failures.push(failure);
            should_retry.push(header);
        }
        assert_eq!(should_retry, [None, None, None, Some(false), None], "{}", wire.protocol.name);
        assert_eq!(
            failures[0].message,
            format!("{} answered HTTP 429.", wire.protocol.name),
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            failures[2].message,
            format!(
                "{} rejected the credential (HTTP 401). Check the key it is configured \
                 with, or log in again with `fiber login {}`.",
                wire.protocol.name, wire.protocol.name
            ),
            "{}",
            wire.protocol.name
        );
        assert_eq!(failures[0].code, ErrorCode::RateLimited, "{}", wire.protocol.name);
        assert_eq!(failures[0].retry_after_ms, Some(7000), "{}", wire.protocol.name);
        assert_eq!(
            failures[0].provider.as_ref().unwrap().message,
            "Slow down.",
            "{}",
            wire.protocol.name
        );
        assert_eq!(failures[1].code, ErrorCode::InvalidRequest, "{}", wire.protocol.name);
        assert_eq!(
            failures[1].provider.as_ref().unwrap().message,
            "Unsupported parameter: temperature",
            "{}",
            wire.protocol.name
        );
        assert_eq!(failures[2].code, ErrorCode::AuthenticationFailed, "{}", wire.protocol.name);
        assert_eq!(failures[3].code, ErrorCode::ProviderUnavailable, "{}", wire.protocol.name);
        assert_eq!(failures[4].code, ErrorCode::ProviderUnavailable, "{}", wire.protocol.name);
    }
}

#[test]
fn timeout_conflict_and_server_errors_are_provider_unavailable() {
    for wire in wires() {
        let server = ProviderServer::start(
            [408, 409, 500, 503].map(|status| Response::status(status, "{}")),
        )
        .unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let calls: Vec<Box<dyn ModelCall>> = [408, 409, 500, 503]
            .iter()
            .map(|_| (wire.protocol.call)(&endpoint, &harness::request()))
            .collect();
        let got = failures(calls);
        for (index, status) in [408, 409, 500, 503].into_iter().enumerate() {
            let (failure, should_retry) = &got[index];
            assert_eq!(failure.code, ErrorCode::ProviderUnavailable, "{status}");
            assert_eq!(should_retry, &None, "{status}");
            assert_eq!(failure.retry_after_ms, None, "{status}");
            assert_eq!(
                failure.provider.as_ref().unwrap().status.unwrap(),
                status,
                "{status}"
            );
        }
    }
}

#[test]
fn a_503_carries_retry_after_and_a_500_carries_x_should_retry() {
    for wire in wires() {
        let server = ProviderServer::start([
            Response::status(503, "{}").header("retry-after", "7"),
            Response::status(500, "{}").header("x-should-retry", "true"),
        ])
        .unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let calls: Vec<Box<dyn ModelCall>> = [0, 1]
            .iter()
            .map(|_| (wire.protocol.call)(&endpoint, &harness::request()))
            .collect();
        let got = failures(calls);
        let (failure, should_retry) = &got[0];
        assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
        assert_eq!(should_retry, &None);
        assert_eq!(failure.retry_after_ms, Some(7000));
        let (failure, should_retry) = &got[1];
        assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
        assert_eq!(should_retry, &Some(true));
        assert_eq!(failure.retry_after_ms, None);
    }
}

#[test]
fn a_429_with_retry_after_2_records_retry_after_ms_2000() {
    for wire in wires() {
        let server =
            ProviderServer::start([Response::status(429, "{}").header("retry-after", "2")])
                .unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let got = failures(vec![(wire.protocol.call)(&endpoint, &harness::request())]);
        let (failure, _) = &got[0];
        assert_eq!(failure.code, ErrorCode::RateLimited, "{}", wire.protocol.name);
        let value = serde_json::to_value(failure).unwrap();
        assert_eq!(
            value.get("retry_after_ms"),
            Some(&serde_json::json!(2000)),
            "{}",
            wire.protocol.name
        );
        assert!(value.get("retry_after").is_none(), "{}", wire.protocol.name);
    }
}

#[test]
fn a_refused_connection_is_connection_failed() {
    for wire in wires() {
        let endpoint = refused_endpoint(wire.protocol.name);
        let got = failures(vec![(wire.protocol.call)(&endpoint, &harness::request())]);
        let (failure, should_retry) = &got[0];
        assert_eq!(failure.code, ErrorCode::ConnectionFailed);
        assert_eq!(should_retry, &None);
    }
}

#[test]
fn a_stream_that_fails_after_its_usage_carries_what_it_saw() {
    for wire in wires() {
        let server = ProviderServer::start([Response::stream(wire.failed.clone())]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let (result, _) = harness::run((wire.protocol.call)(&endpoint, &harness::request()));
        let Err(CallError::Failed { usage, .. }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        assert_eq!(usage.generation_id, wire.failed_usage.0, "{}", wire.protocol.name);
        assert_eq!(usage.tokens, wire.failed_usage.1, "{}", wire.protocol.name);
        assert_eq!(usage.web_searches, None, "{}", wire.protocol.name);
        assert_eq!(usage.input_size.bytes, body_len(&server), "{}", wire.protocol.name);
        assert!(!usage.input_size.media, "{}", wire.protocol.name);
    }
}

#[test]
fn a_stream_closed_after_its_generation_carries_zero_counts() {
    for wire in wires() {
        let server = ProviderServer::start([Response::stream(wire.cut.clone())]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let (result, _) = harness::run((wire.protocol.call)(&endpoint, &harness::request()));
        let Err(CallError::Failed { usage, .. }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        assert_eq!(usage.generation_id, Some(wire.cut_generation.clone()), "{}", wire.protocol.name);
        assert_eq!(usage.tokens, tokens(0, 0, 0), "{}", wire.protocol.name);
        assert_eq!(usage.input_size.bytes, body_len(&server), "{}", wire.protocol.name);
        assert!(!usage.input_size.media, "{}", wire.protocol.name);
    }
}

#[test]
fn an_http_error_carries_an_unnamed_usage() {
    for wire in wires() {
        let server = ProviderServer::start([Response::status(500, "boom")]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let (result, _) = harness::run((wire.protocol.call)(&endpoint, &harness::request()));
        let Err(CallError::Failed { usage, .. }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        assert_eq!(*usage, unnamed(&server), "{}", wire.protocol.name);
    }
}

#[test]
fn an_error_before_any_generation_carries_an_unnamed_usage() {
    for wire in wires() {
        let server = ProviderServer::start([Response::stream(wire.error_first.clone())]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let (result, _) = harness::run((wire.protocol.call)(&endpoint, &harness::request()));
        let Err(CallError::Failed { usage, .. }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        assert_eq!(*usage, unnamed(&server), "{}", wire.protocol.name);
    }
}

#[test]
fn a_call_cancelled_before_its_generation_carries_an_unnamed_usage() {
    for wire in wires() {
        let server =
            ProviderServer::start([Response::stall(200, b": keep-alive\n\n".to_vec(), 1024)
                .header("content-type", "text/event-stream")])
            .unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let call: Arc<dyn ModelCall> =
            Arc::from((wire.protocol.call)(&endpoint, &harness::request()));
        let (done, finished) = mpsc::channel();
        let runner = Arc::clone(&call);
        thread::spawn(move || done.send(runner.run(&mut |_| {})).unwrap());
        assert!(server.await_partial(1, DEADLINE), "{}", wire.protocol.name);
        call.cancel();
        let result = Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("run returned");
        let Err(CallError::Cancelled { usage }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        assert_eq!(*usage, unnamed(&server), "{}", wire.protocol.name);
    }
}
