//! `google-generative-ai` through the provider crate's public API: the probe
//! recordings decoded, requests on the fake provider server, and
//! cancellation from another thread.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

#[path = "support/probes.rs"]
mod probes;

#[path = "support/wire_tools.rs"]
mod wire_tools;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::{
    CacheLifetime, ReasoningCompleted, TextCompleted, TextDelta, ToolCallRequested,
};
use contract::provider::{
    CallError, Delta, Finish, Input, ModelCall, ModelRequest, Provider, Reply, ReplyAction,
    ToolDefinition,
};
use contract::shapes::Tokens;
use contract::{ActionId, ErrorCode, ProviderCallId};
use fakes::{ProviderServer, Response, fingerprint};
use provider::Endpoint;
use provider::google_generative_ai::{Gemini, decode};
use serde_json::{Value, json};

use probes::Recorded;

const DEADLINE: Duration = Duration::from_secs(10);
const REFERENCE: &str = "gemini/gemini-3.1-flash-lite";

fn research(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../research/google-generative-ai-probe")
        .join(path)
}

fn decoded(bytes: &[u8]) -> (Result<Reply, provider::Error>, Vec<Delta>) {
    let mut deltas = Vec::new();
    let reply = decode(bytes, &mut |d| deltas.push(d));
    (reply, deltas)
}

fn recording(name: &str) -> Vec<u8> {
    let exchange = probes::read(&research(&format!("raw/{name}")))
        .unwrap()
        .remove(0);
    let Recorded::Stream(bytes) = exchange.response else {
        panic!("{name}: expected a stream");
    };
    bytes
}

fn endpoint(server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: "gemini".into(),
        model: "gemini-3.1-flash-lite".into(),
        base_url: format!("{}/v1beta", server.url()),
        key: Some("AIza-secret".into()),
        direct: true,
        ..Endpoint::default()
    }
}

fn weather_tool() -> ToolDefinition {
    ToolDefinition {
        name: "get_weather".into(),
        description: "Weather for a city.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
            "additionalProperties": false
        }),
        deferred: false,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: "You are terse.".into(),
        tools: vec![weather_tool()],
        effort: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "session_1".into(),
        previous_end: None,
        max_output_tokens: None,
        conversation: vec![Input::User {
            text: "What is the weather in Paris? Use the tool.".into(),
        }],
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

fn sent_body(server: &ProviderServer, n: usize) -> Value {
    serde_json::from_slice(&server.requests()[n].body).unwrap()
}

/// A stream of `data:` events, one per JSON value.
fn stream(chunks: &[Value]) -> Vec<u8> {
    chunks
        .iter()
        .map(|c| format!("data: {c}\r\n\r\n"))
        .collect::<String>()
        .into_bytes()
}

fn chunk(parts: Value, finish: Option<&str>) -> Value {
    let mut candidate = json!({"content": {"role": "model", "parts": parts}, "index": 0});
    if let Some(reason) = finish {
        candidate["finishReason"] = json!(reason);
    }
    json!({"candidates": [candidate], "responseId": "r1",
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 3}})
}

fn completed_reply() -> Response {
    Response::stream(stream(&[chunk(json!([{"text": "hi"}]), Some("STOP"))]))
}

fn error_code(result: Result<Reply, provider::Error>) -> (ErrorCode, String) {
    let error = result.unwrap_err();
    (error.code(), error.to_string())
}

/// What a recorded stream holds, read from its chunks independently of the
/// decoder: the text, the calls, the signatures and the last usage.
struct Expected {
    text: String,
    calls: Vec<Value>,
    signatures: usize,
    usage: Value,
}

fn expected(bytes: &[u8]) -> Expected {
    let mut want = Expected {
        text: String::new(),
        calls: Vec::new(),
        signatures: 0,
        usage: Value::Null,
    };
    for line in String::from_utf8_lossy(bytes).lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let chunk: Value = serde_json::from_str(data).unwrap();
        for part in chunk["candidates"][0]["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if part.get("thoughtSignature").is_some() {
                want.signatures += 1;
            }
            if let Some(call) = part.get("functionCall") {
                want.calls.push(call.clone());
            } else if let Some(text) = part["text"].as_str() {
                want.text.push_str(text);
            }
        }
        if let Some(usage) = chunk.get("usageMetadata") {
            want.usage = usage.clone();
        }
    }
    want
}

