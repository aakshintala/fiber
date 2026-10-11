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

#[path = "support/large.rs"]
mod large;

#[path = "support/wire_tools.rs"]
mod wire_tools;

#[path = "support/harness.rs"]
mod harness;

use std::path::{Path, PathBuf};

use contract::events::{
    CallStatus, ReasoningCompleted, TextCompleted, TextDelta, ToolCallRequested,
};
use contract::provider::{
    CallError, Delta, Finish, HostedCall, Input, InputSize, ModelRequest, Provider, Reply,
    ReplyAction, ToolDefinition,
};
use contract::shapes::{ContentPart, Tokens};
use contract::{ActionId, ErrorCode, GenerationId, ProviderCallId};
use fakes::{ProviderServer, Response, fingerprint};
use provider::Endpoint;
use provider::google_generative_ai::{Gemini, decode};
use serde_json::{Value, json};

use probes::Recorded;

use harness::{gemini_completed as completed_reply, gemini_sse as stream, run, sent_body};
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
        model: "gemini-3.1-flash-lite".into(),
        base_url: format!("{}/v1beta", server.url()),
        key: Some(contract::Secret::new("AIza-secret".into())),
        ..harness::endpoint("gemini", server)
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
        hosted: None,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        tools: vec![weather_tool()],
        conversation: vec![Input::User {
            text: "What is the weather in Paris? Use the tool.".into(),
            images: Vec::new(),
        }],
        ..harness::request()
    }
}

fn chunk(parts: Value, finish: Option<&str>) -> Value {
    let mut candidate = json!({"content": {"role": "model", "parts": parts}, "index": 0});
    if let Some(reason) = finish {
        candidate["finishReason"] = json!(reason);
    }
    json!({"candidates": [candidate], "responseId": "r1",
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 3}})
}

#[test]
fn a_function_call_with_no_args_records_an_empty_object() {
    let parts = json!([{"functionCall": {"name": "f"}}]);
    let reply = decoded(&stream(&[chunk(parts, Some("STOP"))])).0.unwrap();
    let [ReplyAction::ToolCall(call)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(call.arguments, json!({}));
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
                    ReplyAction::Reasoning(_) | ReplyAction::Text(_) | ReplyAction::Hosted(_) => {
                        None
                    }
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
                        ReplyAction::ToolCall(_) | ReplyAction::Hosted(_) => None,
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
            assert!(reply.generation_id.is_some(), "{label}");
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
    assert_eq!(
        reply.generation_id,
        Some(GenerationId("03y7apiQIoOg1MkP4eXTkAg".into()))
    );
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
    let mut want = want.unwrap();
    want.input_size = InputSize {
        bytes: u64::try_from(server.requests()[0].body.len()).unwrap(),
        media: false,
    };
    assert_eq!(reply.unwrap(), want);
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
fn a_request_body_has_geminis_shape() {
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    let mut reordered = request();
    reordered.tools.insert(
        0,
        ToolDefinition {
            name: "zz_tool".into(),
            description: "Last alphabetically.".into(),
            input_schema: json!({"type": "object", "$defs": {}, "properties": {}}),
            deferred: false,
            hosted: None,
        },
    );
    for request in [request(), reordered] {
        run(Box::new(gemini.request(&request))).0.unwrap();
    }
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
    let reordered = sent_body(&server, 1);
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
fn tool_choice_and_thinking_map_to_geminis_own_values() {
    let script: Vec<Response> = (0..4).map(|_| completed_reply()).collect();
    let server = ProviderServer::start(script).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    for choice in ["none", "any", "get_weather"] {
        let mut request = request();
        request.tool_choice = choice.into();
        run(Box::new(gemini.request(&request))).0.unwrap();
    }
    let mut request = request();
    request.thinking = Some(contract::ThinkingLevel::Low);
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
fn an_extra_body_field_is_sent_as_given() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let safety = json!({"safetySettings": [{"category": "HARM_CATEGORY_HARASSMENT",
        "threshold": "BLOCK_NONE"}]});
    let declared = Endpoint {
        extra_body: safety.as_object().unwrap().clone(),
        ..endpoint(&server)
    };
    run(Box::new(Gemini::new(declared).request(&request())))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["safetySettings"],
        safety["safetySettings"]
    );
}

#[test]
fn an_extra_body_object_replaces_a_non_generation_config_object() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let extra = json!({"toolConfig": {"custom": "declared"}});
    let declared = Endpoint {
        extra_body: extra.as_object().unwrap().clone(),
        ..endpoint(&server)
    };
    run(Box::new(Gemini::new(declared).request(&request())))
        .0
        .unwrap();
    assert_eq!(sent_body(&server, 0)["toolConfig"], extra["toolConfig"]);
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
                model: model.into(),
            }),
            ReplyAction::Hosted(hosted) => {
                // A hosted pair renders as two adjacent own-model lines,
                // the call's block then the result's, as
                // `crates/loop/src/conversation.rs` renders them.
                for item in [
                    hosted.call.provider_item.clone(),
                    hosted.completed.provider_item.clone(),
                ] {
                    conversation.push(Input::Assistant {
                        model: model.into(),
                        text: String::new(),
                        provider_item: item,
                    });
                }
            }
        }
    }
    for (n, action) in reply.actions.iter().enumerate() {
        if let ReplyAction::ToolCall(_) = action {
            conversation.push(Input::ToolResult {
                pdfs: Vec::new(),
                action_id: ActionId(format!("a_{n}")),
                text: "18 C, clear".into(),
                is_error: false,
                images: Vec::new(),
            });
        }
    }
    conversation
}

