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

#[path = "support/wire_tools.rs"]
mod wire_tools;

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread;

use contract::events::TextDelta;
use contract::provider::{
    CallError, CallUsage, Delta, ImageRef, Input, InputSize, ModelCall, ModelRequest,
};
use contract::shapes::Tokens;
use contract::{ErrorCode, GenerationId};
use fakes::{Deadline, ProviderServer, Response, TempDir};
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
    /// The call built after declaring `header` as the cache-key header.
    keyed: fn(&Endpoint, &ModelRequest, &str) -> Box<dyn ModelCall>,
    /// The sent tool list in a request body.
    tools: fn(&Value) -> Value,
    /// Two tool definitions in the protocol's own shape, `b_tool` then
    /// `a_tool`.
    sent_tools: Vec<Value>,
    /// What the protocol sends for a request limit of 1.
    limit_one: u64,
    /// The request's output limit in a body.
    output_limit: fn(&Value) -> Value,
    /// The first user message's text parts and image-part count in a body.
    user: fn(&Value) -> (Vec<String>, usize),
    /// A reply the protocol decodes into `Ok`, for rows that assert on the
    /// request they sent.
    completed: fn() -> Response,
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
    let completions_cut =
        format!("data: {}\n\n", completions_chunk(json!({"content": "Hi"}))).into_bytes();

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
            keyed: anthropic_keyed,
            tools: body_tools,
            sent_tools: vec![
                json!({"name": "b_tool", "description": "Second.",
                    "input_schema": {"type": "object"}, "strict": true}),
                json!({"name": "a_tool", "description": "First.",
                    "input_schema": {"type": "object"}, "strict": false}),
            ],
            limit_one: 1,
            output_limit: anthropic_limit,
            user: anthropic_user,
            completed: harness::anthropic_completed,
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
            keyed: responses_keyed,
            tools: body_tools,
            sent_tools: vec![
                json!({"type": "function", "name": "b_tool", "description": "Second.",
                    "parameters": {"type": "object"}, "strict": true}),
                json!({"type": "function", "name": "a_tool", "description": "First.",
                    "parameters": {"type": "object"}, "strict": false}),
            ],
            limit_one: 16,
            output_limit: responses_limit,
            user: responses_user,
            completed: responses_completed,
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
            keyed: completions_keyed,
            tools: body_tools,
            sent_tools: vec![
                json!({"type": "function", "function": {"name": "b_tool",
                    "description": "Second.", "parameters": {"type": "object"},
                    "strict": true}}),
                json!({"type": "function", "function": {"name": "a_tool",
                    "description": "First.", "parameters": {"type": "object"},
                    "strict": false}}),
            ],
            limit_one: 1,
            output_limit: completions_limit,
            user: completions_user,
            completed: harness::completions_completed,
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
                json!({"error": {"message": "boom", "status": "INVALID_ARGUMENT"}}),
            ]),
            keyed: gemini_keyed,
            tools: gemini_tools,
            sent_tools: vec![
                json!({"name": "b_tool", "description": "Sent.",
                    "parametersJsonSchema": {"type": "object"}}),
                json!({"name": "a_tool", "description": "Sent.",
                    "parametersJsonSchema": {"type": "object"}}),
            ],
            limit_one: 1,
            output_limit: gemini_limit,
            user: gemini_user,
            completed: harness::gemini_completed,
        },
    ]
}

/// The call built after declaring `header` as the cache-key header.
fn anthropic_keyed(
    endpoint: &Endpoint,
    request: &ModelRequest,
    header: &str,
) -> Box<dyn ModelCall> {
    Box::new(
        provider::anthropic_messages::Messages::new(endpoint.clone())
            .cache_key_header(header)
            .request(request),
    )
}

/// The call built after declaring `header` as the cache-key header.
fn responses_keyed(
    endpoint: &Endpoint,
    request: &ModelRequest,
    header: &str,
) -> Box<dyn ModelCall> {
    Box::new(
        provider::openai_responses::Responses::new(endpoint.clone())
            .cache_key_header(header)
            .request(request),
    )
}

/// The call built after declaring `header` as the cache-key header.
fn completions_keyed(
    endpoint: &Endpoint,
    request: &ModelRequest,
    header: &str,
) -> Box<dyn ModelCall> {
    Box::new(
        provider::openai_completions::Completions::new(endpoint.clone())
            .cache_key_header(header)
            .request(request),
    )
}

