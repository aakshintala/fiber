//! A call that ends without a reply carries what it saw
//! (`docs/events.md`, "Usage and notices"): one block per protocol.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::{CacheLifetime, TextDelta};
use contract::provider::{CallError, Delta, Input, ModelCall, ModelRequest, Provider, Reply};
use contract::shapes::Tokens;
use fakes::{ProviderServer, Response};
use provider::Endpoint;
use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(10);

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: "You are terse.".into(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: "s_1".into(),
        conversation: vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
        previous_end: None,
        max_output_tokens: None,
        session_dir: std::path::PathBuf::new(),
    }
}

fn endpoint(provider: &str, model: &str, server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: provider.into(),
        model: model.into(),
        base_url: server.url(),
        key: None,
        direct: true,
        ..Endpoint::default()
    }
}

/// Runs `call` on its own thread, so a call that never returns fails the
/// test at the deadline instead of hanging it.
fn run(call: Box<dyn ModelCall>) -> (Result<Reply, CallError>, Vec<Delta>) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut deltas = Vec::new();
        let reply = call.run(&mut |d| deltas.push(d));
        done.send((reply, deltas)).unwrap();
    });
    finished
        .recv_timeout(DEADLINE)
        .expect("waited for the call to return")
}

fn body_len(server: &ProviderServer) -> u64 {
    u64::try_from(server.requests()[0].body.len()).unwrap()
}

fn assert_input_size(usage: &contract::provider::CallUsage, server: &ProviderServer) {
    assert_eq!(usage.input_size.bytes, body_len(server));
    assert!(!usage.input_size.media);
}

fn tokens(input: u64, cache_read: u64, output: u64) -> Tokens {
    Tokens {
        input,
        cache_read,
        cache_write: Default::default(),
        output,
    }
}

// Anthropic.

fn anthropic_stream(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect::<String>()
        .into_bytes()
}

fn anthropic_started(id: &str, usage: Option<Value>) -> Value {
    let mut message = json!({"id": id});
    if let Some(usage) = usage {
        message["usage"] = usage;
    }
    json!({"type": "message_start", "message": message})
}

fn anthropic_usage(input: u64, output: u64) -> Value {
    json!({"input_tokens": input, "output_tokens": output})
}

#[test]
fn anthropic_failed_after_usage_carries_what_it_saw() {
    let server = ProviderServer::start([Response::stream(anthropic_stream(&[
        anthropic_started("msg_1", Some(anthropic_usage(7, 2))),
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
    ]))])
    .unwrap();
    let call = provider::anthropic_messages::Messages::new(endpoint(
        "anthropic",
        "claude-sonnet-5-5",
        &server,
    ))
    .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "msg_1");
    assert_eq!(usage.tokens, tokens(7, 0, 2));
    assert_eq!(usage.web_searches, None);
    assert_input_size(&usage, &server);
}

#[test]
fn anthropic_closed_early_after_its_generation_carries_zero_counts() {
    let server = ProviderServer::start([Response::stream(anthropic_stream(&[anthropic_started(
        "msg_1", None,
    )]))])
    .unwrap();
    let call = provider::anthropic_messages::Messages::new(endpoint(
        "anthropic",
        "claude-sonnet-5-5",
        &server,
    ))
    .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "msg_1");
    assert_eq!(usage.tokens, tokens(0, 0, 0));
    assert_input_size(&usage, &server);
}