#[track_caller]
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
            ReplyAction::Reasoning(_) | ReplyAction::Text(_) | ReplyAction::Hosted(_) => None,
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
fn each_signed_thought_part_is_its_own_item_with_its_own_signature() {
    let (reply, _) = decoded(&stream(&[
        chunk(
            json!([{"text": "One.", "thought": true, "thoughtSignature": "c2lnMQ=="}]),
            None,
        ),
        chunk(
            json!([{"text": "Two.", "thought": true, "thoughtSignature": "c2lnMg=="}]),
            None,
        ),
        chunk(json!([{"text": "Done."}]), Some("STOP")),
    ]));
    let reasoning: Vec<_> = reply
        .unwrap()
        .actions
        .into_iter()
        .filter_map(|action| match action {
            ReplyAction::Reasoning(item) => Some((item.text, item.provider_item)),
            ReplyAction::Text(_) | ReplyAction::ToolCall(_) | ReplyAction::Hosted(_) => None,
        })
        .collect();
    assert_eq!(
        reasoning,
        [
            (
                "One.".to_owned(),
                Some(json!({"text": "One.", "thought": true, "thoughtSignature": "c2lnMQ=="}))
            ),
            (
                "Two.".to_owned(),
                Some(json!({"text": "Two.", "thought": true, "thoughtSignature": "c2lnMg=="}))
            ),
        ]
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
    let retry: Vec<Option<u64>> = failures.iter().map(|f| f.retry_after_ms).collect();
    assert_eq!(
        retry,
        [Some(37000), Some(1500), Some(7000), None, None, None]
    );
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
        images: Vec::new(),
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
                ReplyAction::ToolCall(_) | ReplyAction::Hosted(_) => {}
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
            images: Vec::new(),
        },
        Input::ToolCall {
            action_id: ActionId("a_later".into()),
            call: ToolCallRequested {
                name: "get_weather".into(),
                arguments: json!({"city": "Paris"}),
                provider_id: None,
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: REFERENCE.into(),
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
                    provider_item: None,
                },
                model: REFERENCE.into(),
            },
            Input::ToolResult {
                pdfs: Vec::new(),
                action_id: ActionId("a_1".into()),
                text: "boom".into(),
                is_error,
                images: Vec::new(),
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

/// A conversation whose one tool result carries `images`.
fn image_conversation(
    is_error: bool,
    text: &str,
    images: Vec<contract::provider::ImageRef>,
) -> Vec<Input> {
    vec![
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.png"}),
                provider_id: Some(ProviderCallId("c1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: REFERENCE.into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_1".into()),
            text: text.into(),
            is_error,
            images,
        },
    ]
}

fn png_ref(path: &str) -> contract::provider::ImageRef {
    contract::provider::ImageRef {
        path: path.into(),
        mime_type: "image/png".into(),
        width: 2,
        height: 1,
    }
}

fn function_response(server: &ProviderServer) -> Value {
    sent_body(server, 0)["contents"][1]["parts"][0]["functionResponse"].clone()
}

#[test]
fn a_stored_image_is_sent_as_inline_data_parts_beside_the_response() {
    let session = fakes::TempDir::new("fiber-gemini-request-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(
            false,
            "Image: 2x1 image/png.\n",
            vec![png_ref("artifacts/i_1.png")],
        ),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    for _ in 0..2 {
        run(Box::new(gemini.request(&request))).0.unwrap();
    }
    let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(bodies[0], bodies[1], "a resume sends the same bytes");
    assert_eq!(
        function_response(&server),
        json!({"name": "read", "id": "c1",
            "response": {"output": "Image: 2x1 image/png.\n"},
            "parts": [{"inlineData": {"mimeType": "image/png", "data": "YWJjZA=="}}]})
    );
}

/// A conversation whose one tool result carries a PDF.
fn pdf_conversation(model: &str, pdf: contract::provider::PdfRef) -> Vec<Input> {
    vec![
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.pdf"}),
                provider_id: Some(ProviderCallId("c1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: model.into(),
        },
        Input::ToolResult {
            action_id: ActionId("a_1".into()),
            text: "PDF: 2 pages.\n".into(),
            is_error: false,
            images: Vec::new(),
            pdfs: vec![pdf],
        },
    ]
}

fn pdf_ref(path: &str) -> contract::provider::PdfRef {
    contract::provider::PdfRef {
        path: path.into(),
        page_count: 2,
        pages: None,
    }
}

#[test]
fn a_stored_pdf_is_sent_as_inline_data_parts_beside_the_response() {
    let session = fakes::TempDir::new("fiber-gemini-request-pdf");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/p_1.pdf"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: pdf_conversation(REFERENCE, pdf_ref("artifacts/p_1.pdf")),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Gemini::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        function_response(&server),
        json!({"name": "read", "id": "c1",
            "response": {"output": "PDF: 2 pages.\n"},
            "parts": [{"inlineData": {"mimeType": "application/pdf", "data": "YWJjZA=="}}]})
    );
}

#[test]
fn a_failed_result_with_an_image_keeps_the_error_key_and_still_has_parts() {
    let session = fakes::TempDir::new("fiber-gemini-request-image");
    std::fs::write(session.path().join("i.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(true, "boom", vec![png_ref("i.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Gemini::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        function_response(&server),
        json!({"name": "read", "id": "c1",
            "response": {"error": "boom"},
            "parts": [{"inlineData": {"mimeType": "image/png", "data": "YWJjZA=="}}]})
    );
}

#[test]
fn a_text_only_model_gets_no_parts_and_the_response_says_so() {
    let session = fakes::TempDir::new("fiber-gemini-request-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(
            false,
            "Image: 2x1 image/png.\n",
            vec![png_ref("artifacts/i_1.png")],
        ),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let endpoint = Endpoint {
        text_only: true,
        ..endpoint(&server)
    };
    run(Box::new(Gemini::new(endpoint).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        function_response(&server),
        json!({"name": "read", "id": "c1",
            "response": {"output": "Image: 2x1 image/png.\n[Image artifacts/i_1.png left out: this model does not take images.]"}})
    );
}

#[test]
fn a_foreign_call_and_result_go_as_text_while_the_models_own_stay_native() {
    let conversation = vec![
        Input::User {
            text: "What is in a.txt?".into(),
            images: Vec::new(),
        },
        Input::Reasoning {
            model: "other/model".into(),
            text: "foreign thoughts here".into(),
            provider_item: Some(
                json!({"text": "foreign thoughts here", "thought": true, "thoughtSignature": "Zm9yZWln"}),
            ),
        },
        Input::Assistant {
            model: "other/model".into(),
            text: "foreign words here".into(),
            provider_item: None,
        },
        Input::ToolCall {
            action_id: ActionId("a_f1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.txt"}),
                provider_id: Some(ProviderCallId("c_foreign".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "other/model".into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_f1".into()),
            text: "hello".into(),
            is_error: false,
            images: Vec::new(),
        },
        Input::Reasoning {
            model: REFERENCE.into(),
            text: String::new(),
            provider_item: Some(json!({"thoughtSignature": "c2ln"})),
        },
        Input::ToolCall {
            action_id: ActionId("a_o1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "b.txt"}),
                provider_id: Some(ProviderCallId("c1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: REFERENCE.into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_o1".into()),
            text: "world".into(),
            is_error: false,
            images: Vec::new(),
        },
    ];
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let request = ModelRequest {
        conversation,
        ..request()
    };
    run(Box::new(Gemini::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let body = sent_body(&server, 0);
    let raw = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert_eq!(
        body["contents"],
        json!([
            {"role": "user", "parts": [{"text": "What is in a.txt?"}]},
            {"role": "model", "parts": [{"text": "foreign words here"}]},
            {"role": "user", "parts": [
                {"text": "other/model called the tool read with arguments {\"path\":\"a.txt\"}"},
                {"text": "The tool read returned:\nhello"}]},
            {"role": "model", "parts": [{"functionCall":
                {"name": "read", "args": {"path": "b.txt"}, "id": "c1"},
                "thoughtSignature": "c2ln"}]},
            {"role": "user", "parts": [{"functionResponse":
                {"name": "read", "id": "c1", "response": {"output": "world"}}}]},
        ])
    );
    // No reasoning from another model is sent, as text or otherwise.
    assert!(!raw.contains("foreign thoughts here"));
    assert!(!raw.contains("Zm9yZWln"));
    // No provider id is sent for a foreign call or result.
    assert!(!raw.contains("c_foreign"));
}

#[test]
fn a_failed_foreign_result_renders_the_failed_text() {
    let conversation = vec![
        Input::ToolCall {
            action_id: ActionId("a_f1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.txt"}),
                provider_id: Some(ProviderCallId("c_foreign".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "other/model".into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_f1".into()),
            text: "boom".into(),
            is_error: true,
            images: Vec::new(),
        },
    ];
    let (contents, raw) = sent_contents(conversation);
    assert_eq!(
        contents,
        json!([{"role": "user", "parts": [
            {"text": "other/model called the tool read with arguments {\"path\":\"a.txt\"}"},
            {"text": "The tool read failed:\nboom"}]}])
    );
    assert!(!contents.to_string().contains("functionCall"));
    assert!(!contents.to_string().contains("functionResponse"));
    assert!(!raw.contains("c_foreign"));
}

#[test]
fn a_foreign_result_with_a_pdf_sends_text_then_inline_data() {
    let session = fakes::TempDir::new("fiber-gemini-foreign-pdf");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/p_1.pdf"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: pdf_conversation("other/model", pdf_ref("artifacts/p_1.pdf")),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Gemini::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["contents"],
        json!([{"role": "user", "parts": [
            {"text": "other/model called the tool read with arguments {\"path\":\"a.pdf\"}"},
            {"text": "The tool read returned:\nPDF: 2 pages.\n"},
            {"inlineData": {"mimeType": "application/pdf", "data": "YWJjZA=="}}]}])
    );
}

#[test]
fn a_foreign_result_with_an_image_sends_text_then_inline_data() {
    let session = fakes::TempDir::new("fiber-gemini-foreign-image");
    std::fs::write(session.path().join("i.png"), b"abcd").unwrap();
    let conversation = vec![
        Input::ToolCall {
            action_id: ActionId("a_f1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.png"}),
                provider_id: Some(ProviderCallId("c_foreign".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "other/model".into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_f1".into()),
            text: "Image: 2x1 image/png.\n".into(),
            is_error: false,
            images: vec![png_ref("i.png")],
        },
    ];
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let request = ModelRequest {
        conversation,
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    run(Box::new(Gemini::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["contents"],
        json!([{"role": "user", "parts": [
            {"text": "other/model called the tool read with arguments {\"path\":\"a.png\"}"},
            {"text": "The tool read returned:\nImage: 2x1 image/png.\n"},
            {"inlineData": {"mimeType": "image/png", "data": "YWJjZA=="}}]}])
    );
}

#[test]
fn a_result_without_its_call_renders_as_today() {
    let conversation = vec![Input::ToolResult {
        pdfs: Vec::new(),
        action_id: ActionId("a_missing".into()),
        text: "hello".into(),
        is_error: false,
        images: Vec::new(),
    }];
    let (contents, _) = sent_contents(conversation);
    assert_eq!(
        contents,
        json!([{"role": "user", "parts": [{"functionResponse":
            {"name": "", "response": {"output": "hello"}}}]}])
    );
}

#[test]
fn thinking_levels_map_to_geminis_thinking_config() {
    use contract::ThinkingLevel::{Low, Off, Xhigh};
    let server = ProviderServer::start([
        completed_reply(),
        completed_reply(),
        completed_reply(),
        completed_reply(),
    ])
    .unwrap();
    for level in [None, Some(Off), Some(Low), Some(Xhigh)] {
        let mut req = request();
        req.thinking = level;
        run(Box::new(Gemini::new(endpoint(&server)).request(&req)))
            .0
            .unwrap();
    }
    assert_eq!(
        sent_body(&server, 0)["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true})
    );
    assert_eq!(
        sent_body(&server, 1)["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingBudget": 0})
    );
    assert_eq!(
        sent_body(&server, 2)["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "LOW"})
    );
    assert_eq!(
        sent_body(&server, 3)["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "XHIGH"})
    );
}

/// A conversation whose one user message carries `images`.
fn user_conversation(text: &str, images: Vec<contract::provider::ImageRef>) -> Vec<Input> {
    vec![Input::User {
        text: text.into(),
        images,
    }]
}

fn user_contents(server: &ProviderServer) -> Value {
    sent_body(server, 0)["contents"].clone()
}

#[test]
fn a_users_image_is_sent_as_inline_data_parts_after_the_text() {
    let session = fakes::TempDir::new("fiber-gemini-user-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: user_conversation("look", vec![png_ref("artifacts/i_1.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    for _ in 0..2 {
        run(Box::new(gemini.request(&request))).0.unwrap();
    }
    let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(bodies[0], bodies[1], "a resume sends the same bytes");
    assert_eq!(
        user_contents(&server),
        json!([{"role": "user", "parts": [
            {"text": "look"},
            {"inlineData": {"mimeType": "image/png", "data": "YWJjZA=="}},
        ]}])
    );
}

#[test]
fn a_user_without_images_keeps_its_text_part_even_when_empty() {
    let (contents, _) = sent_contents(user_conversation("", Vec::new()));
    assert_eq!(contents, json!([{"role": "user", "parts": [{"text": ""}]}]));
}

#[test]
fn sent_tools_set_the_strictness() {
    // A rewound session's first request carries its parent's logged build,
    // not what its own tools would wire (`docs/events.md`, "Rewind"):
    // the declarations go over verbatim, and the strictness the sent
    // schemas imply decides the `toolConfig`.
    let strict_schema = weather_tool().input_schema;
    let loose_schema = json!({"type": "object", "properties": {"a": {"type": "string"}}});
    let declaration = |name: &str, schema: &Value| json!({"name": name, "description": "Sent.", "parametersJsonSchema": schema});
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    // Strict sent schemas with loose wired tools: the sent `VALIDATED`.
    let mut strict_sent = request();
    strict_sent.tools = vec![ToolDefinition {
        name: "loose".into(),
        description: "Loose.".into(),
        input_schema: loose_schema.clone(),
        deferred: false,
        hosted: None,
    }];
    strict_sent.sent_tools = Some(
        [
            declaration("b_tool", &strict_schema),
            declaration("a_tool", &strict_schema),
        ]
        .iter()
        .map(|tool| tool.as_object().unwrap().clone())
        .collect(),
    );
    run(Box::new(gemini.request(&strict_sent))).0.unwrap();
    // Loose sent schemas with strict wired tools: the sent `AUTO`.
    let mut loose_sent = request();
    loose_sent.sent_tools = Some(
        [
            declaration("b_tool", &loose_schema),
            declaration("a_tool", &loose_schema),
        ]
        .iter()
        .map(|tool| tool.as_object().unwrap().clone())
        .collect(),
    );
    run(Box::new(gemini.request(&loose_sent))).0.unwrap();
    // No sent tools with wired ones: neither `tools` nor `toolConfig`.
    let mut empty_sent = request();
    empty_sent.sent_tools = Some(Vec::new());
    run(Box::new(gemini.request(&empty_sent))).0.unwrap();
    let strict = sent_body(&server, 0);
    assert_eq!(
        strict["toolConfig"],
        json!({"functionCallingConfig": {"mode": "VALIDATED"}})
    );
    assert_eq!(
        sent_body(&server, 1)["toolConfig"],
        json!({"functionCallingConfig": {"mode": "AUTO"}})
    );
    let empty = sent_body(&server, 2);
    assert_eq!(empty.get("tools"), None);
    assert_eq!(empty.get("toolConfig"), None);
}

fn hosted_tool() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".into(),
        description: String::new(),
        input_schema: json!({}),
        deferred: false,
        hosted: Some("google_search".into()),
    }
}

fn loose_tool() -> ToolDefinition {
    ToolDefinition {
        name: "a_loose".into(),
        description: "Loose.".into(),
        input_schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
        deferred: false,
        hosted: None,
    }
}

#[test]
fn a_hosted_search_is_its_own_tool_and_turns_on_server_side_invocations() {
    // A hosted definition's wire entry is its kind alone, with no name:
    // the loop zips the wire list with the tool map in name order.
    let wired: Vec<Value> = Gemini::new(Endpoint::default())
        .wire_tools(&[weather_tool(), hosted_tool()])
        .into_iter()
        .map(Value::Object)
        .collect();
    assert_eq!(
        wired,
        vec![
            json!({"name": "get_weather", "description": "Weather for a city.",
                "parametersJsonSchema": weather_tool().input_schema}),
            json!({"google_search": {}}),
        ]
    );
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut sent = request();
    sent.tools = vec![weather_tool(), hosted_tool()];
    run(Box::new(Gemini::new(endpoint(&server)).request(&sent)))
        .0
        .unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(
        body["tools"],
        json!([{"functionDeclarations": [wired[0].clone()]}, {"google_search": {}}])
    );
    // The hosted `{}` schema does not turn off strict: the request stays
    // `VALIDATED`, with the flag the search needs beside function tools.
    assert_eq!(
        body["toolConfig"],
        json!({"functionCallingConfig": {"mode": "VALIDATED"},
            "includeServerSideToolInvocations": true})
    );
}

#[test]
fn a_hosted_search_alone_sends_no_function_declarations() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut sent = request();
    sent.tools = vec![hosted_tool()];
    run(Box::new(Gemini::new(endpoint(&server)).request(&sent)))
        .0
        .unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(body["tools"], json!([{"google_search": {}}]));
    assert_eq!(
        body["toolConfig"],
        json!({"functionCallingConfig": {"mode": "VALIDATED"},
            "includeServerSideToolInvocations": true})
    );
}

#[test]
fn a_non_strict_function_beside_a_hosted_search_sends_auto() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut sent = request();
    sent.tools = vec![loose_tool(), hosted_tool()];
    run(Box::new(Gemini::new(endpoint(&server)).request(&sent)))
        .0
        .unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(
        body["toolConfig"],
        json!({"functionCallingConfig": {"mode": "AUTO"},
            "includeServerSideToolInvocations": true})
    );
    assert_eq!(body["tools"].as_array().unwrap().len(), 2);
}

#[test]
fn a_rewound_build_with_a_hosted_entry_is_split_the_same_way() {
    // A rewound session's logged build splits like a fresh one: the
    // declaration goes in `functionDeclarations`, the hosted entry
    // after it, with the same `toolConfig`.
    let declaration = json!({"name": "get_weather", "description": "Weather for a city.",
        "parametersJsonSchema": weather_tool().input_schema});
    let sent = || {
        vec![declaration.clone(), json!({"google_search": {}})]
            .into_iter()
            .map(|tool| tool.as_object().unwrap().clone())
            .collect::<Vec<_>>()
    };
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    let gemini = Gemini::new(endpoint(&server));
    let mut fresh = request();
    fresh.tools = vec![weather_tool(), hosted_tool()];
    run(Box::new(gemini.request(&fresh))).0.unwrap();
    let mut rewound = request();
    rewound.sent_tools = Some(sent());
    run(Box::new(gemini.request(&rewound))).0.unwrap();
    assert_eq!(sent_body(&server, 1), sent_body(&server, 0));
    // A non-strict sent declaration beside the hosted entry sends `AUTO`
    // with the flag, the strict filter on the sent path.
    let mut loose = request();
    loose.sent_tools = Some(vec![
        json!({"name": "a_loose", "description": "Loose.",
            "parametersJsonSchema": loose_tool().input_schema})
        .as_object()
        .unwrap()
        .clone(),
        json!({"google_search": {}}).as_object().unwrap().clone(),
    ]);
    run(Box::new(gemini.request(&loose))).0.unwrap();
    assert_eq!(
        sent_body(&server, 2)["toolConfig"],
        json!({"functionCallingConfig": {"mode": "AUTO"},
            "includeServerSideToolInvocations": true})
    );
}

#[test]
fn without_a_hosted_search_the_tool_config_has_no_flag() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Gemini::new(endpoint(&server)).request(&request())))
        .0
        .unwrap();
    let config = sent_body(&server, 0)["toolConfig"].clone();
    assert_eq!(config.as_object().unwrap().len(), 1);
    assert_eq!(
        config,
        json!({"functionCallingConfig": {"mode": "VALIDATED"}})
    );
}

#[test]
fn open_returns_the_reply_bytes_unread() {
    use std::io::Read;
    let bytes = recording("sse2-stream-ok.json");
    let server = ProviderServer::start([Response::stream(bytes.clone())]).unwrap();
    let mut out = Vec::new();
    Gemini::new(endpoint(&server))
        .request(&request())
        .open()
        .unwrap()
        .read_to_end(&mut out)
        .unwrap();
    assert_eq!(out, bytes);
}

#[test]
fn a_sent_declaration_without_a_schema_is_not_strict() {
    // The strictness comes from each sent declaration's
    // `parametersJsonSchema`: one without it counts as loose, whatever
    // the wired tools hold.
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let sent = vec![
        json!({"name": "b_tool", "description": "Sent."}),
        json!({"name": "a_tool", "description": "Sent."}),
    ];
    let mut request = request();
    request.sent_tools = Some(
        sent.iter()
            .map(|tool| tool.as_object().unwrap().clone())
            .collect(),
    );
    run(Box::new(Gemini::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(body["tools"][0]["functionDeclarations"], Value::Array(sent));
    assert_eq!(
        body["toolConfig"],
        json!({"functionCallingConfig": {"mode": "AUTO"}})
    );
}

/// A recording saved by the `record` jig, response bytes only.
fn recorded(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/recordings")
            .join(name),
    )
    .unwrap()
}

/// What the Gemini web-search recording holds, read independently of the
/// decoder: the search parts in arrival order, the last grounding and the
/// readable text.
struct RecordedSearch {
    search: Vec<Value>,
    grounding: Option<Value>,
    text: String,
}

impl RecordedSearch {
    /// The recording's `toolCall` parts, in arrival order.
    fn calls(&self) -> impl Iterator<Item = &Value> {
        self.search
            .iter()
            .filter(|part| part.get("toolCall").is_some())
    }

    /// The recording's `toolResponse` parts, in arrival order.
    fn responses(&self) -> impl Iterator<Item = &Value> {
        self.search
            .iter()
            .filter(|part| part.get("toolResponse").is_some())
    }
}

fn recorded_search(bytes: &[u8]) -> RecordedSearch {
    let mut out = RecordedSearch {
        search: Vec::new(),
        grounding: None,
        text: String::new(),
    };
    for line in String::from_utf8_lossy(bytes).lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let event: Value = serde_json::from_str(data).unwrap();
        let candidate = &event["candidates"][0];
        for part in candidate["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if part.get("toolCall").is_some() || part.get("toolResponse").is_some() {
                out.search.push(part.clone());
            } else if part.get("thought").and_then(Value::as_bool) != Some(true) {
                out.text
                    .push_str(part.get("text").and_then(Value::as_str).unwrap_or(""));
            }
        }
        if let Some(grounding) = candidate.get("groundingMetadata") {
            out.grounding = Some(grounding.clone());
        }
    }
    out
}

fn grounding_urls(grounding: &Value) -> Vec<&str> {
    grounding["groundingChunks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|chunk| chunk.pointer("/web/uri").and_then(Value::as_str))
        .collect()
}

fn hosted_actions(reply: &Reply) -> Vec<&HostedCall> {
    reply
        .actions
        .iter()
        .filter_map(|action| match action {
            ReplyAction::Hosted(hosted) => Some(hosted),
            ReplyAction::Reasoning(_) | ReplyAction::Text(_) | ReplyAction::ToolCall(_) => None,
        })
        .collect()
}

#[test]
fn the_recorded_search_decodes_each_call_and_response_as_a_hosted_call() {
    let bytes = recorded("gemini-web-search.sse");
    let want = recorded_search(&bytes);
    assert!(
        want.calls().next().is_some(),
        "the recording holds a search"
    );
    assert!(want.grounding.is_some(), "the recording holds a grounding");
    let (reply, deltas) = decoded(&bytes);
    let reply = reply.unwrap();
    assert!(
        deltas
            .iter()
            .all(|delta| !matches!(delta, Delta::ToolCallArguments(_))),
        "a hosted search streams no arguments"
    );
    let hosted = hosted_actions(&reply);
    assert_eq!(hosted.len(), want.calls().count());
    for (n, (pair, call)) in hosted.iter().zip(want.calls()).enumerate() {
        assert_eq!(pair.call.name, "web_search");
        assert_eq!(pair.call.arguments, call["toolCall"]["args"]);
        assert_eq!(
            pair.call.provider_id,
            call["toolCall"]
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(|id| ProviderCallId(id.to_owned()))
        );
        assert_eq!(pair.call.provider_item, Some(call.clone()));
        let response = want
            .responses()
            .find(|response| response["toolResponse"]["id"] == call["toolCall"]["id"])
            .unwrap();
        // The result block is tens of kilobytes: compare without printing
        // it whole on failure.
        large::assert_json_eq(
            response,
            pair.completed.provider_item.as_ref().unwrap(),
            "the hosted completion's result block",
        );
        assert_eq!(pair.completed.status, CallStatus::Completed);
        if n + 1 == hosted.len() {
            // The grounding's URLs go on the last completion, with the
            // grounding itself as its details.
            let grounding = want.grounding.as_ref().unwrap();
            let urls = grounding_urls(grounding);
            assert!(!urls.is_empty());
            assert_eq!(
                pair.completed.content,
                vec![ContentPart::Text {
                    text: urls.join("\n")
                }]
            );
            large::assert_json_eq(
                grounding,
                pair.completed.details.as_ref().unwrap(),
                "the last completion's grounding details",
            );
        } else {
            // Gemini reports one grounding per reply, never saying which
            // search found which source: earlier completions stay empty.
            assert_eq!(
                pair.completed.content,
                vec![ContentPart::Text {
                    text: String::new()
                }]
            );
            assert_eq!(pair.completed.details, None);
        }
    }
    assert_eq!(reply.web_searches, None);
    assert_eq!(reply.text(), want.text);
}

#[test]
fn the_recorded_search_replays_its_parts_and_never_its_grounding() {
    let bytes = recorded("gemini-web-search.sse");
    let want = recorded_search(&bytes);
    let server = ProviderServer::start([
        Response::stream(bytes.clone()),
        completed_reply(),
        completed_reply(),
    ])
    .unwrap();
    let provider: Box<dyn Provider> = Box::new(Gemini::new(endpoint(&server)));
    let (reply, _) = run(provider.call(&request()));
    let reply = reply.unwrap();
    // The next request, as the loop renders the reply: the search parts
    // go back unchanged, in order.
    let next = ModelRequest {
        conversation: after(&reply, REFERENCE),
        ..request()
    };
    run(provider.call(&next)).0.unwrap();
    let parts = sent_body(&server, 1)["contents"][1]["parts"].clone();
    let replayed: Vec<Value> = parts
        .as_array()
        .unwrap()
        .iter()
        .filter(|part| part.get("toolCall").is_some() || part.get("toolResponse").is_some())
        .cloned()
        .collect();
    // The search parts include a block of tens of kilobytes: compare
    // without printing them whole on failure.
    large::assert_json_eq(
        &Value::Array(replayed),
        &Value::Array(want.search.clone()),
        "the replayed search parts",
    );
    let raw = String::from_utf8(server.requests()[1].body.clone()).unwrap();
    assert!(
        !raw.contains("groundingMetadata"),
        "the replay sends no grounding: {} bytes",
        raw.len()
    );
    assert!(
        !raw.contains("groundingChunks"),
        "the replay sends no grounding: {} bytes",
        raw.len()
    );
    // Another model reference leaves the search parts out.
    let foreign = ModelRequest {
        conversation: after(&reply, "openai/gpt-6-luna"),
        ..request()
    };
    run(provider.call(&foreign)).0.unwrap();
    let raw = String::from_utf8(server.requests()[2].body.clone()).unwrap();
    assert!(
        !raw.contains("toolCall"),
        "another model gets no search parts: {} bytes",
        raw.len()
    );
    assert!(
        !raw.contains("toolResponse"),
        "another model gets no search parts: {} bytes",
        raw.len()
    );
}

/// A scripted `toolCall` part: `args` and `id` are left out when `None`.
fn tool_call_part(id: Option<&str>, args: Option<Value>) -> Value {
    let mut tool = json!({"toolType": "GOOGLE_SEARCH_WEB"});
    if let Some(id) = id {
        tool["id"] = json!(id);
    }
    if let Some(args) = args {
        tool["args"] = args;
    }
    json!({"thoughtSignature": "c2lnMQ", "toolCall": tool})
}

/// A scripted `toolResponse` part: `id` is left out when `None`.
fn tool_response_part(id: Option<&str>, response: Value) -> Value {
    let mut tool = json!({"toolType": "GOOGLE_SEARCH_WEB"});
    if let Some(id) = id {
        tool["id"] = json!(id);
    }
    tool["response"] = response;
    json!({"thoughtSignature": "c2lnMg", "toolResponse": tool})
}

/// A stream holding each part alone in its own chunk, ending stopped.
fn stream_parts(parts: Vec<Value>) -> Vec<u8> {
    let mut chunks: Vec<Value> = parts
        .into_iter()
        .map(|part| chunk(json!([part]), None))
        .collect();
    chunks.push(chunk(json!([]), Some("STOP")));
    stream(&chunks)
}

/// Each action as its kind with what it logged: a hosted pair as its two
/// blocks, reasoning as its block, text as its words.
fn shapes(reply: &Reply) -> Vec<(String, Value)> {
    reply
        .actions
        .iter()
        .map(|action| match action {
            ReplyAction::Hosted(pair) => (
                "hosted".to_owned(),
                json!([pair.call.provider_item, pair.completed.provider_item]),
            ),
            ReplyAction::Reasoning(reasoning) => (
                "reasoning".to_owned(),
                reasoning.provider_item.clone().unwrap(),
            ),
            ReplyAction::Text(text) => ("text".to_owned(), json!(text.text)),
            ReplyAction::ToolCall(_) => ("toolcall".to_owned(), Value::Null),
        })
        .collect()
}

#[test]
fn a_search_whose_response_has_an_error_completes_failed() {
    let decode_pair = |response: Value| {
        decoded(&stream_parts(vec![
            tool_call_part(Some("c1"), Some(json!({"queries": ["x"]}))),
            tool_response_part(Some("c1"), response),
        ]))
        .0
        .unwrap()
    };
    let response = |response: Value| tool_response_part(Some("c1"), response);
    let failed = &decode_pair(json!({"error": {"status": "UNAVAILABLE"}})).actions;
    let [ReplyAction::Hosted(pair)] = failed.as_slice() else {
        panic!("{failed:?}");
    };
    assert_eq!(pair.completed.status, CallStatus::Failed);
    assert_eq!(
        pair.completed.content,
        vec![ContentPart::Text {
            text: "The provider's search failed: UNAVAILABLE.".into()
        }]
    );
    let error = pair.completed.error.as_ref().unwrap();
    assert_eq!(error.code, ErrorCode::ToolError);
    assert_eq!(error.message, "The provider's search failed: UNAVAILABLE.");
    assert_eq!(
        pair.completed.provider_item,
        Some(response(json!({"error": {"status": "UNAVAILABLE"}})))
    );
    // An `error` without a string `status` fails as `unknown`.
    for error in [json!({"error": {}}), json!({"error": {"status": 7}})] {
        let reply = decode_pair(error);
        let [ReplyAction::Hosted(pair)] = reply.actions.as_slice() else {
            panic!("{:?}", reply.actions);
        };
        assert_eq!(pair.completed.status, CallStatus::Failed);
        match pair.completed.content.as_slice() {
            [ContentPart::Text { text }] => {
                assert_eq!(text, "The provider's search failed: unknown.")
            }
            other => panic!("{other:?}"),
        }
    }
    // No `error` completes, with empty text until the grounding lands.
    let reply = decode_pair(json!({"search_suggestions": "<html>"}));
    let [ReplyAction::Hosted(pair)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(pair.completed.status, CallStatus::Completed);
    assert_eq!(
        pair.completed.content,
        vec![ContentPart::Text {
            text: String::new()
        }]
    );
}

#[test]
fn a_call_pairs_only_with_the_response_right_after_it() {
    let call = |id: &str| tool_call_part(Some(id), Some(json!({"queries": ["x"]})));
    let response = |id: &str| tool_response_part(Some(id), json!({"search_suggestions": "<s>"}));
    let text = json!({"text": "hi"});
    // Each row: the stream's parts, then the exact actions in arrival
    // order. Every row replays the stream's parts unchanged, in order.
    let rows = vec![
        // A call with its response right after it is one hosted search.
        (
            vec![call("a"), response("a")],
            vec![("hosted".to_owned(), json!([call("a"), response("a")]))],
        ),
        // Another call between them leaves the first waiting: it is
        // logged as reasoning, then the pair, then the orphaned response.
        (
            vec![call("a"), call("b"), response("b"), response("a")],
            vec![
                ("reasoning".to_owned(), call("a")),
                ("hosted".to_owned(), json!([call("b"), response("b")])),
                ("reasoning".to_owned(), response("a")),
            ],
        ),
        // A response with another id pairs with neither part.
        (
            vec![call("a"), response("x")],
            vec![
                ("reasoning".to_owned(), call("a")),
                ("reasoning".to_owned(), response("x")),
            ],
        ),
        // Text after a call flushes the waiting call first.
        (
            vec![call("a"), text.clone()],
            vec![
                ("reasoning".to_owned(), call("a")),
                ("text".to_owned(), json!("hi")),
            ],
        ),
        // A call at the end of the stream never met its response.
        (vec![call("a")], vec![("reasoning".to_owned(), call("a"))]),
        // A lone response is reasoning too.
        (
            vec![response("a")],
            vec![("reasoning".to_owned(), response("a"))],
        ),
    ];
    for (parts, want) in rows {
        let reply = decoded(&stream_parts(parts.clone())).0.unwrap();
        assert_eq!(shapes(&reply), want);
        let (contents, _) = sent_contents(after(&reply, REFERENCE));
        assert_eq!(contents[1], json!({"role": "model", "parts": parts}));
    }
}

#[test]
fn a_calls_args_and_id_boundaries() {
    let decode_pair =
        |call: Value, response: Value| decoded(&stream_parts(vec![call, response])).0.unwrap();
    // `args` absent, a string or an array give `{}`; an object is kept.
    for (args, want) in [
        (None, json!({})),
        (Some(json!("q")), json!({})),
        (Some(json!(["q"])), json!({})),
        (Some(json!({"queries": ["x"]})), json!({"queries": ["x"]})),
    ] {
        let reply = decode_pair(
            tool_call_part(Some("c1"), args),
            tool_response_part(Some("c1"), json!({})),
        );
        let [ReplyAction::Hosted(pair)] = reply.actions.as_slice() else {
            panic!("{:?}", reply.actions);
        };
        assert_eq!(pair.call.arguments, want);
    }
    // An empty id carries no `provider_id`, and pairs with an empty one.
    let reply = decode_pair(
        tool_call_part(Some(""), None),
        tool_response_part(Some(""), json!({})),
    );
    let [ReplyAction::Hosted(pair)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(pair.call.provider_id, None);
    // Both ids absent pair the same way, with no `provider_id`.
    let reply = decode_pair(
        tool_call_part(None, None),
        tool_response_part(None, json!({})),
    );
    let [ReplyAction::Hosted(pair)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(pair.call.provider_id, None);
    // A call without an id pairs with a response whose id is `""`.
    let reply = decode_pair(
        tool_call_part(None, None),
        tool_response_part(Some(""), json!({})),
    );
    assert!(matches!(reply.actions.as_slice(), [ReplyAction::Hosted(_)]));
    // A call with an id and a response without one never pair.
    let reply = decode_pair(
        tool_call_part(Some("a"), None),
        tool_response_part(None, json!({})),
    );
    assert!(matches!(
        reply.actions.as_slice(),
        [ReplyAction::Reasoning(_), ReplyAction::Reasoning(_)]
    ));
}

#[test]
fn a_search_then_a_function_call_replays_in_order() {
    let thought = json!({"text": "Checking.", "thought": true, "thoughtSignature": "dGhvdWdodA"});
    let call = tool_call_part(Some("c1"), Some(json!({"queries": ["x"]})));
    let response = tool_response_part(Some("c1"), json!({"search_suggestions": "<s>"}));
    let function = json!({"functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
        "thoughtSignature": "c2FsbA"});
    let grounding =
        json!({"groundingChunks": [{"web": {"uri": "https://a.example", "title": "A"}}]});
    let mut last = chunk(json!([{"text": "Done."}]), Some("STOP"));
    last["candidates"][0]["groundingMetadata"] = grounding.clone();
    let reply = decoded(&stream(&[
        chunk(json!([thought.clone()]), None),
        chunk(json!([call.clone()]), None),
        chunk(json!([response.clone()]), None),
        chunk(json!([function.clone()]), None),
        last,
    ]))
    .0
    .unwrap();
    // The pair sits where it arrived; the call's signature stays on the
    // call after it; the grounding lands on the hosted completion.
    assert!(matches!(
        reply.actions.as_slice(),
        [
            ReplyAction::Reasoning(_),
            ReplyAction::Hosted(_),
            ReplyAction::Reasoning(_),
            ReplyAction::ToolCall(_),
            ReplyAction::Text(_),
        ]
    ));
    let [ReplyAction::Reasoning(_), ReplyAction::Hosted(pair), ..] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(
        pair.completed.content,
        vec![ContentPart::Text {
            text: "https://a.example".into()
        }]
    );
    assert_eq!(pair.completed.details, Some(grounding));
    let (contents, _) = sent_contents(after(&reply, REFERENCE));
    assert_eq!(
        contents[1],
        json!({"role": "model", "parts": [
            {"text": "Checking.", "thought": true, "thoughtSignature": "dGhvdWdodA"},
            call, response,
            {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
             "thoughtSignature": "c2FsbA"},
            {"text": "Done."}]})
    );
    // No search part carries the call's signature; the result goes in the
    // next user content as a `functionResponse`.
    assert_eq!(
        contents[2],
        json!({"role": "user", "parts": [{"functionResponse": {"name": "get_weather",
            "response": {"output": "18 C, clear"}}}]})
    );
}

#[test]
fn the_last_grounding_wins() {
    let old = json!({"groundingChunks": [{"web": {"uri": "https://old.example"}}]});
    let new = json!({"groundingChunks": [{"web": {"uri": "https://new.example"}}]});
    let mut first = chunk(json!([{"text": "a"}]), None);
    first["candidates"][0]["groundingMetadata"] = old;
    let mut last = chunk(json!([{"text": "b"}]), Some("STOP"));
    last["candidates"][0]["groundingMetadata"] = new.clone();
    let reply = decoded(&stream(&[
        chunk(json!([tool_call_part(Some("c1"), None)]), None),
        chunk(json!([tool_response_part(Some("c1"), json!({}))]), None),
        first,
        last,
    ]))
    .0
    .unwrap();
    let [ReplyAction::Hosted(pair), ..] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(
        pair.completed.content,
        vec![ContentPart::Text {
            text: "https://new.example".into()
        }]
    );
    assert_eq!(pair.completed.details, Some(new));
}

#[test]
fn only_the_first_candidate_is_read() {
    let pair = chunk(
        json!([
            tool_call_part(Some("c1"), None),
            tool_response_part(Some("c1"), json!({})),
        ]),
        None,
    );
    let mut event = chunk(json!([{"text": "hi"}]), Some("STOP"));
    event["candidates"]
        .as_array_mut()
        .unwrap()
        .push(pair["candidates"][0].clone());
    event["candidates"][1]["groundingMetadata"] =
        json!({"groundingChunks": [{"web": {"uri": "https://a.example"}}]});
    let reply = decoded(&stream(&[event])).0.unwrap();
    // The second candidate's pair and grounding are never read.
    let [ReplyAction::Text(text)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(text.text, "hi");
}

#[test]
fn grounding_goes_on_the_last_hosted_result_only() {
    let grounded = json!({"groundingChunks": [
        {"web": {"uri": "https://a.example"}},
        {"web": {"uri": 7}},
        {"web": {"uri": "https://b.example"}},
    ]});
    let mut last = chunk(json!([]), Some("STOP"));
    last["candidates"][0]["groundingMetadata"] = grounded.clone();
    let reply = decoded(&stream(&[
        chunk(json!([tool_call_part(Some("c1"), None)]), None),
        chunk(json!([tool_response_part(Some("c1"), json!({}))]), None),
        chunk(json!([tool_call_part(Some("c2"), None)]), None),
        chunk(json!([tool_response_part(Some("c2"), json!({}))]), None),
        last,
    ]))
    .0
    .unwrap();
    let [ReplyAction::Hosted(first), ReplyAction::Hosted(second)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    // Non-string URIs are skipped; only the last completion carries the
    // grounding, as its details and its URLs.
    assert_eq!(
        first.completed.content,
        vec![ContentPart::Text {
            text: String::new()
        }]
    );
    assert_eq!(first.completed.details, None);
    assert_eq!(
        second.completed.content,
        vec![ContentPart::Text {
            text: "https://a.example\nhttps://b.example".into()
        }]
    );
    assert_eq!(second.completed.details, Some(grounded));
    // Grounding with no pair logs nothing.
    let mut alone = chunk(json!([{"text": "hi"}]), Some("STOP"));
    alone["candidates"][0]["groundingMetadata"] =
        json!({"groundingChunks": [{"web": {"uri": "https://a.example"}}]});
    let reply = decoded(&stream(&[alone])).0.unwrap();
    assert!(hosted_actions(&reply).is_empty());
    // Pairs with no grounding complete with empty text.
    let reply = decoded(&stream_parts(vec![
        tool_call_part(Some("c1"), None),
        tool_response_part(Some("c1"), json!({})),
    ]))
    .0
    .unwrap();
    let [ReplyAction::Hosted(pair)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(
        pair.completed.content,
        vec![ContentPart::Text {
            text: String::new()
        }]
    );
    assert_eq!(pair.completed.details, None);
    // A failed last completion keeps its failure text and gets the details.
    let mut failed = chunk(json!([]), Some("STOP"));
    failed["candidates"][0]["groundingMetadata"] =
        json!({"groundingChunks": [{"web": {"uri": "https://a.example"}}]});
    let grounding = failed["candidates"][0]["groundingMetadata"].clone();
    let reply = decoded(&stream(&[
        chunk(json!([tool_call_part(Some("c1"), None)]), None),
        chunk(
            json!([tool_response_part(
                Some("c1"),
                json!({"error": {"status": "UNAVAILABLE"}})
            )]),
            None,
        ),
        failed,
    ]))
    .0
    .unwrap();
    let [ReplyAction::Hosted(pair)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(pair.completed.status, CallStatus::Failed);
    match pair.completed.content.as_slice() {
        [ContentPart::Text { text }] => {
            assert_eq!(text, "The provider's search failed: UNAVAILABLE.")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(pair.completed.details, Some(grounding));
}
