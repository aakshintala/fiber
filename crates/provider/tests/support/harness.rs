//! The helpers every provider test file copies: one request, one endpoint,
//! one way to run a call, and the four protocols' stream builders and
//! completed replies. A file whose fixture differs keeps a local `request()`
//! or `endpoint()` under the same name, whose body is only a struct update
//! over the harness value, so the field list exists once.

#![allow(dead_code, reason = "each test file uses only part of it")]

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::events::CacheLifetime;
use contract::provider::{CallError, Delta, Input, ModelCall, ModelRequest, Reply};
use fakes::{Deadline, ProviderServer, Response};
use provider::Endpoint;
use serde_json::{Value, json};

pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

pub(crate) fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: "You are terse.".into(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "session_1".into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        conversation: vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
        session_dir: std::path::PathBuf::new(),
    }
}

pub(crate) fn endpoint(provider: &str, server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: provider.into(),
        model: "m".into(),
        base_url: format!("{}/v1", server.url()),
        key: Some(contract::Secret::new("sk-secret".into())),
        direct: true,
        ..Endpoint::default()
    }
}

/// Runs `call` on its own thread, so a call that never returns fails the
/// test at the deadline instead of hanging it.
#[track_caller]
pub(crate) fn run(call: Box<dyn ModelCall>) -> (Result<Reply, CallError>, Vec<Delta>) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut deltas = Vec::new();
        let reply = call.run(&mut |d| deltas.push(d));
        done.send((reply, deltas)).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("waited for the call to return")
}

#[track_caller]
pub(crate) fn failed(call: Box<dyn ModelCall>) -> (contract::shapes::Failure, Option<bool>) {
    let (reply, _) = run(call);
    let Err(CallError::Failed {
        failure,
        should_retry,
        ..
    }) = reply
    else {
        panic!("expected a failure");
    };
    (failure, should_retry)
}

pub(crate) fn sent_body(server: &ProviderServer, n: usize) -> Value {
    serde_json::from_slice(&server.requests()[n].body).unwrap()
}

/// One protocol's calls, built from the endpoint and request the test hands
/// over, so every cross-protocol test runs the same four calls.
pub(crate) struct Protocol {
    pub(crate) name: &'static str,
    pub(crate) call: fn(&Endpoint, &ModelRequest) -> Box<dyn ModelCall>,
}

pub(crate) fn protocols() -> [Protocol; 4] {
    fn messages(endpoint: &Endpoint, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(provider::anthropic_messages::Messages::new(endpoint.clone()).request(request))
    }
    fn responses(endpoint: &Endpoint, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(provider::openai_responses::Responses::new(endpoint.clone()).request(request))
    }
    fn completions(endpoint: &Endpoint, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(provider::openai_completions::Completions::new(endpoint.clone()).request(request))
    }
    fn gemini(endpoint: &Endpoint, request: &ModelRequest) -> Box<dyn ModelCall> {
        Box::new(provider::google_generative_ai::Gemini::new(endpoint.clone()).request(request))
    }
    [
        Protocol {
            name: "anthropic",
            call: messages,
        },
        Protocol {
            name: "opencode",
            call: responses,
        },
        Protocol {
            name: "openrouter",
            call: completions,
        },
        Protocol {
            name: "gemini",
            call: gemini,
        },
    ]
}

/// An Anthropic stream of `data:` events, one per JSON value.
pub(crate) fn anthropic_sse(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect::<String>()
        .into_bytes()
}

/// A Responses stream of `data:` events, one per JSON value.
pub(crate) fn responses_sse(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect::<String>()
        .into_bytes()
}

/// A Completions stream of `data:` chunks, one per JSON value, then `[DONE]`.
pub(crate) fn completions_sse(chunks: &[Value]) -> Vec<u8> {
    let mut out: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    out.push_str("data: [DONE]\n\n");
    out.into_bytes()
}

/// A Gemini stream of `data:` events, one per JSON value.
pub(crate) fn gemini_sse(chunks: &[Value]) -> Vec<u8> {
    chunks
        .iter()
        .map(|c| format!("data: {c}\r\n\r\n"))
        .collect::<String>()
        .into_bytes()
}

pub(crate) fn anthropic_completed() -> Response {
    Response::stream(anthropic_sse(&[
        json!({"type": "message_start", "message": {"id": "msg_1"}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"input_tokens": 10, "cache_read_input_tokens": 4, "output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]))
}

pub(crate) fn completions_completed() -> Response {
    Response::stream(completions_sse(&[
        json!({"id": "gen-1", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hi"}, "finish_reason": null}]}),
        json!({"id": "gen-1", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        json!({"id": "gen-1", "choices": [], "usage": {"prompt_tokens": 10,
            "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 4}}}),
    ]))
}

pub(crate) fn gemini_completed() -> Response {
    Response::stream(gemini_sse(&[json!({"candidates": [{"content": {"role": "model",
        "parts": [{"text": "hi"}]}, "index": 0, "finishReason": "STOP"}],
        "responseId": "r1",
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 3}})]))
}