#[test]
fn every_probe_recording_decodes_into_the_actions_and_usage_it_holds() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(research("raw"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();

    let (mut streams, mut calls_seen, mut statuses) = (0, 0, 0);
    for file in &files {
        // `auth2-query-stream.txt` is the one stream saved as text, after a
        // `status 200` line.
        let exchanges: Vec<(String, Recorded)> = if file.extension().is_some_and(|e| e == "txt") {
            let text = std::fs::read_to_string(file).unwrap();
            let (status, sse) = text.split_once('\n').unwrap();
            assert_eq!(status, "status 200");
            vec![(
                file.display().to_string(),
                Recorded::Stream(sse.as_bytes().to_vec()),
            )]
        } else {
            probes::read(file)
                .unwrap()
                .into_iter()
                .map(|e| (e.label, e.response))
                .collect()
        };
        for (label, response) in exchanges {
            let bytes = match response {
                Recorded::Stream(bytes) => bytes,
                Recorded::Status(..) => {
                    statuses += 1;
                    continue;
                }
            };
            streams += 1;
            let (reply, deltas) = decoded(&bytes);
            let reply = reply.unwrap_or_else(|e| panic!("{label}: {e}"));
            assert!(
                matches!(reply.finish, Finish::Completed | Finish::OutputLimit),
                "{label}"
            );
            let want = expected(&bytes);

            let text: String = deltas
                .iter()
                .filter_map(|d| match d {
                    Delta::Text(t) => Some(t.text.as_str()),
                    Delta::Reasoning(_) | Delta::ToolCallArguments(_) => None,
                })
                .collect();
            assert_eq!(text, reply.text(), "{label}");
            assert_eq!(reply.text(), want.text, "{label}");

            let calls: Vec<&ToolCallRequested> = reply
                .actions
                .iter()
                .filter_map(|a| match a {
                    ReplyAction::ToolCall(c) => Some(c),
                    ReplyAction::Reasoning(_) | ReplyAction::Text(_) => None,
                })
                .collect();
            assert_eq!(calls.len(), want.calls.len(), "{label}");
            for (index, (call, item)) in calls.iter().zip(&want.calls).enumerate() {
                assert_eq!(call.name, item["name"].as_str().unwrap(), "{label}");
                assert_eq!(call.arguments, item["args"], "{label}");
                assert_eq!(
                    call.provider_id,
                    item["id"].as_str().map(|id| ProviderCallId(id.into())),
                    "{label}"
                );
                let raw: String = deltas
                    .iter()
                    .filter_map(|d| match d {
                        Delta::ToolCallArguments(a) if a.index as usize == index => {
                            assert_eq!(a.name.as_deref(), Some(call.name.as_str()));
                            Some(a.text.as_str())
                        }
                        Delta::Text(_) | Delta::Reasoning(_) | Delta::ToolCallArguments(_) => None,
                    })
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&raw).unwrap(),
                    call.arguments,
                    "{label}"
                );
                calls_seen += 1;
            }
            // No recording holds a `thought` part. A signature on text is
            // the text part's item; a signature on a call is a reasoning
            // carrier.
            let signed = reply
                .actions
                .iter()
                .filter(|action| {
                    let item = match action {
                        ReplyAction::Text(part) => part.provider_item.as_ref(),
                        ReplyAction::Reasoning(reasoning) => reasoning.provider_item.as_ref(),
                        ReplyAction::ToolCall(_) => None,
                    };
                    item.is_some_and(|item| item.get("thoughtSignature").is_some())
                })
                .count();
            assert_eq!(signed, want.signatures, "{label}");

            let count = |key: &str| want.usage[key].as_u64().unwrap_or(0);
            assert_eq!(
                reply.tokens,
                Tokens {
                    input: count("promptTokenCount") - count("cachedContentTokenCount"),
                    cache_read: count("cachedContentTokenCount"),
                    cache_write: std::collections::BTreeMap::new(),
                    output: count("candidatesTokenCount") + count("thoughtsTokenCount"),
                },
                "{label}"
            );
            assert!(reply.tokens.input > 0, "{label}");
            assert!(!reply.generation_id.0.is_empty(), "{label}");
        }
    }
    // 59 non-streamed bodies, 3 SSE bodies and the one text file; 39 HTTP
    // error statuses; 32 function calls, every one with an `id`.
    assert_eq!((streams, statuses, calls_seen), (63, 39, 32));
}