#[test]
fn anthropic_cancelled_after_usage_carries_what_it_saw() {
    let open = format!(
        "data: {}\n\n",
        anthropic_started("msg_1", Some(anthropic_usage(7, 2)))
    );
    let start = format!(
        "data: {}\n\n",
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}})
    );
    let delta = format!(
        "data: {}\n\n",
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hel"}})
    );
    let prefix = format!("{open}{start}{delta}").into_bytes();
    let server = ProviderServer::start([Response::stall(200, prefix.clone(), prefix.len() + 1024)
        .header("content-type", "text/event-stream")])
    .unwrap();
    let call: Arc<dyn ModelCall> = Arc::from(
        provider::anthropic_messages::Messages::new(endpoint(
            "anthropic",
            "claude-sonnet-5-5",
            &server,
        ))
        .call(&request()),
    );
    let (first, seen) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let runner = Arc::clone(&call);
    thread::spawn(move || {
        let result = runner.run(&mut |d| first.send(d).unwrap());
        done.send(result).unwrap();
    });
    let delta = seen.recv_timeout(DEADLINE).expect("a delta in time");
    assert_eq!(delta, Delta::Text(TextDelta { text: "Hel".into() }));
    call.cancel();
    let result = finished.recv_timeout(DEADLINE).expect("run returned");
    let Err(CallError::Cancelled { usage: Some(usage) }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "msg_1");
    assert_eq!(usage.tokens, tokens(7, 0, 2));
    assert_eq!(usage.web_searches, None);
    assert_input_size(&usage, &server);
}

#[test]
fn anthropic_failed_before_any_generation_carries_nothing() {
    let server = ProviderServer::start([Response::status(500, "boom")]).unwrap();
    let call = provider::anthropic_messages::Messages::new(endpoint(
        "anthropic",
        "claude-sonnet-5-5",
        &server,
    ))
    .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage: None, .. }) = result else {
        panic!("{result:?}");
    };
    let server = ProviderServer::start([Response::stream(anthropic_stream(&[
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": "bad"}}),
    ]))])
    .unwrap();
    let call = provider::anthropic_messages::Messages::new(endpoint(
        "anthropic",
        "claude-sonnet-5-5",
        &server,
    ))
    .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage: None, .. }) = result else {
        panic!("{result:?}");
    };
}

// Responses.

fn responses_stream(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect::<String>()
        .into_bytes()
}

fn responses_created(id: &str) -> Value {
    json!({"type": "response.created", "response": {"id": id, "status": "in_progress"}})
}

fn responses_failed(id: &str, usage: Value) -> Value {
    json!({"type": "response.failed", "response": {
        "id": id, "status": "failed",
        "error": {"code": "server_error", "message": "boom"},
        "usage": usage,
    }})
}

fn responses_usage() -> Value {
    json!({"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3})
}

#[test]
fn responses_failed_after_usage_carries_what_it_saw() {
    let server = ProviderServer::start([Response::stream(responses_stream(&[
        responses_created("resp_1"),
        responses_failed("resp_1", responses_usage()),
    ]))])
    .unwrap();
    let call = provider::openai_responses::Responses::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "resp_1");
    assert_eq!(usage.tokens, tokens(6, 4, 3));
    assert_input_size(&usage, &server);
}

#[test]
fn responses_closed_early_after_its_generation_carries_zero_counts() {
    let server = ProviderServer::start([Response::stream(responses_stream(&[responses_created(
        "resp_1",
    )]))])
    .unwrap();
    let call = provider::openai_responses::Responses::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "resp_1");
    assert_eq!(usage.tokens, tokens(0, 0, 0));
    assert_input_size(&usage, &server);
}

#[test]
fn responses_cancelled_after_its_generation_carries_zero_counts() {
    let prefix = responses_stream(&[
        responses_created("resp_1"),
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
    ]);
    let server = ProviderServer::start([Response::stall(200, prefix.clone(), prefix.len() + 1024)
        .header("content-type", "text/event-stream")])
    .unwrap();
    let call: Arc<dyn ModelCall> = Arc::from(
        provider::openai_responses::Responses::new(endpoint("openai", "gpt-5", &server))
            .call(&request()),
    );
    let (first, seen) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let runner = Arc::clone(&call);
    thread::spawn(move || {
        let result = runner.run(&mut |d| first.send(d).unwrap());
        done.send(result).unwrap();
    });
    let delta = seen.recv_timeout(DEADLINE).expect("a delta in time");
    assert_eq!(delta, Delta::Text(TextDelta { text: "Hel".into() }));
    call.cancel();
    let result = finished.recv_timeout(DEADLINE).expect("run returned");
    let Err(CallError::Cancelled { usage: Some(usage) }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "resp_1");
    assert_eq!(usage.tokens, tokens(0, 0, 0));
    assert_eq!(usage.web_searches, None);
    assert_input_size(&usage, &server);
}

