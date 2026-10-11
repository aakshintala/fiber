//! `openai-responses` through the provider crate's public API: the probe
//! recordings decoded, requests on the fake provider server, and
//! cancellation from another thread.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout,
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
use std::sync::Arc;

use contract::events::{ReasoningCompleted, TextDelta, ToolCallRequested};
use contract::provider::{
    CallError, Delta, Finish, Input, InputSize, ModelRequest, Provider, Reply, ReplyAction,
    ToolDefinition,
};
use contract::{ActionId, ErrorCode, GenerationId, ProviderCallId};
use fakes::{ProviderServer, Response};
use provider::openai_responses::{Responses, decode};
use provider::{Compat, Endpoint};
use serde_json::{Value, json};

use probes::Recorded;

use harness::{responses_sse as stream, run, sent_body};

fn research(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../research")
        .join(path)
}

fn decoded(bytes: &[u8]) -> (Result<Reply, provider::Error>, Vec<Delta>) {
    let mut deltas = Vec::new();
    let reply = decode(bytes, &mut |d| deltas.push(d));
    (reply, deltas)
}

fn endpoint(server: &ProviderServer) -> Endpoint {
    Endpoint {
        model: "muse-spark-1.3-contributor".into(),
        base_url: format!("{}/zen/go/v1", server.url()),
        ..harness::endpoint("opencode", server)
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

fn loose_tool() -> ToolDefinition {
    // research/openai-responses-probe: outside the strict subset.
    ToolDefinition {
        name: "f".into(),
        description: "Takes a and maybe b.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
            "required": ["a"]
        }),
        deferred: false,
        hosted: None,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        tools: vec![weather_tool(), loose_tool()],
        thinking: Some(contract::ThinkingLevel::Low),
        cache_key: "s_root".into(),
        conversation: vec![Input::User {
            text: "What is the weather in Paris? Use the tool.".into(),
            images: Vec::new(),
        }],
        ..harness::request()
    }
}

fn completed(status: &str, extra: Value) -> Value {
    let mut response = json!({
        "id": "resp_1",
        "status": status,
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    });
    response
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"type": "response.completed", "response": response})
}

fn text_delta(text: &str) -> Value {
    json!({"type": "response.output_text.delta", "delta": text})
}

fn finished_call() -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call", "id": "fc_1", "call_id": "call_1",
        "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"
    }})
}

#[test]
fn a_function_call_with_empty_argument_text_records_an_empty_object() {
    let call = |arguments: &str| {
        let done = json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1",
            "name": "f", "arguments": arguments}});
        let reply = decoded(&stream(&[done, completed("completed", json!({}))])).0;
        let reply = reply.unwrap();
        let [ReplyAction::ToolCall(call)] = reply.actions.as_slice() else {
            panic!("{:?}", reply.actions);
        };
        call.arguments.clone()
    };
    assert_eq!(call(""), json!({}));
    assert_eq!(call("{\"a\":"), json!("{\"a\":"));
}

fn error_code(result: Result<Reply, provider::Error>) -> (ErrorCode, String) {
    let error = result.unwrap_err();
    (error.code(), error.to_string())
}

// The facts a probe wrapper records beside its stream.
struct Expected {
    text: String,
    calls: Vec<Value>,
    reasoning: usize,
    usage: Value,
}

fn expected(wrapper: &Value) -> Option<Expected> {
    let (items, text, usage): (Vec<Value>, String, Value) =
        if let Some(events) = wrapper.get("events") {
            let items = events
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["event"] == "response.output_item.done")
                .map(|e| e["data"]["item"].clone())
                .collect();
            (
                items,
                wrapper["text"].as_str().unwrap().to_owned(),
                wrapper["usage"].clone(),
            )
        } else {
            let response = wrapper.get("response")?;
            let items = response["output"].as_array().unwrap().clone();
            let text = items
                .iter()
                .filter(|i| i["type"] == "message")
                .flat_map(|i| i["content"].as_array().unwrap().iter())
                .filter_map(|p| p["text"].as_str())
                .collect();
            (items, text, response["usage"].clone())
        };
    Some(Expected {
        text,
        calls: items
            .iter()
            .filter(|i| i["type"] == "function_call")
            .cloned()
            .collect(),
        reasoning: items.iter().filter(|i| i["type"] == "reasoning").count(),
        usage,
    })
}