#[test]
fn the_recorded_stream_decodes_its_text_and_trailing_signature() {
    let (reply, deltas) = decoded(&recording("sse2-stream-ok.json"));
    let reply = reply.unwrap();
    assert_eq!(reply.text(), "Hello! How can I help you today?");
    assert_eq!(deltas.len(), 2);
    let [ReplyAction::Text(part)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    let item = part.provider_item.as_ref().unwrap();
    assert_eq!(part.text, "Hello! How can I help you today?");
    assert_eq!(item["text"], "Hello! How can I help you today?");
    assert!(
        item["thoughtSignature"]
            .as_str()
            .unwrap()
            .starts_with("EnMK")
    );
    assert_eq!(reply.generation_id.0, "03y7apiQIoOg1MkP4eXTkAg");
    assert_eq!(
        reply.tokens,
        Tokens {
            input: 2,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 9,
        }
    );
}

#[test]
fn the_long_recording_that_hit_the_limit_ends_at_the_output_limit() {
    let (reply, _) = decoded(&recording("sse2-long-gen.json"));
    assert_eq!(reply.unwrap().finish, Finish::OutputLimit);
}

#[test]
fn a_recording_served_by_the_fake_server_runs_through_the_seam() {
    let bytes = recording("sse2-stream-ok.json");
    let server = ProviderServer::start([Response::stream(bytes.clone())]).unwrap();
    let provider: Box<dyn Provider> = Box::new(Gemini::new(endpoint(&server)));
    let (reply, deltas) = run(provider.call(&request()));
    let (want, want_deltas) = decoded(&bytes);
    assert_eq!(reply.unwrap(), want.unwrap());
    assert_eq!(deltas, want_deltas);

    let sent = &server.requests()[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(
        sent.path,
        "/v1beta/models/gemini-3.1-flash-lite:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        sent.header("x-goog-api-key"),
        Some(fingerprint("AIza-secret").as_str())
    );
    assert_eq!(sent.header("authorization"), None);
    assert!(!String::from_utf8_lossy(&sent.body).contains("AIza-secret"));
    assert!(sent.header("user-agent").unwrap().starts_with("fiber/"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
}

#[test]
fn requests_from_the_same_inputs_are_byte_identical() {
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    let mut reordered = request();
    reordered.tools.insert(
        0,
        ToolDefinition {
            name: "zz_tool".into(),
            description: "Last alphabetically.".into(),
            input_schema: json!({"type": "object", "$defs": {}, "properties": {}}),
            deferred: false,
        },
    );
    for request in [request(), request(), reordered] {
        run(Box::new(gemini.request(&request))).0.unwrap();
    }
    let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(bodies[0], bodies[1]);
    assert_ne!(bodies[0], bodies[2]);
    assert_eq!(
        sent_body(&server, 0),
        json!({
            "systemInstruction": {"parts": [{"text": "You are terse."}]},
            "contents": [{"role": "user",
                "parts": [{"text": "What is the weather in Paris? Use the tool."}]}],
            "tools": [{"functionDeclarations": [{"name": "get_weather",
                "description": "Weather for a city.",
                "parametersJsonSchema": weather_tool().input_schema}]}],
            "toolConfig": {"functionCallingConfig": {"mode": "VALIDATED"}},
            "generationConfig": {"thinkingConfig": {"includeThoughts": true}},
        })
    );
    // Tools sorted by name, each schema as written; one schema outside the
    // strict subset makes the request `AUTO`.
    let reordered = sent_body(&server, 2);
    let declarations = &reordered["tools"][0]["functionDeclarations"];
    assert_eq!(declarations[0]["name"], "get_weather");
    assert_eq!(
        declarations[1]["parametersJsonSchema"],
        json!({"type": "object", "$defs": {}, "properties": {}})
    );
    assert_eq!(
        reordered["toolConfig"],
        json!({"functionCallingConfig": {"mode": "AUTO"}})
    );
}

#[test]
fn tool_choice_and_effort_map_to_geminis_own_values() {
    let script: Vec<Response> = (0..4).map(|_| completed_reply()).collect();
    let server = ProviderServer::start(script).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    for choice in ["none", "any", "get_weather"] {
        let mut request = request();
        request.tool_choice = choice.into();
        run(Box::new(gemini.request(&request))).0.unwrap();
    }
    let mut request = request();
    request.effort = Some("low".into());
    request.tools.clear();
    run(Box::new(gemini.request(&request))).0.unwrap();
    let config: Vec<Value> = (0..3)
        .map(|n| sent_body(&server, n)["toolConfig"]["functionCallingConfig"].clone())
        .collect();
    assert_eq!(
        config,
        [
            json!({"mode": "NONE"}),
            json!({"mode": "ANY"}),
            json!({"mode": "ANY", "allowedFunctionNames": ["get_weather"]}),
        ]
    );
    let body = sent_body(&server, 3);
    assert_eq!(body.get("tools"), None);
    assert_eq!(body.get("toolConfig"), None);
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "LOW"})
    );
}

#[test]
fn an_extra_body_field_replaces_fibers_own_except_generation_config() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let declared = Endpoint {
        extra_body: json!({"toolConfig": {"retrievalConfig": {}}})
            .as_object()
            .unwrap()
            .clone(),
        ..endpoint(&server)
    };
    run(Box::new(Gemini::new(declared).request(&request())))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["toolConfig"],
        json!({"retrievalConfig": {}})
    );
}

#[test]
fn max_output_tokens_is_the_models_limit_and_never_exceeds_it() {
    let script: Vec<Response> = (0..3).map(|_| completed_reply()).collect();
    let server = ProviderServer::start(script).unwrap();
    let limited = |extra: Value| Endpoint {
        max_output_tokens: Some(65_536),
        extra_body: extra.as_object().unwrap().clone(),
        ..endpoint(&server)
    };
    for extra in [
        json!({}),
        json!({"generationConfig": {"maxOutputTokens": 1024, "temperature": 1}}),
        json!({"generationConfig": {"maxOutputTokens": 10_000_000}}),
    ] {
        run(Box::new(Gemini::new(limited(extra)).request(&request())))
            .0
            .unwrap();
    }
    let sent: Vec<Value> = (0..3)
        .map(|n| sent_body(&server, n)["generationConfig"].clone())
        .collect();
    let thinking = json!({"includeThoughts": true});
    assert_eq!(
        sent,
        [
            json!({"maxOutputTokens": 65_536, "thinkingConfig": thinking}),
            json!({"maxOutputTokens": 1024, "temperature": 1, "thinkingConfig": thinking}),
            json!({"maxOutputTokens": 65_536, "thinkingConfig": thinking}),
        ]
    );
}

