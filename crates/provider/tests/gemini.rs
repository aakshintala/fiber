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

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::{CacheLifetime, ReasoningCompleted, TextDelta, ToolCallRequested};
use contract::provider::{
    CallError, Delta, Finish, Input, ModelCall, ModelRequest, Provider, Reply, ReplyAction,
    ToolDefinition,
};
use contract::shapes::Tokens;
use contract::{ActionId, ErrorCode, ProviderCallId};
use fakes::{ProviderServer, Response};
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
            assert_eq!(text, reply.text, "{label}");
            assert_eq!(reply.text, want.text, "{label}");

            let calls: Vec<&ToolCallRequested> = reply
                .actions
                .iter()
                .filter_map(|a| match a {
                    ReplyAction::ToolCall(c) => Some(c),
                    ReplyAction::Reasoning(_) => None,
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
            // No recording holds a `thought` part, so each reasoning action
            // is a signature that rode on a text or `functionCall` part.
            assert_eq!(
                reply.actions.len() - calls.len(),
                want.signatures,
                "{label}"
            );

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
    assert_eq!(reply.text, "Hello! How can I help you today?");
    assert_eq!(deltas.len(), 2);
    let [
        ReplyAction::Reasoning(ReasoningCompleted {
            text,
            provider_item: Some(item),
        }),
    ] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(text, "");
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
    assert_eq!(sent.header("x-goog-api-key"), Some("<masked>"));
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

/// The conversation after `reply` called `get_weather`, as the loop renders
/// it: the reply's actions in order, then its text.
fn after(reply: &Reply, model: &str) -> Vec<Input> {
    let mut conversation = request().conversation;
    for (n, action) in reply.actions.iter().enumerate() {
        match action {
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
    conversation.push(Input::Assistant {
        text: reply.text.clone(),
    });
    for (n, action) in reply.actions.iter().enumerate() {
        if let ReplyAction::ToolCall(_) = action {
            conversation.push(Input::ToolResult {
                action_id: ActionId(format!("a_{n}")),
                text: "18 C, clear".into(),
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
    let ReplyAction::Reasoning(ReasoningCompleted {
        provider_item: Some(item),
        ..
    }) = &reply.actions[0]
    else {
        panic!("{:?}", reply.actions);
    };
    let signature = &item["thoughtSignature"];
    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Hello! How can I help you today?", "thoughtSignature": signature}]})
    );

    let mut silent = reply.clone();
    silent.text.clear();
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
            ReplyAction::Reasoning(_) => None,
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
        [ReplyAction::Reasoning(ReasoningCompleted {
            text: "Let me think.".into(),
            provider_item: Some(thought.clone()),
        })]
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
    ])
    .unwrap();
    let gemini = Gemini::new(endpoint(&server));
    let failures: Vec<_> = (0..5)
        .map(|_| match run(Box::new(gemini.request(&request()))).0 {
            Err(CallError::Failed { failure, .. }) => failure,
            other => panic!("{other:?}"),
        })
        .collect();
    let retry: Vec<Option<f64>> = failures.iter().map(|f| f.retry_after).collect();
    assert_eq!(retry, [Some(37.0), Some(1.5), Some(7.0), None, None]);
    let codes: Vec<ErrorCode> = failures.iter().map(|f| f.code.clone()).collect();
    assert_eq!(
        codes,
        [
            ErrorCode::RateLimited,
            ErrorCode::RateLimited,
            ErrorCode::RateLimited,
            ErrorCode::ProviderUnavailable,
            ErrorCode::AuthenticationFailed,
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
    let delta = first_seen.recv_timeout(DEADLINE).unwrap();
    assert_eq!(delta, Delta::Text(TextDelta { text: "Hel".into() }));
    call.cancel();
    let result = finished.recv_timeout(DEADLINE).unwrap();
    assert_eq!(result, Err(CallError::Cancelled));
}