#[test]
fn responses_failed_before_any_generation_carries_nothing() {
    let server = ProviderServer::start([Response::status(500, "boom")]).unwrap();
    let call = provider::openai_responses::Responses::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage: None, .. }) = result else {
        panic!("{result:?}");
    };
    let server = ProviderServer::start([Response::stream(responses_stream(&[
        json!({"type": "error", "code": "server_error", "message": "boom"}),
    ]))])
    .unwrap();
    let call = provider::openai_responses::Responses::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage: None, .. }) = result else {
        panic!("{result:?}");
    };
}

// Completions.

fn completions_chunk(content: &str) -> Value {
    json!({"id": "gen-1", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]})
}

fn completions_usage() -> Value {
    json!({"id": "gen-1", "choices": [], "usage": {"prompt_tokens": 10,
        "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 4}}})
}

#[test]
fn completions_failed_after_usage_carries_what_it_saw() {
    let body = {
        let mut out = format!(
            "data: {}\n\ndata: {}\n\n",
            completions_chunk("Hi"),
            completions_usage()
        );
        out.push_str(&format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"error": {"message": "boom", "code": "server_error"}})
        ));
        out.into_bytes()
    };
    let server = ProviderServer::start([Response::stream(body)]).unwrap();
    let call = provider::openai_completions::Completions::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "gen-1");
    assert_eq!(usage.tokens, tokens(6, 4, 3));
    assert_input_size(&usage, &server);
}

#[test]
fn completions_closed_early_after_its_generation_carries_zero_counts() {
    let out = format!("data: {}\n\n", completions_chunk("Hi")).into_bytes();
    // No `[DONE]`: the stream ends early.
    let server = ProviderServer::start([Response::stream(out)]).unwrap();
    let call = provider::openai_completions::Completions::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "gen-1");
    assert_eq!(usage.tokens, tokens(0, 0, 0));
    assert_input_size(&usage, &server);
}

#[test]
fn completions_cancelled_after_usage_carries_what_it_saw() {
    let chunk = json!({"id": "gen-1", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"content": "Hel"}, "finish_reason": null}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 3,
            "prompt_tokens_details": {"cached_tokens": 4}}});
    let prefix = format!("data: {chunk}\n\n").into_bytes();
    let server = ProviderServer::start([Response::stall(200, prefix.clone(), prefix.len() + 1024)
        .header("content-type", "text/event-stream")])
    .unwrap();
    let call: Arc<dyn ModelCall> = Arc::from(
        provider::openai_completions::Completions::new(endpoint("openai", "gpt-5", &server))
            .call(&request()),
    );
    let (first, seen) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let runner = Arc::clone(&call);
    thread::spawn(move || {
        let result = runner.run(&mut |d| first.send(d).unwrap());
        done.send(result).unwrap();
    });
    let delta = seen.recv_timeout(DEADLINE).expect("a delta in time");
    assert_eq!(delta, Delta::Text(TextDelta { text: "Hel".into() }));
    call.cancel();
    let result = finished.recv_timeout(DEADLINE).expect("run returned");
    let Err(CallError::Cancelled { usage: Some(usage) }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "gen-1");
    assert_eq!(usage.tokens, tokens(6, 4, 3));
    assert_eq!(usage.web_searches, None);
    assert_input_size(&usage, &server);
}

#[test]
fn completions_failed_before_any_generation_carries_nothing() {
    let server = ProviderServer::start([Response::status(500, "boom")]).unwrap();
    let call = provider::openai_completions::Completions::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage: None, .. }) = result else {
        panic!("{result:?}");
    };
    let body = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"error": {"message": "boom", "code": "server_error"}})
    )
    .into_bytes();
    let server = ProviderServer::start([Response::stream(body)]).unwrap();
    let call = provider::openai_completions::Completions::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage: None, .. }) = result else {
        panic!("{result:?}");
    };
}

// Gemini.

fn gemini_stream(chunks: &[Value]) -> Vec<u8> {
    chunks
        .iter()
        .map(|c| format!("data: {c}\r\n\r\n"))
        .collect::<String>()
        .into_bytes()
}