#[test]
fn the_requests_own_output_limit_is_capped_by_the_models() {
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let limited = Endpoint {
        max_output_tokens: Some(4096),
        ..endpoint(&server)
    };
    let low = ModelRequest {
        max_output_tokens: Some(1),
        ..request()
    };
    let high = ModelRequest {
        max_output_tokens: Some(9000),
        ..request()
    };
    run(Box::new(Gemini::new(limited.clone()).request(&low)))
        .0
        .unwrap();
    run(Box::new(Gemini::new(limited).request(&high)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["generationConfig"]["maxOutputTokens"],
        1
    );
    assert_eq!(
        sent_body(&server, 1)["generationConfig"]["maxOutputTokens"],
        4096
    );
}

#[test]
fn a_request_output_limit_without_generation_config_in_extra_body_sets_max_output_tokens() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let limited = Endpoint {
        max_output_tokens: Some(4096),
        extra_body: json!({"retrievalConfig": {}}).as_object().unwrap().clone(),
        ..endpoint(&server)
    };
    let request = ModelRequest {
        max_output_tokens: Some(1),
        ..request()
    };
    run(Box::new(Gemini::new(limited).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["generationConfig"]["maxOutputTokens"],
        1
    );
}

/// The conversation after `reply`, as the loop renders it: each action in
/// the order the model emitted it, then each call's result.
fn after(reply: &Reply, model: &str) -> Vec<Input> {
    let mut conversation = request().conversation;
    for (n, action) in reply.actions.iter().enumerate() {
        match action {
            ReplyAction::Text(part) => conversation.push(Input::Assistant {
                model: model.into(),
                text: part.text.clone(),
                provider_item: part.provider_item.clone(),
            }),
            ReplyAction::Reasoning(r) => conversation.push(Input::Reasoning {
                model: model.into(),
                text: r.text.clone(),
                provider_item: r.provider_item.clone(),
            }),
            ReplyAction::ToolCall(call) => conversation.push(Input::ToolCall {
                action_id: ActionId(format!("a_{n}")),
                call: call.clone(),
            }),
        }
    }
    for (n, action) in reply.actions.iter().enumerate() {
        if let ReplyAction::ToolCall(_) = action {
            conversation.push(Input::ToolResult {
                action_id: ActionId(format!("a_{n}")),
                text: "18 C, clear".into(),
                is_error: false,
            });
        }
    }
    conversation
}

fn sent_contents(conversation: Vec<Input>) -> (Value, String) {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let request = ModelRequest {
        conversation,
        ..request()
    };
    run(Box::new(Gemini::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let raw = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    (sent_body(&server, 0)["contents"].clone(), raw)
}

#[test]
fn a_thought_signature_goes_back_unchanged_on_its_call_to_the_model_that_made_it() {
    let bytes = recording("id2-emitted-gemini-3.1-flash-lite.json");
    let reply = decoded(&bytes).0.unwrap();
    let part = &expected_parts(&bytes)[0];
    assert!(part.get("thoughtSignature").is_some());

    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    // The model's part exactly as it arrived, then the call's response
    // naming the same id.
    assert_eq!(contents[1], json!({"role": "model", "parts": [part]}));
    assert_eq!(
        contents[2],
        json!({"role": "user", "parts": [{"functionResponse": {
            "name": "plot", "id": "call_603161", "response": {"output": "18 C, clear"}}}]})
    );

    // Another model's signature is left out.
    let (contents, raw) = sent_contents(after(&reply, "openai/gpt-6-luna"));
    assert_eq!(contents[1]["parts"][0].get("thoughtSignature"), None);
    assert!(!raw.contains(part["thoughtSignature"].as_str().unwrap()));
}

fn expected_parts(bytes: &[u8]) -> Vec<Value> {
    let data = String::from_utf8_lossy(bytes);
    let data = data.trim().strip_prefix("data: ").unwrap();
    let chunk: Value = serde_json::from_str(data).unwrap();
    chunk["candidates"][0]["content"]["parts"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn a_text_signature_rides_on_the_reply_text_or_alone_when_there_is_none() {
    let reply = decoded(&recording("sse2-stream-ok.json")).0.unwrap();
    let ReplyAction::Text(part) = &reply.actions[0] else {
        panic!("{:?}", reply.actions);
    };
    let item = part.provider_item.as_ref().unwrap();
    let signature = &item["thoughtSignature"];
    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Hello! How can I help you today?", "thoughtSignature": signature}]})
    );

    // Silence arrives with no text on the part, and rides alone on an
    // empty text part.
    let (silent, _) = decoded(&stream(&[chunk(
        json!([{"text": "", "thoughtSignature": signature}]),
        Some("STOP"),
    )]));
    let silent = silent.unwrap();
    assert_eq!(silent.text(), "");
    let (contents, _) = sent_contents(after(&silent, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [{"text": "", "thoughtSignature": signature}]})
    );
}

#[test]
fn a_function_call_without_an_id_is_logged_without_one_and_sent_back_without_one() {
    let (reply, _) = decoded(&stream(&[
        chunk(
            json!([{"functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
                "thoughtSignature": "c2ln"}]),
            Some("STOP"),
        ),
        // A function call with an empty id carries none either.
        chunk(
            json!([{"functionCall": {"name": "get_weather", "args": {"city": "Rome"}, "id": ""}}]),
            None,
        ),
    ]));
    let reply = reply.unwrap();
    let calls: Vec<&ToolCallRequested> = reply
        .actions
        .iter()
        .filter_map(|a| match a {
            ReplyAction::ToolCall(c) => Some(c),
            ReplyAction::Reasoning(_) | ReplyAction::Text(_) => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.provider_id.is_none()));

    let (contents, raw) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
             "thoughtSignature": "c2ln"},
            {"functionCall": {"name": "get_weather", "args": {"city": "Rome"}}}]})
    );
    assert_eq!(
        contents[2]["parts"][0],
        json!({"functionResponse": {"name": "get_weather",
            "response": {"output": "18 C, clear"}}})
    );
    // The local action ids never reach the wire.
    assert!(!raw.contains("a_0"));
    assert!(!raw.contains("\"id\""));
}