#[test]
fn every_probe_recording_decodes_into_the_actions_and_usage_it_holds() {
    let mut files = vec![
        research("opencode-probe/raw/go_stream_tools_0.sse"),
        research("opencode-probe/raw/go_stream_tools_1.sse"),
        research("openai-responses-probe/raw/probe.json"),
        // Recorded with the `record` jig, from the request Fiber builds.
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recordings/opencode-go-tool-call.sse"),
    ];
    let mut codex: Vec<PathBuf> = std::fs::read_dir(research("codex-responses-probe/raw"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    codex.sort();
    files.extend(codex);

    let (mut streams, mut checked, mut statuses) = (0, 0, 0);
    for file in &files {
        for exchange in probes::read(file).unwrap() {
            let label = &exchange.label;
            let bytes = match exchange.response {
                Recorded::Stream(bytes) => bytes,
                Recorded::Status(..) => {
                    statuses += 1;
                    continue;
                }
            };
            streams += 1;
            let (reply, deltas) = decoded(&bytes);
            let reply = reply.unwrap_or_else(|e| panic!("{label}: {e}"));
            assert_eq!(reply.finish, Finish::Completed, "{label}");

            // Text deltas add up to the reply's text, and each call's
            // argument deltas to its arguments.
            let text: String = deltas
                .iter()
                .filter_map(|d| match d {
                    Delta::Text(t) => Some(t.text.as_str()),
                    Delta::Reasoning(_) | Delta::ToolCallArguments(_) => None,
                })
                .collect();
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
            let streamed = deltas
                .iter()
                .any(|d| matches!(d, Delta::Text(_) | Delta::ToolCallArguments(_)));
            if streamed {
                assert_eq!(text, reply.text(), "{label}");
                for (index, call) in calls.iter().enumerate() {
                    let raw: String = deltas
                        .iter()
                        .filter_map(|d| match d {
                            Delta::ToolCallArguments(a) if a.index as usize == index => {
                                assert_eq!(a.name.as_deref(), Some(call.name.as_str()));
                                Some(a.text.as_str())
                            }
                            Delta::Text(_) | Delta::Reasoning(_) | Delta::ToolCallArguments(_) => {
                                None
                            }
                        })
                        .collect();
                    let parsed: Value = serde_json::from_str(&raw).unwrap();
                    assert_eq!(parsed, call.arguments, "{label}: call {index}");
                }
            }

            let Some(want) = expected(&exchange.wrapper) else {
                continue;
            };
            checked += 1;
            assert_eq!(reply.text(), want.text, "{label}");
            assert_eq!(calls.len(), want.calls.len(), "{label}");
            for (call, item) in calls.iter().zip(&want.calls) {
                assert_eq!(call.name, item["name"].as_str().unwrap(), "{label}");
                assert_eq!(
                    call.provider_id,
                    Some(ProviderCallId(item["call_id"].as_str().unwrap().into())),
                    "{label}"
                );
                let arguments: Value =
                    serde_json::from_str(item["arguments"].as_str().unwrap()).unwrap();
                assert_eq!(call.arguments, arguments, "{label}");
            }
            let reasoning = reply
                .actions
                .iter()
                .filter(|action| matches!(action, ReplyAction::Reasoning(_)))
                .count();
            assert_eq!(reasoning, want.reasoning, "{label}");
            let cached = want.usage["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap();
            assert_eq!(reply.tokens.cache_read, cached, "{label}");
            assert_eq!(
                reply.tokens.input,
                want.usage["input_tokens"].as_u64().unwrap() - cached,
                "{label}"
            );
            assert_eq!(
                reply.tokens.output,
                want.usage["output_tokens"].as_u64().unwrap(),
                "{label}"
            );
        }
    }
    // 3 OpenCode streams, 12 OpenAI responses and 45 codex streams; the
    // other 5 are HTTP 400s.
    assert_eq!((streams, checked, statuses), (60, 57, 5));
}

#[test]
fn the_opencode_tool_exchange_decodes_call_reasoning_and_answer() {
    let bytes = std::fs::read(research("opencode-probe/raw/go_stream_tools_0.sse")).unwrap();
    let (reply, deltas) = decoded(&bytes);
    let reply = reply.unwrap();
    assert_eq!(reply.text(), "I'll check the weather in Paris for you.");
    assert_eq!(
        reply.generation_id,
        Some(GenerationId("resp_6abe177dec230a748c9f42a9".into()))
    );
    let [
        ReplyAction::Reasoning(reasoning),
        ReplyAction::Text(part),
        ReplyAction::ToolCall(call),
    ] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(part.text, reply.text());
    assert_eq!(part.provider_item.as_ref().unwrap()["type"], "message");
    let item = reasoning.provider_item.as_ref().unwrap();
    assert_eq!(item["type"], "reasoning");
    assert!(item["encrypted_content"].as_str().unwrap().len() > 100);
    assert_eq!(
        call,
        &ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({"city": "Paris"}),
            provider_id: Some(ProviderCallId(
                "call_01a0f68bc62570139103eeb490cad7a4".into()
            )),
            repair: None,
            ran_by: None,
            provider_item: None,
        }
    );
    assert!(deltas.contains(&Delta::ToolCallArguments(
        contract::events::ToolCallArgumentsDelta {
            index: 0,
            name: Some("get_weather".into()),
            text: "{\"city\":\"Paris\"}".into(),
        }
    )));

    let bytes = std::fs::read(research("opencode-probe/raw/go_stream_tools_1.sse")).unwrap();
    let reply = decoded(&bytes).0.unwrap();
    assert_eq!(reply.text(), "The weather in Paris is 18°C and clear.");
    assert_eq!(
        (
            reply.tokens.input,
            reply.tokens.cache_read,
            reply.tokens.output
        ),
        (119, 497, 159)
    );
    assert!(reply.tokens.cache_write.is_empty());
}

#[test]
fn a_recording_served_by_the_fake_server_runs_through_the_seam() {
    let bytes = std::fs::read(research("opencode-probe/raw/go_stream_tools_0.sse")).unwrap();
    let server = ProviderServer::start([Response::stream(bytes.clone())]).unwrap();
    let provider: Box<dyn Provider> = Box::new(Responses::new(endpoint(&server)));
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
    assert_eq!(sent.path, "/zen/go/v1/responses");
    assert_ne!(sent.header("authorization"), None);
    assert!(!String::from_utf8_lossy(&sent.body).contains("sk-secret"));
    assert!(sent.header("user-agent").unwrap().starts_with("fiber/"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
}

#[test]
fn a_request_body_has_the_responses_shape() {
    let reply = || Response::stream(stream(&[completed("completed", json!({}))]));
    let server = ProviderServer::start([reply()]).unwrap();
    let responses = Responses::new(endpoint(&server));
    run(Box::new(responses.request(&request()))).0.unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(body["model"], "muse-spark-1.3-contributor");
    assert_eq!(body["instructions"], "You are terse.");
    assert_eq!(body["stream"], true);
    assert_eq!(body["reasoning"], json!({"effort": "low"}));
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["prompt_cache_key"], "s_root");
    assert_eq!(
        body["input"],
        json!([{"role": "user", "content": "What is the weather in Paris? Use the tool."}])
    );
}

#[test]
fn strict_is_sent_per_tool_and_true_only_for_a_schema_in_the_strict_subset() {
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    run(Box::new(
        Responses::new(endpoint(&server)).request(&request()),
    ))
    .0
    .unwrap();
    let tools = sent_body(&server, 0)["tools"].clone();
    assert_eq!(
        tools,
        json!([
            {"type": "function", "name": "f", "description": "Takes a and maybe b.",
             "parameters": loose_tool().input_schema, "strict": false},
            {"type": "function", "name": "get_weather", "description": "Weather for a city.",
             "parameters": weather_tool().input_schema, "strict": true},
        ])
    );
}

#[test]
fn a_deferred_tool_is_sent_in_full() {
    let reply = || Response::stream(stream(&[completed("completed", json!({}))]));
    let server = ProviderServer::start([reply(), reply()]).unwrap();
    let responses = Responses::new(endpoint(&server));
    let mut deferred = request();
    deferred.tools[0].deferred = true;
    for request in [request(), deferred] {
        run(Box::new(responses.request(&request))).0.unwrap();
    }
    let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(bodies[0], bodies[1]);
}

#[test]
fn store_and_extra_fields_come_from_model_data_only() {
    let reply = || Response::stream(stream(&[completed("completed", json!({}))]));
    let server = ProviderServer::start([reply(), reply()]).unwrap();
    // The same base URL with and without the flag: only the data decides.
    run(Box::new(
        Responses::new(endpoint(&server)).request(&request()),
    ))
    .0
    .unwrap();
    let declared = Endpoint {
        compat: Compat {
            store: Some(false),
            ..Compat::default()
        },
        extra_body: json!({"service_tier": "priority"})
            .as_object()
            .unwrap()
            .clone(),
        ..endpoint(&server)
    };
    run(Box::new(Responses::new(declared).request(&request())))
        .0
        .unwrap();
    let undeclared = sent_body(&server, 0);
    assert_eq!(undeclared.get("store"), None);
    assert_eq!(undeclared.get("service_tier"), None);
    let body = sent_body(&server, 1);
    assert_eq!(body["store"], false);
    assert_eq!(body["service_tier"], "priority");
}

#[test]
fn reasoning_goes_back_unchanged_only_to_the_model_reference_that_produced_it() {
    let bytes = std::fs::read(research("opencode-probe/raw/go_stream_tools_0.sse")).unwrap();
    let reply = decoded(&bytes).0.unwrap();
    let ReplyAction::Reasoning(ReasoningCompleted {
        provider_item: Some(item),
        ..
    }) = &reply.actions[0]
    else {
        panic!("{:?}", reply.actions);
    };
    let ReplyAction::ToolCall(call) = &reply.actions[2] else {
        panic!("{:?}", reply.actions);
    };
    let mut conversation = request().conversation;
    conversation.extend([
        Input::Reasoning {
            model: "openai/gpt-6-luna".into(),
            text: "another model's thoughts".into(),
            provider_item: Some(json!({"type": "reasoning", "encrypted_content": "other"})),
        },
        Input::Reasoning {
            model: "opencode/muse-spark-1.3-contributor".into(),
            text: String::new(),
            provider_item: Some(item.clone()),
        },
        Input::Assistant {
            model: "opencode/muse-spark-1.3-contributor".into(),
            text: reply.text().clone(),
            provider_item: None,
        },
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: call.clone(),
            model: "opencode/muse-spark-1.3-contributor".into(),
        },
        Input::ToolCall {
            action_id: ActionId("a_2".into()),
            call: ToolCallRequested {
                provider_id: None,
                arguments: Value::String("{not json".into()),
                ..call.clone()
            },
            model: "opencode/muse-spark-1.3-contributor".into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_2".into()),
            text: "bad arguments".into(),
            is_error: false,
            images: Vec::new(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_1".into()),
            text: "18 C, clear".into(),
            is_error: false,
            images: Vec::new(),
        },
    ]);
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    let request = ModelRequest {
        conversation,
        ..request()
    };
    run(Box::new(
        Responses::new(endpoint(&server)).request(&request),
    ))
    .0
    .unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(
        body["input"],
        json!([
            {"role": "user", "content": "What is the weather in Paris? Use the tool."},
            item,
            {"role": "assistant", "content": "I'll check the weather in Paris for you."},
            {"type": "function_call", "call_id": "call_01a0f68bc62570139103eeb490cad7a4",
             "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"},
            {"type": "function_call", "call_id": "a_2", "name": "get_weather",
             "arguments": "{not json"},
            {"type": "function_call_output", "call_id": "a_2", "output": "bad arguments"},
            {"type": "function_call_output", "call_id": "call_01a0f68bc62570139103eeb490cad7a4",
             "output": "18 C, clear"},
        ])
    );
    let sent = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert!(!sent.contains("another model"));
    assert!(!sent.contains("\"other\""));
    assert!(sent.contains(item["encrypted_content"].as_str().unwrap()));
}

#[test]
fn a_stream_ending_response_failed_drops_what_it_streamed() {
    let failed = |error: Value| {
        stream(&[
            text_delta("Checking"),
            finished_call(),
            json!({"type": "response.failed", "response": {"id": "resp_1", "status": "failed", "error": error}}),
        ])
    };
    let crashed = || failed(json!({"code": "server_error", "message": "The model crashed."}));
    let server = ProviderServer::start([
        Response::stream(crashed()),
        Response::stream(crashed()).header("x-should-retry", "false"),
    ])
    .unwrap();
    let responses = Responses::new(endpoint(&server));
    let (result, deltas) = run(Box::new(responses.request(&request())));
    let Err(CallError::Failed {
        failure,
        should_retry: None,
        ..
    }) = result
    else {
        panic!("{result:?}");
    };
    // The 200 response's header governs the failure its stream reports.
    let (vetoed, _) = run(Box::new(responses.request(&request())));
    let Err(CallError::Failed {
        should_retry: Some(false),
        ..
    }) = vetoed
    else {
        panic!("{vetoed:?}");
    };
    assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
    let said = failure.provider.unwrap();
    assert_eq!(
        (
            said.name.as_str(),
            said.status.unwrap(),
            said.message.as_str()
        ),
        ("opencode", 200, "The model crashed.")
    );
    // It streamed, and kept none of it.
    assert_eq!(
        deltas,
        [Delta::Text(TextDelta {
            text: "Checking".into()
        })]
    );

    let (code, _) = error_code(
        decoded(&failed(
            json!({"code": "rate_limit_exceeded", "message": ""}),
        ))
        .0,
    );
    assert_eq!(code, ErrorCode::RateLimited);
    let overflow = json!({"code": "invalid_prompt", "message": "Your input exceeds the context window of this model."});
    assert_eq!(
        error_code(decoded(&failed(overflow)).0).0,
        ErrorCode::ContextOverflow
    );
    let (code, _) = error_code(
        decoded(&failed(
            json!({"code": "vector_store_timeout", "message": ""}),
        ))
        .0,
    );
    assert_eq!(code, ErrorCode::StreamIncomplete);
    let (code, _) = error_code(decoded(&failed(Value::Null)).0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
}

#[test]
fn an_error_event_or_a_stream_cut_short_fails_the_call() {
    let error = json!({"type": "error", "code": "server_error", "message": "boom"});
    let (code, _) = error_code(decoded(&stream(&[text_delta("a"), error])).0);
    assert_eq!(code, ErrorCode::ProviderUnavailable);
    let (code, message) = error_code(decoded(&stream(&[text_delta("a"), finished_call()])).0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
    assert!(message.contains("terminal event"), "{message}");
    let (code, _) = error_code(decoded(b"data: {not json}\n\n").0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
}

#[test]
fn each_terminal_status_maps_as_the_docs_say_and_an_unknown_one_fails() {
    let end = |status: &str, extra: Value| decoded(&stream(&[completed(status, extra)])).0;
    assert_eq!(
        end("completed", json!({})).unwrap().finish,
        Finish::Completed
    );
    let cut = end(
        "incomplete",
        json!({"incomplete_details": {"reason": "max_output_tokens"}}),
    );
    assert_eq!(cut.unwrap().finish, Finish::OutputLimit);
    let filtered = end(
        "incomplete",
        json!({"incomplete_details": {"reason": "content_filter"}}),
    );
    assert_eq!(error_code(filtered).0, ErrorCode::Refused);
    for status in ["in_progress", "queued"] {
        assert_eq!(
            error_code(end(status, json!({}))).0,
            ErrorCode::StreamIncomplete
        );
    }
    let (code, message) = error_code(end("cancelled", json!({})));
    assert_eq!(code, ErrorCode::StreamIncomplete);
    assert!(message.contains("cancelled"), "{message}");
    let (code, message) = error_code(end("paused", json!({})));
    assert_eq!(code, ErrorCode::UnknownStopReason);
    assert!(message.contains("paused"), "{message}");
    let (code, message) = error_code(end(
        "incomplete",
        json!({"incomplete_details": {"reason": "tea_break"}}),
    ));
    assert_eq!(code, ErrorCode::UnknownStopReason);
    assert!(message.contains("tea_break"), "{message}");
}

#[test]
fn a_policy_refusal_fails_the_call_as_refused() {
    let refusal = json!({"type": "response.output_item.done", "item": {
        "type": "message", "role": "assistant",
        "content": [{"type": "refusal", "refusal": "I can't help with that."}]
    }});
    let (code, message) =
        error_code(decoded(&stream(&[refusal, completed("completed", json!({}))])).0);
    assert_eq!(code, ErrorCode::Refused);
    assert!(message.contains("I can't help with that."), "{message}");
}

#[test]
fn a_context_overflow_is_recognised_only_in_its_seen_shape() {
    let window = "Your input exceeds the context window of this model.";
    let body = |code: &str, message: &str| {
        json!({"error": {"code": code, "message": message}}).to_string()
    };
    let server = ProviderServer::start([
        Response::status(400, body("invalid_prompt", window)),
        Response::status(400, body("invalid_prompt", "Your prompt was flagged.")),
        Response::status(400, body("other_code", window)),
    ])
    .unwrap();
    let responses = Responses::new(endpoint(&server));
    let codes: Vec<ErrorCode> = (0..3)
        .map(|_| match run(Box::new(responses.request(&request()))).0 {
            Err(CallError::Failed { failure, .. }) => failure.code,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        codes,
        [
            ErrorCode::ContextOverflow,
            ErrorCode::InvalidRequest,
            ErrorCode::InvalidRequest
        ]
    );

    let failed = |error: Value| {
        stream(&[json!({"type": "response.failed",
            "response": {"status": "failed", "error": error}})])
    };
    for error in [
        json!({"code": "invalid_prompt", "message": "Your prompt was flagged."}),
        json!({"code": "other_code", "message": window}),
    ] {
        assert_eq!(
            error_code(decoded(&failed(error)).0).0,
            ErrorCode::StreamIncomplete
        );
    }
}

#[test]
fn reasoning_deltas_stream_as_reasoning_and_an_empty_reply_text_is_not_sent() {
    let (reply, deltas) = decoded(&stream(&[
        json!({"type": "response.reasoning_summary_text.delta", "delta": "Think"}),
        json!({"type": "response.reasoning_text.delta", "delta": "ing"}),
        completed("completed", json!({})),
    ]));
    reply.unwrap();
    assert_eq!(
        deltas,
        [
            Delta::Reasoning(TextDelta {
                text: "Think".into()
            }),
            Delta::Reasoning(TextDelta { text: "ing".into() }),
        ]
    );

    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    let mut request = request();
    request.conversation.push(Input::Assistant {
        model: "opencode/muse-spark-1.3-contributor".into(),
        text: String::new(),
        provider_item: None,
    });
    run(Box::new(
        Responses::new(endpoint(&server)).request(&request),
    ))
    .0
    .unwrap();
    assert_eq!(
        sent_body(&server, 0)["input"],
        json!([{"role": "user", "content": "What is the weather in Paris? Use the tool."}])
    );
}

#[test]
fn a_message_item_replays_unchanged_to_its_model_and_as_text_to_another() {
    let item = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "Hi."}]
    });
    let (reply, _) = decoded(&stream(&[
        json!({"type": "response.output_item.done", "item": item}),
        completed("completed", json!({})),
    ]));
    let reply = reply.unwrap();
    let [ReplyAction::Text(part)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(part.text, "Hi.");
    assert_eq!(
        part.provider_item.as_ref().unwrap()["phase"],
        "final_answer"
    );

    let send = |model: &str| {
        let server = ProviderServer::start([Response::stream(stream(&[completed(
            "completed",
            json!({}),
        )]))])
        .unwrap();
        let mut request = request();
        request.conversation.push(Input::Assistant {
            model: model.into(),
            text: "Hi.".into(),
            provider_item: Some(item.clone()),
        });
        run(Box::new(
            Responses::new(endpoint(&server)).request(&request),
        ))
        .0
        .unwrap();
        sent_body(&server, 0)["input"].clone()
    };
    assert_eq!(send("opencode/muse-spark-1.3-contributor")[1], item);
    assert_eq!(
        send("openai/gpt-6-luna")[1],
        json!({"role": "assistant", "content": "Hi."})
    );
}

#[test]
fn wire_tools_is_what_the_request_sends() {
    let tools = wire_tools::wire_tools_fixture();
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    let responses = Responses::new(endpoint(&server));
    let wired: Vec<Value> = responses
        .wire_tools(&tools)
        .into_iter()
        .map(Value::Object)
        .collect();
    let mut request = request();
    request.tools = tools;
    run(Box::new(responses.request(&request))).0.unwrap();
    let sent = sent_body(&server, 0)["tools"].clone();
    assert_eq!(Value::Array(wired), sent);
    let by_name = |name: &str| {
        sent.as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["strict"]
            .clone()
    };
    assert_eq!(by_name("a_loose"), json!(false));
    assert_eq!(by_name("z_enum"), json!(true));
    assert_eq!(sent.as_array().unwrap().len(), 24);
}

#[test]
fn a_failed_tool_result_sends_the_same_bytes_as_a_success() {
    // OpenAI's Responses API defines no error field on a function-call
    // output (openai-openapi `FunctionCallOutputItemParam` /
    // `FunctionToolCallOutput`: `type, call_id, output, id, name,
    // namespace, caller, status`, where `status` is item progress, not tool
    // failure), so the flag is ignored.
    let conversation = |is_error: bool| {
        vec![
            Input::ToolCall {
                action_id: ActionId("a_1".into()),
                call: ToolCallRequested {
                    name: "get_weather".into(),
                    arguments: json!({"city": "Paris"}),
                    provider_id: Some(ProviderCallId("call_1".into())),
                    repair: None,
                    ran_by: None,
                    provider_item: None,
                },
                model: "opencode/muse-spark-1.3-contributor".into(),
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
    let input = |is_error: bool| {
        let server = ProviderServer::start([Response::stream(stream(&[completed(
            "completed",
            json!({}),
        )]))])
        .unwrap();
        let request = ModelRequest {
            conversation: conversation(is_error),
            ..request()
        };
        run(Box::new(
            Responses::new(endpoint(&server)).request(&request),
        ))
        .0
        .unwrap();
        sent_body(&server, 0)["input"].clone()
    };
    assert_eq!(
        input(true),
        json!([
            {"type": "function_call", "call_id": "call_1",
             "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "boom"}
        ])
    );
    assert_eq!(input(true), input(false));
}

/// A conversation whose one tool result carries `images`.
fn image_conversation(text: &str, images: Vec<contract::provider::ImageRef>) -> Vec<Input> {
    vec![
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.png"}),
                provider_id: Some(ProviderCallId("call_1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "opencode/muse-spark-1.3-contributor".into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_1".into()),
            text: text.into(),
            is_error: false,
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

/// A conversation whose one tool result carries a PDF.
fn pdf_conversation(pdf: contract::provider::PdfRef) -> Vec<Input> {
    vec![
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.pdf"}),
                provider_id: Some(ProviderCallId("call_1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "opencode/muse-spark-1.3-contributor".into(),
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

fn pdf_output(server: &ProviderServer) -> Value {
    sent_body(server, 0)["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap()["output"]
        .clone()
}

#[test]
fn a_stored_pdf_is_sent_as_an_input_file_part_after_the_text() {
    let session = fakes::TempDir::new("fiber-responses-request-pdf");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/p_1.pdf"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: pdf_conversation(pdf_ref("artifacts/p_1.pdf")),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    run(Box::new(
        Responses::new(endpoint(&server)).request(&request),
    ))
    .0
    .unwrap();
    assert_eq!(
        pdf_output(&server),
        json!([
            {"type": "input_text", "text": "PDF: 2 pages.\n"},
            {"type": "input_file", "filename": "p_1.pdf",
             "file_data": "data:application/pdf;base64,YWJjZA=="},
        ])
    );
}

fn image_output(server: &ProviderServer) -> Value {
    sent_body(server, 0)["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap()["output"]
        .clone()
}

#[test]
fn a_stored_image_is_sent_as_input_image_parts_after_the_text() {
    let session = fakes::TempDir::new("fiber-responses-request-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(
            "Image: 2x1 image/png.\n",
            vec![png_ref("artifacts/i_1.png")],
        ),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    run(Box::new(
        Responses::new(endpoint(&server)).request(&request),
    ))
    .0
    .unwrap();
    assert_eq!(
        image_output(&server),
        json!([
            {"type": "input_text", "text": "Image: 2x1 image/png.\n"},
            {"type": "input_image", "image_url": "data:image/png;base64,YWJjZA=="},
        ])
    );
}

#[test]
fn empty_text_sends_no_input_text_part_and_an_unreadable_image_sends_no_part() {
    let session = fakes::TempDir::new("fiber-responses-request-image");
    std::fs::write(session.path().join("i.png"), b"abcd").unwrap();
    let imaged = |text: &str, path: &str| {
        let request = ModelRequest {
            conversation: image_conversation(text, vec![png_ref(path)]),
            session_dir: session.path().to_path_buf(),
            ..request()
        };
        let server = ProviderServer::start([Response::stream(stream(&[completed(
            "completed",
            json!({}),
        )]))])
        .unwrap();
        run(Box::new(
            Responses::new(endpoint(&server)).request(&request),
        ))
        .0
        .unwrap();
        image_output(&server)
    };
    let output = imaged("", "i.png");
    assert_eq!(output.as_array().map(Vec::len), Some(1));
    assert_eq!(output[0]["type"], "input_image");
    assert_eq!(
        imaged("Image: 2x1 image/png.\n", "artifacts/gone.png"),
        json!("Image: 2x1 image/png.\n[Image artifacts/gone.png could not be read.]")
    );
}

#[test]
fn a_text_only_model_gets_a_string_output_saying_the_image_was_left_out() {
    let session = fakes::TempDir::new("fiber-responses-request-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(
            "Image: 2x1 image/png.\n",
            vec![png_ref("artifacts/i_1.png")],
        ),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let reply = || Response::stream(stream(&[completed("completed", json!({}))]));
    let server = ProviderServer::start([reply(), reply()]).unwrap();
    let endpoint = Endpoint {
        text_only: true,
        ..endpoint(&server)
    };
    let responses = Responses::new(endpoint);
    for _ in 0..2 {
        run(Box::new(responses.request(&request))).0.unwrap();
    }
    let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(bodies[0], bodies[1], "a resume sends the same bytes");
    assert_eq!(
        image_output(&server),
        json!(
            "Image: 2x1 image/png.\n[Image artifacts/i_1.png left out: this model does not take images.]"
        )
    );
}

#[test]
fn thinking_levels_map_to_the_reasoning_effort() {
    use contract::ThinkingLevel::{Low, Off, Xhigh};
    let reply = || Response::stream(stream(&[completed("completed", json!({}))]));
    let server = ProviderServer::start([reply(), reply(), reply(), reply()]).unwrap();
    for level in [None, Some(Off), Some(Low), Some(Xhigh)] {
        let mut req = request();
        req.thinking = level;
        run(Box::new(Responses::new(endpoint(&server)).request(&req)))
            .0
            .unwrap();
    }
    let none = sent_body(&server, 0);
    assert_eq!(none.get("reasoning"), None);
    assert_eq!(
        sent_body(&server, 1)["reasoning"],
        json!({"effort": "none"})
    );
    assert_eq!(sent_body(&server, 2)["reasoning"], json!({"effort": "low"}));
    assert_eq!(
        sent_body(&server, 3)["reasoning"],
        json!({"effort": "xhigh"})
    );
}

#[test]
fn a_codex_base_url_still_sends_the_reasoning_effort() {
    use contract::ThinkingLevel::Xhigh;
    let reply = || Response::stream(stream(&[completed("completed", json!({}))]));
    let server = ProviderServer::start([reply()]).unwrap();
    let mut req = request();
    req.thinking = Some(Xhigh);
    let endpoint = Endpoint {
        base_url: format!("{}/backend-api/codex", server.url()),
        ..endpoint(&server)
    };
    run(Box::new(Responses::new(endpoint).request(&req)))
        .0
        .unwrap();
    assert_eq!(server.requests()[0].path, "/backend-api/codex/responses");
    assert_eq!(
        sent_body(&server, 0)["reasoning"],
        json!({"effort": "xhigh"})
    );
}

/// A conversation whose one user message carries `images`.
fn user_conversation(text: &str, images: Vec<contract::provider::ImageRef>) -> Vec<Input> {
    vec![Input::User {
        text: text.into(),
        images,
    }]
}

fn user_item(server: &ProviderServer) -> Value {
    sent_body(server, 0)["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["role"] == "user")
        .unwrap()
        .clone()
}

#[test]
fn a_users_image_is_sent_as_input_image_parts_after_the_text() {
    let session = fakes::TempDir::new("fiber-responses-user-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: user_conversation("look", vec![png_ref("artifacts/i_1.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    run(Box::new(
        Responses::new(endpoint(&server)).request(&request),
    ))
    .0
    .unwrap();
    assert_eq!(
        user_item(&server),
        json!({"role": "user", "content": [
            {"type": "input_text", "text": "look"},
            {"type": "input_image", "image_url": "data:image/png;base64,YWJjZA=="},
        ]})
    );
}

/// Adds the account-id header a codex `credential()` returns.
struct AccountId;

impl contract::signing::Signer for AccountId {
    fn sign(
        &self,
        _: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![(
            "chatgpt-account-id".to_owned(),
            "acct_123".to_owned(),
        )])
    }
}

fn codex_endpoint(server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: "codex".into(),
        model: "gpt-6-luna".into(),
        base_url: server.url(),
        key: None,
        headers: vec![("originator".to_owned(), "fiber".to_owned())],
        signer: Some(Arc::new(AccountId)),
        compat: Compat {
            store: Some(false),
            ..Compat::default()
        },
        direct: true,
        ..Endpoint::default()
    }
}

#[test]
fn a_codex_request_carries_the_codex_headers_and_body() {
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    run(Box::new(
        Responses::new(codex_endpoint(&server))
            .cache_key_header("session_id")
            .request(&request()),
    ))
    .0
    .unwrap();
    let sent = &server.requests()[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/responses");
    assert_eq!(sent.header("session_id"), Some("s_root"));
    assert_eq!(sent.header("originator"), Some("fiber"));
    assert_eq!(sent.header("chatgpt-account-id"), Some("acct_123"));
    assert_eq!(sent.header("openai-beta"), None);
    let body = sent_body(&server, 0);
    assert_eq!(body["model"], "gpt-6-luna");
    assert_eq!(body["instructions"], "You are terse.");
    assert_eq!(body["store"], false);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body.get("temperature"), None);
    assert_eq!(body.get("service_tier"), None);
}

// unprobed: shape from pi `parseErrorResponse` and codex-cli strings,
// `research/codex-responses-probe`, "The usage-limit error body".
fn usage_limit_response(status: u16) -> Response {
    Response::status(
        status,
        json!({"error": {
            "type": "usage_limit_reached",
            "message": "The usage limit has been reached",
            "plan_type": "plus",
            "resets_at": 1_791_396_000,
        }})
        .to_string(),
    )
    .header("date", "Wed, 07 Oct 2026 16:00:00 GMT")
}

#[test]
fn a_usage_limit_status_is_quota_exceeded_with_its_reset_wait() {
    let server =
        ProviderServer::start([usage_limit_response(429), usage_limit_response(400)]).unwrap();
    let responses = Responses::new(codex_endpoint(&server)).cache_key_header("session_id");
    for status in [429, 400] {
        let Err(CallError::Failed {
            failure,
            should_retry,
            ..
        }) = run(Box::new(responses.request(&request()))).0
        else {
            panic!("a usage-limit {status} was not a failure");
        };
        assert_eq!(failure.code, ErrorCode::QuotaExceeded, "{status}");
        assert_eq!(failure.retry_after_ms, Some(7_200_000), "{status}");
        assert_eq!(should_retry, None, "{status}: never retried");
        assert!(
            failure.message.contains("2026-10-07 18:00 UTC"),
            "{status}: {}",
            failure.message
        );
    }
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn a_usage_limit_in_the_stream_is_quota_exceeded() {
    let failed = |error: Value| {
        stream(&[json!({"type": "response.failed",
            "response": {"id": "resp_1", "status": "failed", "error": error}})])
    };
    for error in [
        json!({"code": "usage_not_included", "message": "Not included."}),
        json!({"type": "usage_limit_reached", "message": "Limited."}),
    ] {
        assert_eq!(
            error_code(decoded(&failed(error)).0).0,
            ErrorCode::QuotaExceeded
        );
    }
    let event = json!({"type": "error", "code": "usage_limit_reached", "message": "Limited."});
    assert_eq!(
        error_code(decoded(&stream(&[event])).0).0,
        ErrorCode::QuotaExceeded
    );
    let limited = json!({"code": "rate_limit_exceeded", "message": "Slow down."});
    assert_eq!(
        error_code(decoded(&failed(limited)).0).0,
        ErrorCode::RateLimited
    );
}

fn hosted_tool() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".into(),
        description: String::new(),
        input_schema: json!({}),
        deferred: false,
        hosted: Some("web_search".into()),
    }
}

fn search_item(id: &str) -> Value {
    json!({"type": "web_search_call", "id": id, "status": "completed",
        "action": {"type": "search", "query": "rust 1.90"}})
}

fn answer_item() -> Value {
    json!({"id": "msg_1", "type": "message", "role": "assistant",
        "content": [{"type": "output_text", "text": "Rust 1.90 is out."}]})
}

fn hosted_assistant(model: &str, item: Value) -> Input {
    Input::Assistant {
        model: model.into(),
        text: String::new(),
        provider_item: Some(item),
    }
}

#[track_caller]
fn sent_input(conversation: Vec<Input>) -> Value {
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    let request = ModelRequest {
        conversation,
        ..request()
    };
    run(Box::new(
        Responses::new(endpoint(&server)).request(&request),
    ))
    .0
    .unwrap();
    sent_body(&server, 0)["input"].clone()
}

const HOSTED_REFERENCE: &str = "opencode/muse-spark-1.3-contributor";

#[test]
fn a_hosted_tool_is_sent_as_its_type_alone() {
    let tools = vec![weather_tool(), hosted_tool()];
    let wired: Vec<Value> = Responses::new(Endpoint::default())
        .wire_tools(&tools)
        .into_iter()
        .map(Value::Object)
        .collect();
    assert_eq!(
        wired,
        vec![
            json!({"type": "function", "name": "get_weather",
                "description": "Weather for a city.",
                "parameters": weather_tool().input_schema, "strict": true}),
            json!({"type": "web_search"}),
        ]
    );
    let server = ProviderServer::start([Response::stream(stream(&[completed(
        "completed",
        json!({}),
    )]))])
    .unwrap();
    let responses = Responses::new(endpoint(&server));
    let mut sent = request();
    sent.tools = tools;
    run(Box::new(responses.request(&sent))).0.unwrap();
    assert_eq!(sent_body(&server, 0)["tools"], Value::Array(wired));
}

#[test]
fn a_hosted_item_goes_back_once_unchanged_to_the_model_that_produced_it() {
    let ws = search_item("ws_1");
    let msg = answer_item();
    let input = sent_input(vec![
        Input::User {
            text: "Which Rust is new?".into(),
            images: Vec::new(),
        },
        hosted_assistant(HOSTED_REFERENCE, ws.clone()),
        hosted_assistant(HOSTED_REFERENCE, ws.clone()),
        Input::Assistant {
            model: HOSTED_REFERENCE.into(),
            text: "Rust 1.90 is out.".into(),
            provider_item: Some(msg.clone()),
        },
        Input::User {
            text: "Thanks.".into(),
            images: Vec::new(),
        },
    ]);
    assert_eq!(
        input,
        json!([
            {"role": "user", "content": "Which Rust is new?"},
            ws,
            msg,
            {"role": "user", "content": "Thanks."},
        ])
    );
}

#[test]
fn an_equal_hosted_item_after_another_input_goes_back_again() {
    let ws = search_item("ws_1");
    let input = sent_input(vec![
        hosted_assistant(HOSTED_REFERENCE, ws.clone()),
        Input::User {
            text: "Again?".into(),
            images: Vec::new(),
        },
        hosted_assistant(HOSTED_REFERENCE, ws.clone()),
    ]);
    assert_eq!(
        input,
        json!([
            ws,
            {"role": "user", "content": "Again?"},
            ws,
        ])
    );
}

#[test]
fn two_different_hosted_items_in_a_row_both_go_back() {
    let first = search_item("ws_1");
    let second = search_item("ws_2");
    let input = sent_input(vec![
        hosted_assistant(HOSTED_REFERENCE, first.clone()),
        hosted_assistant(HOSTED_REFERENCE, second.clone()),
    ]);
    assert_eq!(input, json!([first, second]));
}

#[test]
fn two_equal_message_items_in_a_row_both_go_back() {
    let msg = answer_item();
    let input = sent_input(vec![
        Input::Assistant {
            model: HOSTED_REFERENCE.into(),
            text: "Rust 1.90 is out.".into(),
            provider_item: Some(msg.clone()),
        },
        Input::Assistant {
            model: HOSTED_REFERENCE.into(),
            text: "Rust 1.90 is out.".into(),
            provider_item: Some(msg.clone()),
        },
    ]);
    assert_eq!(input, json!([msg, msg]));
}

#[test]
fn a_hosted_pair_is_left_out_for_another_model() {
    let ws = search_item("ws_1");
    let msg = answer_item();
    let input = sent_input(vec![
        Input::User {
            text: "Which Rust is new?".into(),
            images: Vec::new(),
        },
        hosted_assistant("openai/gpt-6-luna", ws.clone()),
        hosted_assistant("openai/gpt-6-luna", ws),
        Input::Assistant {
            model: "openai/gpt-6-luna".into(),
            text: "Rust 1.90 is out.".into(),
            provider_item: Some(msg),
        },
    ]);
    assert_eq!(
        input,
        json!([
            {"role": "user", "content": "Which Rust is new?"},
            {"role": "assistant", "content": "Rust 1.90 is out."},
        ])
    );
}

fn search_done(item: Value) -> Value {
    json!({"type": "response.output_item.done", "item": item})
}

fn search_item_with(action: Value, extra: Value) -> Value {
    let mut item = json!({"type": "web_search_call", "id": "ws_1",
        "status": "completed", "action": action});
    item.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    item
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

/// The recording's `response.output_item.done` items, in order. Lines
/// that are not JSON, such as muse's trailing `[DONE]`, are skipped:
/// the decoder stops at the terminal event before them.
fn recorded_items(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .filter(|event| {
            event.get("type").and_then(Value::as_str) == Some("response.output_item.done")
        })
        .map(|event| event["item"].clone())
        .collect()
}

fn hosted_calls(reply: &Reply) -> Vec<&contract::provider::HostedCall> {
    reply
        .actions
        .iter()
        .filter_map(|action| match action {
            ReplyAction::Hosted(hosted) => Some(hosted),
            ReplyAction::Reasoning(_) | ReplyAction::Text(_) | ReplyAction::ToolCall(_) => None,
        })
        .collect()
}

fn hosted_text(hosted: &contract::provider::HostedCall) -> &str {
    match hosted.completed.content.as_slice() {
        [contract::shapes::ContentPart::Text { text }] => text,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_recorded_openai_search_decodes_its_query_and_sources() {
    let bytes = recorded("openai-web-search.sse");
    let items = recorded_items(&bytes);
    let searches: Vec<&Value> = items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
        .collect();
    assert!(!searches.is_empty(), "the recording holds a search");
    let (reply, deltas) = decoded(&bytes);
    let reply = reply.unwrap();
    assert!(
        deltas
            .iter()
            .all(|d| !matches!(d, Delta::ToolCallArguments(_))),
        "a hosted call streams no arguments"
    );
    let hosted = hosted_calls(&reply);
    assert_eq!(hosted.len(), searches.len());
    for (pair, item) in hosted.iter().zip(searches) {
        // Arguments are the action less `sources`; both logged lines are
        // the item itself.
        let mut action = item["action"].clone();
        action.as_object_mut().unwrap().remove("sources");
        assert_eq!(pair.call.name, "web_search");
        assert_eq!(pair.call.arguments, action);
        // A done item is several kilobytes: compare without printing it
        // whole on failure.
        large::assert_json_eq(
            item,
            pair.call.provider_item.as_ref().unwrap(),
            "the hosted call's logged item",
        );
        large::assert_json_eq(
            item,
            pair.completed.provider_item.as_ref().unwrap(),
            "the hosted result's logged item",
        );
        assert_eq!(
            pair.completed.status,
            contract::events::CallStatus::Completed
        );
        let urls: Vec<&str> = item["action"]["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|source| source.get("url").and_then(Value::as_str))
            .collect();
        assert!(!urls.is_empty(), "the recording asked for sources");
        assert_eq!(hosted_text(pair), urls.join("\n"));
    }
    assert_eq!(reply.web_searches, None);
}

#[test]
fn a_search_without_sources_completes_with_no_urls() {
    let bytes = recorded("openai-web-search-no-sources.sse");
    let items = recorded_items(&bytes);
    assert!(
        items
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call")),
        "the recording holds a search"
    );
    assert!(
        !String::from_utf8_lossy(&bytes).contains("\"sources\""),
        "the recording carries no sources"
    );
    let reply = decoded(&bytes).0.unwrap();
    let hosted = hosted_calls(&reply);
    assert_eq!(hosted.len(), 1);
    assert_eq!(
        hosted[0].completed.status,
        contract::events::CallStatus::Completed
    );
    assert_eq!(hosted_text(hosted[0]), "");
}

#[test]
fn the_recorded_muse_search_takes_its_urls_from_results() {
    let bytes = recorded("muse-web-search.sse");
    let items = recorded_items(&bytes);
    let searches: Vec<&Value> = items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
        .collect();
    assert!(!searches.is_empty(), "the recording holds searches");
    let reply = decoded(&bytes).0.unwrap();
    let hosted = hosted_calls(&reply);
    assert_eq!(hosted.len(), searches.len());
    for (pair, item) in hosted.iter().zip(searches) {
        assert_eq!(
            pair.completed.status,
            contract::events::CallStatus::Completed
        );
        if item["action"].get("type").and_then(Value::as_str) == Some("open_page") {
            // An `open_page` item names its URL on the action.
            assert_eq!(hosted_text(pair), item["action"]["url"].as_str().unwrap());
        } else {
            let urls: Vec<&str> = item["results"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|result| result.get("url").and_then(Value::as_str))
                .collect();
            assert!(!urls.is_empty(), "the recording holds results");
            assert_eq!(hosted_text(pair), urls.join("\n"));
        }
    }
    assert_eq!(reply.web_searches, None);
}

#[test]
fn results_win_over_sources_in_the_recording() {
    let bytes = recorded("muse-web-search-sources.sse");
    let items = recorded_items(&bytes);
    let reply = decoded(&bytes).0.unwrap();
    let hosted = hosted_calls(&reply);
    let searches: Vec<&Value> = items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
        .collect();
    assert_eq!(hosted.len(), searches.len());
    for (pair, item) in hosted.iter().zip(searches) {
        if item["action"].get("type").and_then(Value::as_str) == Some("open_page") {
            continue;
        }
        // Both lists name the same URLs: the content is the results, each
        // once, never results then sources.
        let results: Vec<&str> = item["results"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|result| result.get("url").and_then(Value::as_str))
            .collect();
        assert!(!results.is_empty());
        assert!(!item["action"]["sources"].as_array().unwrap().is_empty());
        assert_eq!(hosted_text(pair), results.join("\n"));
    }
}

#[test]
fn urls_are_results_else_sources_else_the_action_url() {
    let content = |item: &Value| {
        let events = vec![search_done(item.clone()), completed("completed", json!({}))];
        let reply = decoded(&stream(&events)).0.unwrap();
        let [ReplyAction::Hosted(hosted)] = reply.actions.as_slice() else {
            panic!("{:?}", reply.actions);
        };
        assert_eq!(
            hosted.completed.status,
            contract::events::CallStatus::Completed
        );
        hosted_text(hosted).to_owned()
    };
    // Empty results fall through to sources.
    assert_eq!(
        content(&search_item_with(
            json!({"type": "search", "query": "rust 1.90",
                "sources": [{"url": "https://a.example"}]}),
            json!({"results": []}),
        )),
        "https://a.example"
    );
    // Neither list, with an `open_page` URL, gives that URL.
    assert_eq!(
        content(&search_item_with(
            json!({"type": "open_page", "url": "https://a.example"}),
            json!({"results": []}),
        )),
        "https://a.example"
    );
    // Neither list and no URL: empty text.
    assert_eq!(
        content(&search_item_with(
            json!({"type": "search", "query": "rust 1.90"}),
            json!({}),
        )),
        ""
    );
    // Only string URLs count.
    assert_eq!(
        content(&search_item_with(
            json!({"type": "open_page", "url": 7}),
            json!({"results": [{"url": null}]}),
        )),
        ""
    );
    assert_eq!(
        content(&search_item_with(
            json!({"type": "search", "query": "x",
                "sources": [{"url": 7}, {"url": "https://a.example"}]}),
            json!({"results": [{"url": null}]}),
        )),
        "https://a.example"
    );
    // No action, or a non-object one: arguments are empty, results count.
    for action in [None, Some(json!("search"))] {
        let mut item = json!({"type": "web_search_call", "id": "ws_1",
            "status": "completed",
            "results": [{"url": "https://b.example"}]});
        if let Some(action) = action {
            item["action"] = action;
        }
        let reply = decoded(&stream(&[
            search_done(item.clone()),
            completed("completed", json!({})),
        ]))
        .0
        .unwrap();
        let [ReplyAction::Hosted(hosted)] = reply.actions.as_slice() else {
            panic!("{:?}", reply.actions);
        };
        assert_eq!(hosted.call.arguments, json!({}));
        assert_eq!(hosted.completed.provider_item, Some(item));
        assert_eq!(hosted_text(hosted), "https://b.example");
    }
}

#[test]
fn a_web_search_call_that_did_not_complete_completes_failed_with_its_status() {
    let content = |status: Option<&str>| {
        let mut item = json!({"type": "web_search_call", "id": "ws_3",
            "action": {"type": "search", "query": "x"}});
        if let Some(status) = status {
            item["status"] = json!(status);
        }
        let reply = decoded(&stream(&[
            search_done(item.clone()),
            completed("completed", json!({})),
        ]))
        .0
        .unwrap();
        let [ReplyAction::Hosted(hosted)] = reply.actions.as_slice() else {
            panic!("{:?}", reply.actions);
        };
        assert_eq!(hosted.completed.provider_item, Some(item));
        hosted.completed.clone()
    };
    let failed = content(Some("failed"));
    assert_eq!(failed.status, contract::events::CallStatus::Failed);
    assert_eq!(
        failed.content,
        vec![contract::shapes::ContentPart::Text {
            text: "The provider's search failed: failed.".into()
        }]
    );
    let error = failed.error.as_ref().unwrap();
    assert_eq!(error.code, ErrorCode::ToolError);
    assert_eq!(error.message, "The provider's search failed: failed.");
    let unknown = content(None);
    assert_eq!(unknown.status, contract::events::CallStatus::Failed);
    match unknown.content.as_slice() {
        [contract::shapes::ContentPart::Text { text }] => {
            assert_eq!(text, "The provider's search failed: unknown.")
        }
        other => panic!("{other:?}"),
    }
    let done = content(Some("completed"));
    assert_eq!(done.status, contract::events::CallStatus::Completed);
    assert!(done.error.is_none());
}

#[test]
fn the_recorded_searches_run_through_the_seam_and_replay_their_items() {
    for name in ["openai-web-search.sse", "muse-web-search.sse"] {
        let bytes = recorded(name);
        let items = recorded_items(&bytes);
        // Every done item is one the decoder reads: an ignored type, a
        // refusal or a replay mismatch stops here, as a replay bug, never
        // as a bent test.
        for item in &items {
            assert!(
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("message" | "reasoning" | "web_search_call")
                ),
                "{name}: an item the decoder ignores: {item}"
            );
        }
        let server = ProviderServer::start([
            Response::stream(bytes.clone()),
            Response::stream(stream(&[completed("completed", json!({}))])),
        ])
        .unwrap();
        let provider: Box<dyn Provider> = Box::new(Responses::new(endpoint(&server)));
        let (reply, _) = run(provider.call(&request()));
        let reply = reply.unwrap();
        assert_eq!(reply.finish, Finish::Completed);
        // The next request, as the loop renders the reply: reasoning keeps
        // its item, text keeps its item, and a hosted pair goes as two
        // adjacent lines carrying the same item, sent once.
        let mut conversation: Vec<Input> = Vec::new();
        for action in &reply.actions {
            match action {
                ReplyAction::Text(part) => conversation.push(Input::Assistant {
                    model: HOSTED_REFERENCE.into(),
                    text: part.text.clone(),
                    provider_item: part.provider_item.clone(),
                }),
                ReplyAction::Reasoning(reasoning) => conversation.push(Input::Reasoning {
                    model: HOSTED_REFERENCE.into(),
                    text: reasoning.text.clone(),
                    provider_item: reasoning.provider_item.clone(),
                }),
                ReplyAction::Hosted(hosted) => {
                    for item in [
                        hosted.call.provider_item.clone(),
                        hosted.completed.provider_item.clone(),
                    ] {
                        conversation.push(hosted_assistant(HOSTED_REFERENCE, item.unwrap()));
                    }
                }
                ReplyAction::ToolCall(call) => panic!("{name}: a tool call: {call:?}"),
            }
        }
        let next = ModelRequest {
            conversation,
            ..request()
        };
        run(provider.call(&next)).0.unwrap();
        // The replayed input holds whole done items, tens of kilobytes:
        // compare without printing them whole on failure.
        let input = sent_body(&server, 1)["input"].clone();
        large::assert_json_eq(
            &Value::Array(items),
            &input,
            &format!("{name}: the replayed input"),
        );
    }
}