/// The call built after declaring `header` as the cache-key header.
fn gemini_keyed(endpoint: &Endpoint, request: &ModelRequest, header: &str) -> Box<dyn ModelCall> {
    Box::new(
        provider::google_generative_ai::Gemini::new(endpoint.clone())
            .cache_key_header(header)
            .request(request),
    )
}

/// A reply the Responses protocol decodes into `Ok`.
fn responses_completed() -> Response {
    Response::stream(harness::responses_sse(&[
        json!({"type": "response.completed",
        "response": {"id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10,
                "input_tokens_details": {"cached_tokens": 4},
                "output_tokens": 3}}} ),
    ]))
}

/// The sent tool list in a request body, except Gemini's, which nests its
/// declarations under one entry per tool kind.
fn body_tools(body: &Value) -> Value {
    body["tools"].clone()
}

/// The sent declarations in a Gemini body.
fn gemini_tools(body: &Value) -> Value {
    body["tools"][0]["functionDeclarations"].clone()
}

/// The request's output limit in an Anthropic body.
fn anthropic_limit(body: &Value) -> Value {
    body["max_tokens"].clone()
}

/// The request's output limit in a Responses body.
fn responses_limit(body: &Value) -> Value {
    body["max_output_tokens"].clone()
}

/// The request's output limit in a Completions body.
fn completions_limit(body: &Value) -> Value {
    body["max_completion_tokens"].clone()
}

/// The request's output limit in a Gemini body.
fn gemini_limit(body: &Value) -> Value {
    body["generationConfig"]["maxOutputTokens"].clone()
}

/// The first user message's text parts and image-part count. `content` is a
/// plain string when the message carries no parts, otherwise blocks whose
/// text `text_of` finds and whose images `is_image` names.
fn user_parts(
    content: &Value,
    text_of: fn(&Value) -> Option<String>,
    is_image: fn(&Value) -> bool,
) -> (Vec<String>, usize) {
    match content {
        Value::String(text) => (vec![text.clone()], 0),
        Value::Array(blocks) => {
            let mut texts = Vec::new();
            let mut images = 0;
            for block in blocks {
                if is_image(block) {
                    images += 1;
                } else if let Some(text) = text_of(block) {
                    texts.push(text);
                }
            }
            (texts, images)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
            panic!("a user message is a string or blocks, got {content}")
        }
    }
}

/// The text of an Anthropic block, when it carries text.
fn anthropic_text(block: &Value) -> Option<String> {
    (block["type"] == "text").then(|| block["text"].as_str().unwrap().to_owned())
}

/// Whether an Anthropic block is an image.
fn anthropic_image(block: &Value) -> bool {
    block["type"] == "image"
}

/// The first user message's text parts and image-part count in an
/// Anthropic body.
fn anthropic_user(body: &Value) -> (Vec<String>, usize) {
    let content = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"]
        .clone();
    user_parts(&content, anthropic_text, anthropic_image)
}

/// The text of a Responses block, when it carries text.
fn responses_text(block: &Value) -> Option<String> {
    (block["type"] == "input_text").then(|| block["text"].as_str().unwrap().to_owned())
}

/// Whether a Responses block is an image.
fn responses_image(block: &Value) -> bool {
    block["type"] == "input_image"
}

/// The first user message's text parts and image-part count in a Responses
/// body.
fn responses_user(body: &Value) -> (Vec<String>, usize) {
    let content = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["role"] == "user")
        .unwrap()["content"]
        .clone();
    user_parts(&content, responses_text, responses_image)
}

/// The text of a Completions block, when it carries text.
fn completions_text(block: &Value) -> Option<String> {
    (block["type"] == "text").then(|| block["text"].as_str().unwrap().to_owned())
}

/// Whether a Completions block is an image.
fn completions_image(block: &Value) -> bool {
    block["type"] == "image_url"
}

/// The first user message's text parts and image-part count in a
/// Completions body. The first message is the system one, so the user
/// message is found, not assumed first.
fn completions_user(body: &Value) -> (Vec<String>, usize) {
    let content = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"]
        .clone();
    user_parts(&content, completions_text, completions_image)
}

/// The text of a Gemini part, when it carries text.
fn gemini_text(block: &Value) -> Option<String> {
    block.get("text")?.as_str().map(str::to_owned)
}