#[test]
fn thought_parts_stream_as_reasoning_and_go_back_as_one_part() {
    let (reply, deltas) = decoded(&stream(&[
        chunk(json!([{"text": "Let", "thought": true}]), None),
        chunk(
            json!([{"text": " me think.", "thought": true, "thoughtSignature": "dGhv"}]),
            None,
        ),
        json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "Done."}]},
            "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 100, "cachedContentTokenCount": 60,
                "candidatesTokenCount": 2, "thoughtsTokenCount": 40}}),
    ]));
    let reply = reply.unwrap();
    assert_eq!(
        deltas,
        [
            Delta::Reasoning(TextDelta { text: "Let".into() }),
            Delta::Reasoning(TextDelta {
                text: " me think.".into()
            }),
            Delta::Text(TextDelta {
                text: "Done.".into()
            }),
        ]
    );
    let thought = json!({"text": "Let me think.", "thought": true, "thoughtSignature": "dGhv"});
    assert_eq!(
        reply.actions,
        [
            ReplyAction::Reasoning(ReasoningCompleted {
                text: "Let me think.".into(),
                provider_item: Some(thought.clone()),
            }),
            ReplyAction::Text(TextCompleted {
                text: "Done.".into(),
                provider_item: None,
            }),
        ]
    );
    assert_eq!(
        reply.tokens,
        Tokens {
            input: 40,
            cache_read: 60,
            cache_write: std::collections::BTreeMap::new(),
            output: 42,
        }
    );
    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [thought, {"text": "Done."}]})
    );
}

#[test]
fn each_finish_reason_maps_as_the_docs_say_and_an_unknown_one_fails() {
    let end = |reason: &str| decoded(&stream(&[chunk(json!([]), Some(reason))])).0;
    assert_eq!(end("STOP").unwrap().finish, Finish::Completed);
    assert_eq!(end("MAX_TOKENS").unwrap().finish, Finish::OutputLimit);
    for reason in [
        "SAFETY",
        "RECITATION",
        "LANGUAGE",
        "BLOCKLIST",
        "PROHIBITED_CONTENT",
        "SPII",
        "IMAGE_SAFETY",
        "IMAGE_PROHIBITED_CONTENT",
        "IMAGE_RECITATION",
        "ESCALATION",
        "PUP_LIMITED_DISABLED",
    ] {
        let (code, message) = error_code(end(reason));
        assert_eq!(code, ErrorCode::Refused, "{reason}");
        assert!(message.contains(reason), "{message}");
    }
    for reason in [
        "MALFORMED_FUNCTION_CALL",
        "UNEXPECTED_TOOL_CALL",
        "TOO_MANY_TOOL_CALLS",
        "MISSING_THOUGHT_SIGNATURE",
        "MALFORMED_RESPONSE",
        "OTHER",
        "IMAGE_OTHER",
        "NO_IMAGE",
    ] {
        assert_eq!(
            error_code(end(reason)).0,
            ErrorCode::StreamIncomplete,
            "{reason}"
        );
    }
    for reason in ["FINISH_REASON_UNSPECIFIED", "SOMETHING_NEW"] {
        let (code, message) = error_code(end(reason));
        assert_eq!(code, ErrorCode::UnknownStopReason);
        assert!(message.contains(reason), "{message}");
    }
    // The finish message, when the candidate carries one, is the words.
    let mut refused = chunk(json!([]), Some("SAFETY"));
    refused["candidates"][0]["finishMessage"] = json!("Blocked for safety.");
    let (_, message) = error_code(decoded(&stream(&[refused])).0);
    assert!(message.contains("Blocked for safety."), "{message}");
}

#[test]
fn a_blocked_prompt_is_refused() {
    let (code, message) = error_code(
        decoded(&stream(&[
            json!({"promptFeedback": {"blockReason": "PROHIBITED_CONTENT"},
            "usageMetadata": {"promptTokenCount": 5}}),
        ]))
        .0,
    );
    assert_eq!(code, ErrorCode::Refused);
    assert!(message.contains("PROHIBITED_CONTENT"), "{message}");
}

