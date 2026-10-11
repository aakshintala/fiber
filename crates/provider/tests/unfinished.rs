//! A call that ends without a reply carries what it saw
//! (`docs/events.md`, "Usage and notices"): one block per protocol.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

#[path = "support/harness.rs"]
mod harness;

use contract::GenerationId;
use contract::events::CacheLifetime;
use contract::provider::{CallError, ModelRequest, Provider};
use contract::shapes::Tokens;
use fakes::{ProviderServer, Response};
use provider::Endpoint;
use serde_json::{Value, json};

use harness::{gemini_sse as gemini_stream, responses_sse as responses_stream, run};

fn request() -> ModelRequest {
    ModelRequest {
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: "s_1".into(),
        ..harness::request()
    }
}

fn endpoint(provider: &str, model: &str, server: &ProviderServer) -> Endpoint {
    Endpoint {
        model: model.into(),
        base_url: server.url(),
        key: None,
        ..harness::endpoint(provider, server)
    }
}

fn body_len(server: &ProviderServer) -> u64 {
    u64::try_from(server.requests()[0].body.len()).unwrap()
}

fn assert_input_size(usage: &contract::provider::CallUsage, server: &ProviderServer) {
    assert_eq!(usage.input_size.bytes, body_len(server));
    assert!(!usage.input_size.media);
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

// Anthropic.

// Responses.

fn responses_created(id: &str) -> Value {
    json!({"type": "response.created", "response": {"id": id, "status": "in_progress"}})
}

fn responses_usage() -> Value {
    json!({"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3})
}

// Completions.

fn completions_chunk(content: &str) -> Value {
    json!({"id": "gen-1", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]})
}

fn completions_usage_with_write() -> Value {
    json!({"id": "gen-1", "choices": [], "usage": {"prompt_tokens": 18,
        "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 4,
            "cache_write_tokens": 8}}})
}

#[test]
fn completions_failed_after_cache_write_carries_it_under_the_hour_lifetime() {
    let body = {
        let mut out = format!(
            "data: {}\n\ndata: {}\n\n",
            completions_chunk("Hi"),
            completions_usage_with_write()
        );
        out.push_str(&format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"error": {"message": "boom", "code": "server_error"}})
        ));
        out.into_bytes()
    };
    let server = ProviderServer::start([Response::stream(body)]).unwrap();
    let request = ModelRequest {
        cache_lifetime: CacheLifetime::OneHour,
        ..request()
    };
    let call = provider::openai_completions::Completions::new(endpoint("openai", "gpt-5", &server))
        .call(&request);
    let (result, _) = run(call);
    let Err(CallError::Failed { usage, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id, named("gen-1"));
    assert_eq!(
        usage.tokens,
        Tokens {
            input: 6,
            cache_read: 4,
            cache_write: std::collections::BTreeMap::from([("1h".to_owned(), 8)]),
            output: 3,
        }
    );
    assert_input_size(&usage, &server);
}

// Gemini.

fn gemini_chunk(text: &str, usage: Option<Value>) -> Value {
    let mut chunk = json!({"candidates": [{"content": {"role": "model",
        "parts": [{"text": text}]}, "index": 0}],
        "responseId": "r1"});
    if let Some(usage) = usage {
        chunk["usageMetadata"] = usage;
    }
    chunk
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
    let Err(CallError::Failed { usage, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(usage.tokens.input, 30);
    assert_eq!(usage.tokens.cache_read, 20);
    assert_eq!(usage.tokens.output, 0);
    assert_input_size(&usage, &server);
}

#[test]
fn completions_cut_before_any_id_carries_the_tokens_it_saw() {
    let chunk = json!({"object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"content": "Hi"}, "finish_reason": null}],
        "usage": {"prompt_tokens": 7, "completion_tokens": 2}});
    let server =
        ProviderServer::start([Response::stream(format!("data: {chunk}\n\n").into_bytes())])
            .unwrap();
    let call = provider::openai_completions::Completions::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id, None);
    assert_eq!(usage.tokens, tokens(7, 0, 2));
    assert_input_size(&usage, &server);
}

#[test]
fn gemini_cut_before_any_id_carries_the_tokens_it_saw() {
    let chunk = json!({"candidates": [{"content": {"role": "model",
        "parts": [{"text": "Hi"}]}, "index": 0}],
        "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 2}});
    let server = ProviderServer::start([Response::stream(gemini_stream(&[chunk]))]).unwrap();
    let call = provider::google_generative_ai::Gemini::new(endpoint("google", "gemini-3", &server))
        .call(&request());
    let (result, _) = run(call);
    let Err(CallError::Failed { usage, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(usage.generation_id, None);
    assert_eq!(usage.tokens, tokens(7, 0, 2));
    assert_input_size(&usage, &server);
}

#[test]
fn completions_completed_without_an_id_names_no_generation() {
    let chunk = json!({"object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"content": "Hi"}, "finish_reason": "stop"}]});
    let body = format!("data: {chunk}\n\ndata: [DONE]\n\n").into_bytes();
    let server = ProviderServer::start([Response::stream(body)]).unwrap();
    let call = provider::openai_completions::Completions::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    let reply = result.unwrap();
    assert_eq!(reply.text(), "Hi");
    assert_eq!(reply.generation_id, None);
}

#[test]
fn gemini_completed_without_an_id_names_no_generation() {
    let chunk = json!({"candidates": [{"content": {"role": "model",
        "parts": [{"text": "Hi"}]}, "index": 0, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 2}});
    let server = ProviderServer::start([Response::stream(gemini_stream(&[chunk]))]).unwrap();
    let call = provider::google_generative_ai::Gemini::new(endpoint("google", "gemini-3", &server))
        .call(&request());
    let (result, _) = run(call);
    let reply = result.unwrap();
    assert_eq!(reply.text(), "Hi");
    assert_eq!(reply.generation_id, None);
}

#[test]
fn responses_keeps_the_id_it_saw_first_whatever_the_terminal_says() {
    for terminal in [json!({}), json!({"id": "resp_other"})] {
        let mut response = json!({"status": "completed", "usage": responses_usage()});
        response
            .as_object_mut()
            .unwrap()
            .extend(terminal.as_object().unwrap().clone());
        let server = ProviderServer::start([Response::stream(responses_stream(&[
            responses_created("resp_1"),
            json!({"type": "response.completed", "response": response}),
        ]))])
        .unwrap();
        let call = provider::openai_responses::Responses::new(endpoint("openai", "gpt-5", &server))
            .call(&request());
        let (result, _) = run(call);
        let reply = result.unwrap();
        assert_eq!(reply.generation_id, named("resp_1"), "{terminal}");
        assert_eq!(reply.tokens, tokens(6, 4, 3), "{terminal}");
    }
}

#[test]
fn responses_named_only_by_its_terminal_takes_that_id() {
    let server = ProviderServer::start([Response::stream(responses_stream(&[
        json!({"type": "response.created", "response": {"status": "in_progress"}}),
        json!({"type": "response.completed", "response": {
            "id": "resp_9", "status": "completed", "usage": responses_usage()}}),
    ]))])
    .unwrap();
    let call = provider::openai_responses::Responses::new(endpoint("openai", "gpt-5", &server))
        .call(&request());
    let (result, _) = run(call);
    assert_eq!(result.unwrap().generation_id, named("resp_9"));
}