/// Whether a Gemini part is inline image data.
fn gemini_image(block: &Value) -> bool {
    block.get("inlineData").is_some()
}

/// The first user message's text parts and image-part count in a Gemini
/// body.
fn gemini_user(body: &Value) -> (Vec<String>, usize) {
    let content = body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["parts"]
        .clone();
    user_parts(&content, gemini_text, gemini_image)
}

/// A session dir holding `artifacts/i_1.png` (`b"abcd"`), with a request
/// whose one user message carries `text` with that image.
fn imaged_user_request(text: &str) -> (TempDir, ModelRequest) {
    let session = TempDir::new("fiber-common-request-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: vec![Input::User {
            text: text.into(),
            images: vec![ImageRef {
                path: "artifacts/i_1.png".into(),
                mime_type: "image/png".into(),
                width: 2,
                height: 1,
            }],
        }],
        session_dir: session.path().to_path_buf(),
        ..harness::request()
    };
    (session, request)
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
fn failures(
    protocol: &'static str,
    calls: Vec<Box<dyn ModelCall>>,
) -> Vec<(contract::shapes::Failure, Option<bool>)> {
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
                    panic!("{protocol}: expected a failure");
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
        assert_eq!(
            usage.generation_id, wire.stalled_usage.0,
            "{}",
            wire.protocol.name
        );
        assert_eq!(usage.tokens, wire.stalled_usage.1, "{}", wire.protocol.name);
        assert_eq!(usage.web_searches, None, "{}", wire.protocol.name);
        assert_eq!(
            usage.input_size.bytes,
            body_len(&server),
            "{}",
            wire.protocol.name
        );
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
        let (result, _) = harness::run((wire.protocol.call)(&endpoint, &harness::request()));
        let Err(CallError::Failed { failure, .. }) = result else {
            panic!("{}: {result:?}", wire.protocol.name);
        };
        assert_eq!(
            failure.code,
            ErrorCode::ConnectionFailed,
            "{}",
            wire.protocol.name
        );
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
            Response::status(
                400,
                json!({"detail": "Unsupported parameter: temperature"}).to_string(),
            ),
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
        assert_eq!(
            should_retry,
            [None, None, None, Some(false), None],
            "{}",
            wire.protocol.name
        );
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
        assert_eq!(
            failures[0].code,
            ErrorCode::RateLimited,
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            failures[0].retry_after_ms,
            Some(7000),
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            failures[0].provider.as_ref().unwrap().message,
            "Slow down.",
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            failures[1].code,
            ErrorCode::InvalidRequest,
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            failures[1].provider.as_ref().unwrap().message,
            "Unsupported parameter: temperature",
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            failures[2].code,
            ErrorCode::AuthenticationFailed,
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            failures[3].code,
            ErrorCode::ProviderUnavailable,
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            failures[4].code,
            ErrorCode::ProviderUnavailable,
            "{}",
            wire.protocol.name
        );
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
        let got = failures(wire.protocol.name, calls);
        for (index, status) in [408, 409, 500, 503].into_iter().enumerate() {
            let (failure, should_retry) = &got[index];
            assert_eq!(
                failure.code,
                ErrorCode::ProviderUnavailable,
                "{}: {status}",
                wire.protocol.name
            );
            assert_eq!(should_retry, &None, "{}: {status}", wire.protocol.name);
            assert_eq!(
                failure.retry_after_ms, None,
                "{}: {status}",
                wire.protocol.name
            );
            assert_eq!(
                failure.provider.as_ref().unwrap().status.unwrap(),
                status,
                "{}: {status}",
                wire.protocol.name
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
        let got = failures(wire.protocol.name, calls);
        let (failure, should_retry) = &got[0];
        assert_eq!(
            failure.code,
            ErrorCode::ProviderUnavailable,
            "{}",
            wire.protocol.name
        );
        assert_eq!(should_retry, &None, "{}", wire.protocol.name);
        assert_eq!(failure.retry_after_ms, Some(7000), "{}", wire.protocol.name);
        let (failure, should_retry) = &got[1];
        assert_eq!(
            failure.code,
            ErrorCode::ProviderUnavailable,
            "{}",
            wire.protocol.name
        );
        assert_eq!(should_retry, &Some(true), "{}", wire.protocol.name);
        assert_eq!(failure.retry_after_ms, None, "{}", wire.protocol.name);
    }
}

#[test]
fn a_429_with_retry_after_2_records_retry_after_ms_2000() {
    for wire in wires() {
        let server =
            ProviderServer::start([Response::status(429, "{}").header("retry-after", "2")])
                .unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let got = failures(
            wire.protocol.name,
            vec![(wire.protocol.call)(&endpoint, &harness::request())],
        );
        let (failure, _) = &got[0];
        assert_eq!(
            failure.code,
            ErrorCode::RateLimited,
            "{}",
            wire.protocol.name
        );
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
        let got = failures(
            wire.protocol.name,
            vec![(wire.protocol.call)(&endpoint, &harness::request())],
        );
        let (failure, should_retry) = &got[0];
        assert_eq!(
            failure.code,
            ErrorCode::ConnectionFailed,
            "{}",
            wire.protocol.name
        );
        assert_eq!(should_retry, &None, "{}", wire.protocol.name);
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
        assert_eq!(
            usage.generation_id, wire.failed_usage.0,
            "{}",
            wire.protocol.name
        );
        assert_eq!(usage.tokens, wire.failed_usage.1, "{}", wire.protocol.name);
        assert_eq!(usage.web_searches, None, "{}", wire.protocol.name);
        assert_eq!(
            usage.input_size.bytes,
            body_len(&server),
            "{}",
            wire.protocol.name
        );
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
        assert_eq!(
            usage.generation_id,
            Some(wire.cut_generation.clone()),
            "{}",
            wire.protocol.name
        );
        assert_eq!(usage.tokens, tokens(0, 0, 0), "{}", wire.protocol.name);
        assert_eq!(
            usage.input_size.bytes,
            body_len(&server),
            "{}",
            wire.protocol.name
        );
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

#[test]
fn two_requests_built_from_the_same_inputs_are_the_same_bytes() {
    for wire in wires() {
        let mut request = harness::request();
        request.tools = wire_tools::wire_tools_fixture();
        let mut reordered = request.clone();
        reordered.tools.reverse();
        let server =
            ProviderServer::start([(wire.completed)(), (wire.completed)(), (wire.completed)()])
                .unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        for request in [&request, &request, &reordered] {
            harness::run((wire.protocol.call)(&endpoint, request))
                .0
                .unwrap();
        }
        let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
        assert_eq!(bodies.len(), 3, "{}", wire.protocol.name);
        assert_eq!(bodies[0], bodies[1], "{}", wire.protocol.name);
        assert_eq!(
            bodies[0], bodies[2],
            "{}: tools go in one list sorted by name",
            wire.protocol.name
        );
    }
}

#[test]
fn a_reply_carries_the_size_of_the_body_it_sent() {
    for wire in wires() {
        let (_session, request) = imaged_user_request("look");
        let server = ProviderServer::start([(wire.completed)(), (wire.completed)()]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let reply = harness::run((wire.protocol.call)(&endpoint, &request))
            .0
            .unwrap();
        assert_eq!(
            reply.input_size,
            InputSize {
                bytes: u64::try_from(server.requests()[0].body.len()).unwrap(),
                media: true,
            },
            "{}",
            wire.protocol.name
        );
        let endpoint = Endpoint {
            text_only: true,
            ..harness::endpoint(wire.protocol.name, &server)
        };
        let reply = harness::run((wire.protocol.call)(&endpoint, &request))
            .0
            .unwrap();
        assert_eq!(
            reply.input_size,
            InputSize {
                bytes: u64::try_from(server.requests()[1].body.len()).unwrap(),
                media: false,
            },
            "{}",
            wire.protocol.name
        );
    }
}

#[test]
fn sent_tools_are_sent_verbatim_in_order() {
    // A rewound session's first request carries its parent's logged build,
    // not what its own tools would wire (`docs/events.md`, "Rewind").
    for wire in wires() {
        let server = ProviderServer::start([(wire.completed)()]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let mut request = harness::request();
        request.sent_tools = Some(
            wire.sent_tools
                .iter()
                .map(|tool| tool.as_object().unwrap().clone())
                .collect(),
        );
        harness::run((wire.protocol.call)(&endpoint, &request))
            .0
            .unwrap();
        assert_eq!(
            (wire.tools)(&harness::sent_body(&server, 0)),
            Value::Array(wire.sent_tools.clone()),
            "{}",
            wire.protocol.name
        );
    }
}

#[test]
fn a_users_empty_text_sends_no_text_part() {
    for wire in wires() {
        let (_session, request) = imaged_user_request("");
        let server = ProviderServer::start([(wire.completed)()]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        harness::run((wire.protocol.call)(&endpoint, &request))
            .0
            .unwrap();
        assert_eq!(
            (wire.user)(&harness::sent_body(&server, 0)),
            (Vec::<String>::new(), 1),
            "{}",
            wire.protocol.name
        );
    }
}

#[test]
fn a_users_image_is_left_out_for_a_text_only_model() {
    for wire in wires() {
        let (_session, request) = imaged_user_request("look");
        let server = ProviderServer::start([(wire.completed)()]).unwrap();
        let endpoint = Endpoint {
            text_only: true,
            ..harness::endpoint(wire.protocol.name, &server)
        };
        harness::run((wire.protocol.call)(&endpoint, &request))
            .0
            .unwrap();
        assert_eq!(
            (wire.user)(&harness::sent_body(&server, 0)),
            (
                vec![
                    "look\n[Image artifacts/i_1.png left out: this model does not take images.]"
                        .to_owned()
                ],
                0
            ),
            "{}",
            wire.protocol.name
        );
    }
}

#[test]
fn a_users_unreadable_image_is_named_in_the_text_and_not_sent() {
    for wire in wires() {
        // No file is written: the image cannot be read.
        let session = TempDir::new("fiber-common-request-image");
        let imaged = |text: &str| ModelRequest {
            conversation: vec![Input::User {
                text: text.into(),
                images: vec![ImageRef {
                    path: "artifacts/gone.png".into(),
                    mime_type: "image/png".into(),
                    width: 2,
                    height: 1,
                }],
            }],
            session_dir: session.path().to_path_buf(),
            ..harness::request()
        };
        let server = ProviderServer::start([(wire.completed)(), (wire.completed)()]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        for text in ["look", "Image: 2x1 image/png.\n"] {
            let request = imaged(text);
            harness::run((wire.protocol.call)(&endpoint, &request))
                .0
                .unwrap();
        }
        assert_eq!(
            (wire.user)(&harness::sent_body(&server, 0)),
            (
                vec!["look\n[Image artifacts/gone.png could not be read.]".to_owned()],
                0
            ),
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            (wire.user)(&harness::sent_body(&server, 1)),
            (
                vec![
                    "Image: 2x1 image/png.\n[Image artifacts/gone.png could not be read.]"
                        .to_owned()
                ],
                0
            ),
            "{}",
            wire.protocol.name
        );
    }
}

#[test]
fn the_requests_own_output_limit_is_capped_by_the_models() {
    for wire in wires() {
        let server = ProviderServer::start([(wire.completed)(), (wire.completed)()]).unwrap();
        let endpoint = Endpoint {
            max_output_tokens: Some(4096),
            ..harness::endpoint(wire.protocol.name, &server)
        };
        for limit in [Some(1), Some(9000)] {
            let request = ModelRequest {
                max_output_tokens: limit,
                ..harness::request()
            };
            harness::run((wire.protocol.call)(&endpoint, &request))
                .0
                .unwrap();
        }
        assert_eq!(
            (wire.output_limit)(&harness::sent_body(&server, 0)),
            json!(wire.limit_one),
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            (wire.output_limit)(&harness::sent_body(&server, 1)),
            json!(4096),
            "{}",
            wire.protocol.name
        );
    }
}

#[test]
fn the_cache_key_goes_in_the_declared_header_and_nowhere_else_without_one() {
    for wire in wires() {
        let server = ProviderServer::start([(wire.completed)(), (wire.completed)()]).unwrap();
        let endpoint = harness::endpoint(wire.protocol.name, &server);
        let request = harness::request();
        harness::run((wire.protocol.call)(&endpoint, &request))
            .0
            .unwrap();
        harness::run((wire.keyed)(&endpoint, &request, "x-opencode-session"))
            .0
            .unwrap();
        let sent = server.requests();
        assert_eq!(
            sent[0].header("x-opencode-session"),
            None,
            "{}",
            wire.protocol.name
        );
        assert_eq!(
            sent[1].header("x-opencode-session"),
            Some("session_1"),
            "{}",
            wire.protocol.name
        );
    }
}