#[test]
fn a_stream_cut_short_or_with_an_error_fails_and_drops_what_it_streamed() {
    let (code, message) = error_code(decoded(&stream(&[chunk(json!([{"text": "a"}]), None)])).0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
    assert!(message.contains("finishReason"), "{message}");
    assert_eq!(
        error_code(decoded(b"data: {not json}\n\n").0).0,
        ErrorCode::StreamIncomplete
    );

    let error = |status: &str| {
        stream(&[
            chunk(json!([{"text": "Checking"}]), None),
            json!({"error": {"code": 503, "message": "Overloaded.", "status": status}}),
        ])
    };
    let server = ProviderServer::start([Response::stream(error("UNAVAILABLE"))]).unwrap();
    let (result, deltas) = run(Box::new(Gemini::new(endpoint(&server)).request(&request())));
    let Err(CallError::Failed { failure, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
    assert_eq!(failure.provider.unwrap().message, "Overloaded.");
    assert_eq!(deltas.len(), 1);
    assert_eq!(
        error_code(decoded(&error("RESOURCE_EXHAUSTED")).0).0,
        ErrorCode::RateLimited
    );
    assert_eq!(
        error_code(decoded(&error("INTERNAL")).0).0,
        ErrorCode::ProviderUnavailable
    );
    assert_eq!(
        error_code(decoded(&error("INVALID_ARGUMENT")).0).0,
        ErrorCode::StreamIncomplete
    );
}

#[test]
fn a_failed_status_reads_retry_after_or_retry_info_and_the_providers_words() {
    let exhausted = |delay: &str| {
        json!({"error": {"code": 429, "message": "Quota exceeded.", "status": "RESOURCE_EXHAUSTED",
            "details": [
                {"@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": []},
                {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": delay}]}})
        .to_string()
    };
    let bad_key = probes::read(&research("raw/auth-badheader.json"))
        .unwrap()
        .remove(0);
    let Recorded::Status(400, bad_key) = bad_key.response else {
        panic!("expected a 400");
    };
    let server = ProviderServer::start([
        Response::status(429, exhausted("37s")),
        Response::status(429, exhausted("1.5s")),
        Response::status(429, exhausted("37s")).header("retry-after", "7"),
        Response::status(503, "{}"),
        Response::status(400, bad_key),
        Response::status(
            400,
            json!({"error": {"code": 400, "message": "Bad.", "status": "INVALID_ARGUMENT",
                "details": [{"@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "SOMETHING_ELSE"}]}})
            .to_string(),
        ),
    ])
    .unwrap();
    let gemini = Gemini::new(endpoint(&server));
    let failures: Vec<_> = (0..6)
        .map(|_| match run(Box::new(gemini.request(&request()))).0 {
            Err(CallError::Failed { failure, .. }) => failure,
            other => panic!("{other:?}"),
        })
        .collect();
    let retry: Vec<Option<f64>> = failures.iter().map(|f| f.retry_after).collect();
    assert_eq!(retry, [Some(37.0), Some(1.5), Some(7.0), None, None, None]);
    let codes: Vec<ErrorCode> = failures.iter().map(|f| f.code.clone()).collect();
    assert_eq!(
        codes,
        [
            ErrorCode::RateLimited,
            ErrorCode::RateLimited,
            ErrorCode::RateLimited,
            ErrorCode::ProviderUnavailable,
            ErrorCode::AuthenticationFailed,
            ErrorCode::InvalidRequest,
        ]
    );
    assert_eq!(
        failures[0].provider.as_ref().unwrap().message,
        "Quota exceeded."
    );
    assert!(failures[4].message.contains("fiber login gemini"));
}

#[test]
fn a_call_cancelled_before_it_runs_returns_without_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = Endpoint {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        ..Endpoint::default()
    };
    let call = Gemini::new(endpoint).request(&request());
    call.cancel();
    assert_eq!(run(Box::new(call)).0, Err(CallError::Cancelled));
    let accepted = listener.accept().map(|_| ()).unwrap_err();
    assert_eq!(accepted.kind(), std::io::ErrorKind::WouldBlock);
}

/// A server that answers one request with one text chunk, then holds the
/// socket open until `hold` is dropped.
fn stalling_server() -> (String, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (hold, held) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        let payload = String::from_utf8(stream(&[chunk(json!([{"text": "Hel"}]), None)])).unwrap();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
             transfer-encoding: chunked\r\n\r\n{:x}\r\n{payload}\r\n",
            payload.len()
        )
        .unwrap();
        socket.flush().unwrap();
        held.recv().unwrap_err();
    });
    (url, hold)
}

#[test]
fn cancelling_from_another_thread_ends_a_blocked_read() {
    let (url, _hold) = stalling_server();
    let endpoint = Endpoint {
        base_url: url,
        direct: true,
        ..Endpoint::default()
    };
    let call: Arc<dyn ModelCall> = Arc::from(Gemini::new(endpoint).call(&request()));
    let (first, first_seen) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let runner = Arc::clone(&call);
    thread::spawn(move || {
        let result = runner.run(&mut |delta| first.send(delta).unwrap());
        done.send(result).unwrap();
    });
    let delta = first_seen
        .recv_timeout(DEADLINE)
        .expect("waited for the first delta");
    assert_eq!(delta, Delta::Text(TextDelta { text: "Hel".into() }));
    call.cancel();
    let result = finished
        .recv_timeout(DEADLINE)
        .expect("waited for run to return after the cancel");
    assert_eq!(result, Err(CallError::Cancelled));
}

#[test]
fn the_cache_key_goes_in_the_declared_header_and_nowhere_else_without_one() {
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let endpoint = endpoint(&server);
    run(Box::new(Gemini::new(endpoint.clone()).request(&request())))
        .0
        .unwrap();
    run(Box::new(
        Gemini::new(endpoint)
            .cache_key_header("x-opencode-session")
            .request(&request()),
    ))
    .0
    .unwrap();
    let sent = server.requests();
    assert_eq!(sent[0].header("x-opencode-session"), None);
    assert_eq!(sent[1].header("x-opencode-session"), Some("session_1"));
}

#[test]
fn a_text_signature_stays_on_its_text_when_a_call_follows() {
    let (reply, _) = decoded(&stream(&[
        chunk(
            json!([{"text": "Checking.", "thoughtSignature": "c2ln"}]),
            None,
        ),
        chunk(
            json!([{"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}]),
            Some("STOP"),
        ),
    ]));
    let reply = reply.unwrap();
    // The signature rode on the text part, so it is stored with it.
    assert_eq!(
        reply.actions[0],
        ReplyAction::Text(TextCompleted {
            text: "Checking.".into(),
            provider_item: Some(json!({"text": "Checking.", "thoughtSignature": "c2ln"})),
        })
    );
    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    // Each logged part replays in order: the text with its signature,
    // then the call without one.
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Checking.", "thoughtSignature": "c2ln"},
            {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}]})
    );
}

#[test]
fn a_text_signature_stays_on_its_text_when_a_signed_call_follows() {
    let (reply, _) = decoded(&stream(&[
        chunk(
            json!([{"text": "Checking.", "thoughtSignature": "c2ln"}]),
            None,
        ),
        chunk(
            json!([{"functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
                "thoughtSignature": "c2FsbA"}]),
            Some("STOP"),
        ),
    ]));
    let reply = reply.unwrap();
    let (contents, raw) = sent_contents(after(&reply, REFERENCE));
    // Each signature stays on its own part: the text's on the text,
    // the call's on the call. Neither moves onto an empty text part.
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Checking.", "thoughtSignature": "c2ln"},
            {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
             "thoughtSignature": "c2FsbA"}]})
    );
    assert!(!raw.contains("{\"text\":\"\""));
}

#[test]
fn a_signed_text_marked_not_thought_replays_exactly_once() {
    let part = json!({"text": "Hi.", "thought": false, "thoughtSignature": "c2ln"});
    let (reply, _) = decoded(&stream(&[chunk(json!([part.clone()]), Some("STOP"))]));
    let reply = reply.unwrap();
    assert_eq!(reply.text(), "Hi.");
    let (contents, raw) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(contents[1], json!({"role": "model", "parts": [part]}));
    assert_eq!(raw.matches("Hi.").count(), 1);
}

#[test]
fn a_thoughts_words_are_not_mistaken_for_replayed_text() {
    // The thought's words reappear in the answer: only the stored text
    // part is consumed from the reply's text, never the thought.
    let (reply, _) = decoded(&stream(&[
        chunk(json!([{"text": "Ready", "thought": true}]), None),
        chunk(json!([{"text": "Ready, go."}]), Some("STOP")),
    ]));
    let reply = reply.unwrap();
    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Ready", "thought": true},
            {"text": "Ready, go."}]})
    );
}