fn gemini_chunk(text: &str, usage: Option<Value>) -> Value {
    let mut chunk = json!({"candidates": [{"content": {"role": "model",
        "parts": [{"text": text}]}, "index": 0}],
        "responseId": "r1"});
    if let Some(usage) = usage {
        chunk["usageMetadata"] = usage;
    }
    chunk
}

fn gemini_usage() -> Value {
    json!({"promptTokenCount": 10, "candidatesTokenCount": 3})
}

#[test]
fn gemini_failed_after_usage_carries_what_it_saw() {
    let server = ProviderServer::start([Response::stream(gemini_stream(&[
        gemini_chunk("Hi", Some(gemini_usage())),
        json!({"error": {"message": "boom", "status": "UNAVAILABLE"}}),
    ]))])
    .unwrap();
    let call = provider::google_generative_ai::Gemini::new(endpoint("google", "gemini-3", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "r1");
    assert_eq!(usage.tokens, tokens(10, 0, 3));
    assert_input_size(&usage, &server);
}

#[test]
fn gemini_closed_early_after_its_generation_carries_zero_counts() {
    let server =
        ProviderServer::start([Response::stream(gemini_stream(&[gemini_chunk("Hi", None)]))])
            .unwrap();
    let call = provider::google_generative_ai::Gemini::new(endpoint("google", "gemini-3", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "r1");
    assert_eq!(usage.tokens, tokens(0, 0, 0));
    assert_input_size(&usage, &server);
}

#[test]
fn gemini_cancelled_after_usage_carries_what_it_saw() {
    let prefix = gemini_stream(&[gemini_chunk("Hel", Some(gemini_usage()))]);
    let server = ProviderServer::start([Response::stall(200, prefix.clone(), prefix.len() + 1024)
        .header("content-type", "text/event-stream")])
    .unwrap();
    let call: Arc<dyn ModelCall> = Arc::from(
        provider::google_generative_ai::Gemini::new(endpoint("google", "gemini-3", &server))
            .call(&request()),
    );
    let (first, seen) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let runner = Arc::clone(&call);
    thread::spawn(move || {
        let result = runner.run(&mut |d| first.send(d).unwrap());
        done.send(result).unwrap();
    });
    let delta = seen.recv_timeout(DEADLINE).expect("a delta in time");
    assert_eq!(delta, Delta::Text(TextDelta { text: "Hel".into() }));
    call.cancel();
    let result = finished.recv_timeout(DEADLINE).expect("run returned");
    let Err(CallError::Cancelled { usage: Some(usage) }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id.0, "r1");
    assert_eq!(usage.tokens, tokens(10, 0, 3));
    assert_eq!(usage.web_searches, None);
    assert_input_size(&usage, &server);
}

#[test]
fn gemini_failed_before_any_generation_carries_nothing() {
    let server = ProviderServer::start([Response::status(500, "boom")]).unwrap();
    let call = provider::google_generative_ai::Gemini::new(endpoint("google", "gemini-3", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage: None, .. }) = result else {
        panic!("{result:?}");
    };
    let server = ProviderServer::start([Response::stream(gemini_stream(&[
        json!({"error": {"message": "boom", "status": "INVALID_ARGUMENT"}}),
    ]))])
    .unwrap();
    let call = provider::google_generative_ai::Gemini::new(endpoint("google", "gemini-3", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage: None, .. }) = result else {
        panic!("{result:?}");
    };
}

#[test]
fn gemini_an_unrepresentable_output_count_is_zero_and_keeps_the_rest() {
    let usage = json!({"promptTokenCount": 50, "cachedContentTokenCount": 20,
        "candidatesTokenCount": u64::MAX, "thoughtsTokenCount": 1});
    let server = ProviderServer::start([Response::stream(gemini_stream(&[gemini_chunk(
        "Hi",
        Some(usage),
    )]))])
    .unwrap();
    let call = provider::google_generative_ai::Gemini::new(endpoint("google", "gemini-3", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed {
        usage: Some(usage), ..
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(usage.tokens.input, 30);
    assert_eq!(usage.tokens.cache_read, 20);
    assert_eq!(usage.tokens.output, 0);
    assert_input_size(&usage, &server);
}