#[test]
fn a_repeated_text_replays_each_part_with_its_own_signature() {
    // The same words twice, signed once: the signed part replays with
    // its signature, and the unsigned words still go out.
    let (reply, _) = decoded(&stream(&[
        chunk(json!([{"text": "Yo"}]), None),
        chunk(
            json!([{"text": "Yo", "thoughtSignature": "c2ln"}]),
            Some("STOP"),
        ),
    ]));
    let reply = reply.unwrap();
    assert_eq!(reply.text(), "YoYo");
    let (contents, raw) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Yo"},
            {"text": "Yo", "thoughtSignature": "c2ln"}]})
    );
    assert_eq!(raw.matches("\"text\":\"Yo\"").count(), 2);
}

#[test]
fn a_signed_reply_does_not_eat_the_same_words_in_a_later_reply() {
    // Two replies, each logged on its own. The first signs "Hi."; the
    // second says "Hi." with no signature. Both go out.
    let signed = json!({"text": "Hi.", "thoughtSignature": "c2ln"});
    let (earlier, _) = decoded(&stream(&[chunk(json!([signed.clone()]), Some("STOP"))]));
    let (later, _) = decoded(&stream(&[chunk(json!([{"text": "Hi."}]), Some("STOP"))]));
    let mut conversation = after(&earlier.unwrap(), REFERENCE);
    conversation.push(Input::User {
        text: "Say it again.".into(),
    });
    let later = later.unwrap();
    conversation.push(Input::Assistant {
        model: REFERENCE.into(),
        text: later.text(),
        provider_item: None,
    });
    let (contents, _) = sent_contents(conversation);
    assert_eq!(contents[1], json!({"role": "model", "parts": [signed]}));
    assert_eq!(
        contents[3],
        json!({"role": "model", "parts": [{"text": "Hi."}]})
    );
}

#[test]
fn a_bare_signature_is_not_dropped_when_another_follows_it() {
    let (reply, _) = decoded(&stream(&[chunk(
        json!([
            {"text": "", "thoughtSignature": "b25l"},
            {"text": "", "thoughtSignature": "dHdv"},
        ]),
        Some("STOP"),
    )]));
    let (contents, _) = sent_contents(after(&reply.unwrap(), REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "", "thoughtSignature": "b25l"},
            {"text": "", "thoughtSignature": "dHdv"}]})
    );
}

#[test]
fn unsigned_text_stays_ahead_of_the_signed_text_after_it() {
    let (reply, _) = decoded(&stream(&[chunk(
        json!([
            {"text": "Hello "},
            {"text": "world", "thoughtSignature": "c2ln"},
        ]),
        Some("STOP"),
    )]));
    let reply = reply.unwrap();
    assert_eq!(reply.text(), "Hello world");
    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Hello "},
            {"text": "world", "thoughtSignature": "c2ln"}]})
    );
}

#[test]
fn consecutive_replies_each_keep_their_own_text() {
    // A reply cut off by the output limit, unsigned, and the turn's next
    // reply, signed, with nothing between them (`docs/loop.md`, "A reply
    // cut off by the output limit"). Both go out, in order.
    let (cut, _) = decoded(&stream(&[chunk(
        json!([{"text": "Part one"}]),
        Some("MAX_TOKENS"),
    )]));
    let signed = json!({"text": "Part two.", "thoughtSignature": "c2ln"});
    let (next, _) = decoded(&stream(&[chunk(json!([signed.clone()]), Some("STOP"))]));
    let mut conversation = request().conversation;
    for reply in [cut.unwrap(), next.unwrap()] {
        for action in &reply.actions {
            match action {
                ReplyAction::Text(part) => conversation.push(Input::Assistant {
                    model: REFERENCE.into(),
                    text: part.text.clone(),
                    provider_item: part.provider_item.clone(),
                }),
                ReplyAction::Reasoning(r) => conversation.push(Input::Reasoning {
                    model: REFERENCE.into(),
                    text: r.text.clone(),
                    provider_item: r.provider_item.clone(),
                }),
                ReplyAction::ToolCall(_) => {}
            }
        }
    }
    let (contents, _) = sent_contents(conversation);
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [{"text": "Part one"}, signed]})
    );
}

#[test]
fn overflowing_usage_counts_fail_instead_of_panicking() {
    let mut overflow = chunk(json!([{"text": "hi"}]), Some("STOP"));
    overflow["usageMetadata"] = json!({"promptTokenCount": 10,
        "candidatesTokenCount": u64::MAX, "thoughtsTokenCount": 1});
    let (code, message) = error_code(decoded(&stream(&[overflow])).0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
    assert!(message.contains("usageMetadata"), "{message}");
}

#[test]
fn text_a_signed_call_and_signed_text_replay_in_model_order() {
    let (reply, _) = decoded(&stream(&[
        chunk(json!([{"text": "Yo"}]), None),
        chunk(
            json!([{"functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
                "thoughtSignature": "c2FsbA"}]),
            None,
        ),
        chunk(
            json!([{"text": "Yo", "thoughtSignature": "c2ln"}]),
            Some("STOP"),
        ),
    ]));
    let reply = reply.unwrap();
    assert!(matches!(
        reply.actions.as_slice(),
        [
            ReplyAction::Text(TextCompleted {
                provider_item: None,
                ..
            }),
            ReplyAction::Reasoning(_),
            ReplyAction::ToolCall(_),
            ReplyAction::Text(TextCompleted {
                provider_item: Some(_),
                ..
            }),
        ]
    ));
    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Yo"},
            {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
             "thoughtSignature": "c2FsbA"},
            {"text": "Yo", "thoughtSignature": "c2ln"}]})
    );
}

#[test]
fn a_signed_text_from_another_model_loses_its_signature_and_an_empty_one_adds_nothing() {
    let (reply, _) = decoded(&stream(&[chunk(
        json!([{"text": "Hi.", "thoughtSignature": "c2ln"}]),
        Some("STOP"),
    )]));
    let (contents, raw) = sent_contents(after(&reply.unwrap(), "openai/gpt-6-luna"));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [{"text": "Hi."}]})
    );
    assert!(!raw.contains("thoughtSignature"));

    let (silent, _) = decoded(&stream(&[chunk(
        json!([{"text": "", "thoughtSignature": "c2ln"}]),
        Some("STOP"),
    )]));
    let (contents, _) = sent_contents(after(&silent.unwrap(), "openai/gpt-6-luna"));
    assert_eq!(
        contents,
        json!([{"role": "user", "parts": [{"text": "What is the weather in Paris? Use the tool."}]}])
    );
}

#[test]
fn a_bare_call_signature_parks_before_user_content_and_not_on_a_later_call() {
    // A call signature with no `functionCall` before the next user message
    // goes out on an empty text part, ahead of that message. A later call
    // does not inherit it.
    let mut conversation = request().conversation;
    conversation.extend([
        Input::Reasoning {
            model: REFERENCE.into(),
            text: String::new(),
            provider_item: Some(json!({"thoughtSignature": "c2ln"})),
        },
        Input::User {
            text: "And tomorrow?".into(),
        },
        Input::ToolCall {
            action_id: ActionId("a_later".into()),
            call: ToolCallRequested {
                name: "get_weather".into(),
                arguments: json!({"city": "Paris"}),
                provider_id: None,
                repair: None,
                ran_by: None,
            },
        },
    ]);
    let (contents, _) = sent_contents(conversation);
    assert_eq!(
        contents,
        json!([
            {"role": "user", "parts": [
                {"text": "What is the weather in Paris? Use the tool."}]},
            {"role": "model", "parts": [{"text": "", "thoughtSignature": "c2ln"}]},
            {"role": "user", "parts": [{"text": "And tomorrow?"}]},
            {"role": "model", "parts": [
                {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}]},
        ])
    );
}

#[test]
fn wire_tools_is_what_the_request_sends() {
    let tools = wire_tools::wire_tools_fixture();
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    let wired: Vec<Value> = gemini
        .wire_tools(&tools)
        .into_iter()
        .map(Value::Object)
        .collect();
    let mut request = request();
    request.tools = tools;
    run(Box::new(gemini.request(&request))).0.unwrap();
    let sent = sent_body(&server, 0)["tools"][0]["functionDeclarations"].clone();
    assert_eq!(Value::Array(wired), sent);
    let names: Vec<String> = sent
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(names.len(), 24);
}

#[test]
fn a_failed_tool_result_sends_an_error_key_and_a_success_sends_output() {
    // A failed call sends the documented `error` key in place of `output`
    // (googleapis `google/ai/generativelanguage/v1beta/content.proto`,
    // `FunctionResponse.response`: "if the function call failed to execute,
    // the response can have an \"error\" key"). Gemini defines no boolean flag.
    let conversation = |is_error: bool| {
        vec![
            Input::ToolCall {
                action_id: ActionId("a_1".into()),
                call: ToolCallRequested {
                    name: "plot".into(),
                    arguments: json!({"city": "Paris"}),
                    provider_id: Some(ProviderCallId("c1".into())),
                    repair: None,
                    ran_by: None,
                },
            },
            Input::ToolResult {
                action_id: ActionId("a_1".into()),
                text: "boom".into(),
                is_error,
            },
        ]
    };
    let part = |is_error: bool| {
        let server = ProviderServer::start([completed_reply()]).unwrap();
        let request = ModelRequest {
            conversation: conversation(is_error),
            ..request()
        };
        run(Box::new(Gemini::new(endpoint(&server)).request(&request)))
            .0
            .unwrap();
        sent_body(&server, 0)["contents"][1]["parts"][0].clone()
    };
    assert_eq!(
        part(true),
        json!({"functionResponse": {"name": "plot", "id": "c1", "response": {"error": "boom"}}})
    );
    assert_eq!(
        part(false),
        json!({"functionResponse": {"name": "plot", "id": "c1", "response": {"output": "boom"}}})
    );
}
