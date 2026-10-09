//! `anthropic-messages` through the provider crate's public API: the probe
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

#[path = "support/wire_tools.rs"]
mod wire_tools;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::{CacheLifetime, ReasoningCompleted, TextDelta, ToolCallRequested};
use contract::provider::{
    CallError, CallUsage, Delta, Finish, Input, InputSize, ModelCall, ModelRequest, Provider,
    Reply, ReplyAction, ToolDefinition,
};
use contract::shapes::Tokens;
use contract::{ActionId, ErrorCode, GenerationId, ProviderCallId};
use fakes::{ProviderServer, Response};
use provider::Endpoint;
use provider::anthropic_messages::{Messages, decode};
use serde_json::{Value, json};

use probes::Recorded;

const DEADLINE: Duration = Duration::from_secs(10);

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
        provider: "anthropic".into(),
        model: "claude-sonnet-5-5".into(),
        base_url: format!("{}/v1", server.url()),
        key: Some(contract::Secret::new("sk-secret".into())),
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
        hosted: None,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: "You are terse.".into(),
        tools: vec![weather_tool()],
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: "session_1".into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        conversation: vec![Input::User {
            text: "What is the weather in Paris? Use the tool.".into(),
            images: Vec::new(),
        }],
        session_dir: std::path::PathBuf::new(),
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
fn stream(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect::<String>()
        .into_bytes()
}

fn text_block(index: u64, text: &str) -> [Value; 2] {
    [
        json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}),
    ]
}

fn stopped(index: u64) -> Value {
    json!({"type": "content_block_stop", "index": index})
}

fn finished(stop_reason: &str) -> [Value; 2] {
    [
        json!({"type": "message_delta", "delta": {"stop_reason": stop_reason}, "usage": {"input_tokens": 10, "cache_read_input_tokens": 4, "output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]
}

fn started() -> Value {
    json!({"type": "message_start", "message": {"id": "msg_1"}})
}

fn error_code(result: Result<Reply, provider::Error>) -> (ErrorCode, String) {
    let error = result.unwrap_err();
    (error.code(), error.to_string())
}

// The facts a non-streamed probe wrapper (`body`, a full Anthropic Message)
// records beside the content Fiber decodes.
struct Expected {
    text: String,
    calls: Vec<Value>,
    reasoning: usize,
}

/// The usage a recording holds: a non-streamed body's `usage`, or for a
/// real stream (`raw_sse`), the input counts and cache-write split from
/// `message_start` and the output count from the last `message_delta`.
fn recorded_usage(wrapper: &Value) -> Value {
    if let Some(usage) = wrapper.pointer("/body/usage") {
        return usage.clone();
    }
    let events: Vec<Value> = wrapper["raw_sse"]
        .as_str()
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str(data).unwrap())
        .collect();
    let start = events
        .iter()
        .find(|e| e["type"] == "message_start")
        .unwrap();
    let delta = events
        .iter()
        .rfind(|e| e["type"] == "message_delta")
        .unwrap();
    let mut usage = start["message"]["usage"].clone();
    usage["output_tokens"] = delta["usage"]["output_tokens"].clone();
    usage
}

fn expected(wrapper: &Value) -> Option<Expected> {
    let body = wrapper.get("body")?.as_object()?;
    let content = body.get("content")?.as_array()?.clone();
    let text = content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    Some(Expected {
        text,
        calls: content
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .cloned()
            .collect(),
        reasoning: content.iter().filter(|b| b["type"] == "thinking").count(),
    })
}

#[test]
fn every_probe_recording_decodes_into_the_actions_and_usage_it_holds() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(research("anthropic-messages-probe/raw"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();

    let (mut streams, mut checked, mut statuses, mut usages) = (0, 0, 0, 0);
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
            assert!(
                matches!(reply.finish, Finish::Completed | Finish::OutputLimit),
                "{label}"
            );

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

            let usage = recorded_usage(&exchange.wrapper);
            let count = |key: &str| usage[key].as_u64().unwrap();
            let split = |key: &str| usage["cache_creation"][key].as_u64().unwrap();
            let cache_write: BTreeMap<String, u64> = [
                ("5m", split("ephemeral_5m_input_tokens")),
                ("1h", split("ephemeral_1h_input_tokens")),
            ]
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(k, n)| (k.to_owned(), n))
            .collect();
            assert_eq!(
                reply.tokens,
                Tokens {
                    input: count("input_tokens"),
                    cache_read: count("cache_read_input_tokens"),
                    cache_write,
                    output: count("output_tokens"),
                },
                "{label}"
            );
            usages += 1;

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
                    Some(ProviderCallId(item["id"].as_str().unwrap().into())),
                    "{label}"
                );
                assert_eq!(&call.arguments, &item["input"], "{label}");
            }
            let reasoning = reply
                .actions
                .iter()
                .filter(|action| matches!(action, ReplyAction::Reasoning(_)))
                .count();
            assert_eq!(reasoning, want.reasoning, "{label}");
        }
    }
    // 26 streams (16 non-streamed Message bodies, 10 real SSE recordings),
    // every one's usage checked, and 8 HTTP error statuses.
    assert_eq!((streams, checked, usages, statuses), (26, 16, 26, 8));
}

#[test]
fn the_hard_question_stream_decodes_thinking_then_text() {
    let exchanges = probes::read(&research("anthropic-messages-probe/raw/stream.json")).unwrap();
    let exchange = exchanges
        .into_iter()
        .find(|e| e.label.contains("thinking forced"))
        .unwrap();
    let Recorded::Stream(bytes) = exchange.response else {
        panic!("expected a stream");
    };
    let reply = decoded(&bytes).0.unwrap();
    assert!(
        reply.text().starts_with("There are **62**"),
        "{}",
        reply.text()
    );
    let [
        ReplyAction::Reasoning(ReasoningCompleted {
            text,
            provider_item,
        }),
        ReplyAction::Text(part),
    ] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(part.text, reply.text());
    // The model streamed a signature but no readable summary for this
    // question (`docs/events.md`: "`""` when the provider sent none").
    assert_eq!(text, "");
    let item = provider_item.as_ref().unwrap();
    assert_eq!(item["type"], "thinking");
    assert!(item["signature"].as_str().unwrap().len() > 100);
}

#[test]
fn the_tool_use_stream_decodes_the_call() {
    let exchanges = probes::read(&research("anthropic-messages-probe/raw/stream.json")).unwrap();
    let exchange = exchanges
        .into_iter()
        .find(|e| e.label.contains("stream tool use"))
        .unwrap();
    let Recorded::Stream(bytes) = exchange.response else {
        panic!("expected a stream");
    };
    let (reply, deltas) = decoded(&bytes);
    let reply = reply.unwrap();
    let [ReplyAction::ToolCall(call)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(
        call,
        &ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({"city": "Paris"}),
            provider_id: Some(ProviderCallId("toolu_01AXa3EtWnvLzfgA63BeZm68".into())),
            repair: None,
            ran_by: None,
            provider_item: None,
        }
    );
    assert!(deltas.iter().any(
        |d| matches!(d, Delta::ToolCallArguments(a) if a.name.as_deref() == Some("get_weather"))
    ));
    assert_eq!(reply.finish, Finish::Completed);
    assert_eq!(
        reply.generation_id,
        Some(GenerationId("msg_011CfXSx5JPwTKf8hnNi81M7".into()))
    );
}

fn single_call(events: &[Value]) -> Value {
    let reply = decoded(&stream(events)).0.unwrap();
    let [ReplyAction::ToolCall(call)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    call.arguments.clone()
}

#[test]
fn a_tool_use_with_no_input_text_records_an_empty_object() {
    // A tool that takes no arguments streams `input: {}` and no deltas.
    let arguments = single_call(&[
        started(),
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "t1", "name": "f", "input": {}}}),
        stopped(0),
        finished("tool_use")[0].clone(),
        finished("tool_use")[1].clone(),
    ]);
    assert_eq!(arguments, json!({}));
}

#[test]
fn a_call_with_no_input_text_is_sent_back_as_an_empty_object() {
    // The request after the tool result carries the call back; `input: ""`
    // there is an HTTP 400 (#1295).
    let events = [
        started(),
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "toolu_1", "name": "ping", "input": {}}}),
        stopped(0),
        finished("tool_use")[0].clone(),
        finished("tool_use")[1].clone(),
    ];
    let reply = decoded(&stream(&events)).0.unwrap();
    let [ReplyAction::ToolCall(call)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let request = ModelRequest {
        conversation: vec![
            Input::ToolCall {
                action_id: ActionId("a_1".into()),
                call: call.clone(),
                model: "anthropic/claude-sonnet-5-5".into(),
            },
            Input::ToolResult {
                pdfs: Vec::new(),
                action_id: ActionId("a_1".into()),
                text: "pong".into(),
                is_error: false,
                images: Vec::new(),
            },
        ],
        ..request()
    };
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let sent = sent_body(&server, 0)["messages"][0]["content"].clone();
    let tool_use = sent
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "tool_use")
        .unwrap_or_else(|| panic!("{sent}"));
    assert_eq!(tool_use["input"], json!({}));
}

#[test]
fn a_tool_use_whose_input_text_does_not_parse_records_the_raw_text() {
    let arguments = single_call(&[
        started(),
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "t1", "name": "f", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": "{\"a\":"}}),
        stopped(0),
        finished("tool_use")[0].clone(),
        finished("tool_use")[1].clone(),
    ]);
    assert_eq!(arguments, json!("{\"a\":"));
}

#[test]
fn a_tool_uses_empty_array_input_is_not_sent_as_a_seed() {
    // The `Value::Array` arm of the seed filter (`Decoder::start`) has no
    // probed example: every recorded tool call takes an object. An empty
    // array must be treated the same as an empty object, a placeholder the
    // deltas fill in, or the two tool calls below would glue their
    // arguments onto a stray `[]`.
    let (reply, _) = decoded(&stream(&[
        started(),
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "t1", "name": "f", "input": []}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": "[1,2]"}}),
        stopped(0),
        finished("tool_use")[0].clone(),
        finished("tool_use")[1].clone(),
    ]));
    let reply = reply.unwrap();
    let [ReplyAction::ToolCall(call)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(call.arguments, json!([1, 2]));
}

#[test]
fn interleaved_tool_use_blocks_decode_independently_by_index() {
    // Two tool calls whose deltas interleave before either closes, so a
    // decoder that confused their indices would merge their arguments.
    let (reply, deltas) = decoded(&stream(&[
        started(),
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "t0", "name": "first", "input": {}}}),
        json!({"type": "content_block_start", "index": 1, "content_block": {
            "type": "tool_use", "id": "t1", "name": "second", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": "{\"a\":1}"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {
            "type": "input_json_delta", "partial_json": "{\"b\":2}"}}),
        stopped(0),
        stopped(1),
        finished("tool_use")[0].clone(),
        finished("tool_use")[1].clone(),
    ]));
    let reply = reply.unwrap();
    let [ReplyAction::ToolCall(first), ReplyAction::ToolCall(second)] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(first.name, "first");
    assert_eq!(first.arguments, json!({"a": 1}));
    assert_eq!(second.name, "second");
    assert_eq!(second.arguments, json!({"b": 2}));
    let indices: Vec<u32> = deltas
        .iter()
        .filter_map(|d| match d {
            Delta::ToolCallArguments(a) => Some(a.index),
            Delta::Text(_) | Delta::Reasoning(_) => None,
        })
        .collect();
    assert_eq!(indices, [0, 1]);
}

#[test]
fn thinking_delta_text_streams_as_reasoning_and_joins_in_order() {
    let (reply, deltas) = decoded(&stream(&[
        started(),
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "thinking_delta", "thinking": "Let"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "thinking_delta", "thinking": " me think."}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "signature_delta", "signature": "sig"}}),
        stopped(0),
        finished("end_turn")[0].clone(),
        finished("end_turn")[1].clone(),
    ]));
    let reply = reply.unwrap();
    let [
        ReplyAction::Reasoning(ReasoningCompleted {
            text,
            provider_item,
        }),
    ] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(text, "Let me think.");
    assert_eq!(provider_item.as_ref().unwrap()["signature"], "sig");
    assert_eq!(
        deltas,
        [
            Delta::Reasoning(TextDelta { text: "Let".into() }),
            Delta::Reasoning(TextDelta {
                text: " me think.".into()
            }),
        ]
    );
}

#[test]
fn a_recording_served_by_the_fake_server_runs_through_the_seam() {
    let exchanges = probes::read(&research("anthropic-messages-probe/raw/stream.json")).unwrap();
    let exchange = exchanges
        .into_iter()
        .find(|e| e.label.contains("plain text"))
        .unwrap();
    let Recorded::Stream(bytes) = exchange.response else {
        panic!("expected a stream");
    };
    let server = ProviderServer::start([Response::stream(bytes.clone())]).unwrap();
    let provider: Box<dyn Provider> = Box::new(Messages::new(endpoint(&server)));
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
    assert_eq!(sent.path, "/v1/messages");
    assert_ne!(sent.header("x-api-key"), None);
    assert!(!String::from_utf8_lossy(&sent.body).contains("sk-secret"));
    assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
    assert!(sent.header("user-agent").unwrap().starts_with("fiber/"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
}

fn completed_reply() -> Response {
    Response::stream(stream(&[
        started(),
        text_block(0, "hi")[0].clone(),
        text_block(0, "hi")[1].clone(),
        stopped(0),
        finished("end_turn")[0].clone(),
        finished("end_turn")[1].clone(),
    ]))
}

#[test]
fn two_requests_built_from_the_same_inputs_are_the_same_bytes() {
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    let messages = Messages::new(endpoint(&server));
    let mut reordered = request();
    reordered.tools.push(ToolDefinition {
        name: "aaa_tool".into(),
        description: "First alphabetically.".into(),
        input_schema: json!({"type": "object", "properties": {}, "required": []}),
        deferred: false,
        hosted: None,
    });
    for request in [request(), request(), reordered.clone()] {
        run(Box::new(messages.request(&request))).0.unwrap();
    }
    let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0], bodies[1]);
    assert_ne!(
        bodies[0], bodies[2],
        "a different tool set is a different body"
    );
    let body = sent_body(&server, 0);
    assert_eq!(body["model"], "claude-sonnet-5-5");
    let marker = json!({"type": "ephemeral"});
    assert_eq!(
        body["system"],
        json!([{"type": "text", "text": "You are terse.", "cache_control": marker}])
    );
    assert_eq!(body["stream"], true);
    assert_eq!(body["tool_choice"], json!({"type": "auto"}));
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": [
            {"type": "text", "text": "What is the weather in Paris? Use the tool.",
             "cache_control": marker}]}])
    );
    assert_eq!(
        body["tools"],
        json!([{"name": "get_weather", "description": "Weather for a city.",
            "input_schema": weather_tool().input_schema, "strict": true}])
    );
    let reordered = sent_body(&server, 2);
    assert_eq!(reordered["tools"][0]["name"], "aaa_tool");
    assert_eq!(
        reordered["tools"][0]["strict"], false,
        "its schema does not fit the strict subset"
    );
    assert_eq!(body.get("thinking"), None);
    assert_eq!(body.get("output_config"), None);
}

#[test]
fn adaptive_thinking_is_sent_as_an_effort_not_a_token_budget() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut request = request();
    request.thinking = Some(contract::ThinkingLevel::Medium);
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
    assert_eq!(body["output_config"], json!({"effort": "medium"}));
    assert_eq!(body.get("max_tokens"), None);
}

#[test]
fn tool_choice_names_a_tool_outside_anthropics_own_values() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut request = request();
    request.tool_choice = "get_weather".into();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["tool_choice"],
        json!({"type": "tool", "name": "get_weather"})
    );
}

#[test]
fn max_tokens_is_the_models_limit_and_never_exceeds_it() {
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    let limited = |extra: Value| Endpoint {
        max_output_tokens: Some(64_000),
        extra_body: extra.as_object().unwrap().clone(),
        ..endpoint(&server)
    };
    for extra in [
        json!({}),
        json!({"max_tokens": 1024}),
        json!({"max_tokens": 200_000}),
    ] {
        run(Box::new(Messages::new(limited(extra)).request(&request())))
            .0
            .unwrap();
    }
    let sent: Vec<Value> = (0..3)
        .map(|n| sent_body(&server, n)["max_tokens"].clone())
        .collect();
    assert_eq!(sent, [json!(64_000), json!(1024), json!(64_000)]);
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
    run(Box::new(Messages::new(limited.clone()).request(&low)))
        .0
        .unwrap();
    run(Box::new(Messages::new(limited).request(&high)))
        .0
        .unwrap();
    assert_eq!(sent_body(&server, 0)["max_tokens"], 1);
    assert_eq!(sent_body(&server, 1)["max_tokens"], 4096);
}

#[test]
fn an_enum_with_an_object_or_array_value_is_sent_not_strict() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let with_enum = |name: &str, values: Value| ToolDefinition {
        name: name.into(),
        input_schema: json!({
            "type": "object",
            "properties": {"pick": {"type": "string", "enum": values}},
            "required": ["pick"],
            "additionalProperties": false
        }),
        ..weather_tool()
    };
    let mut request = request();
    request.tools = vec![
        with_enum("a_array", json!([["x"]])),
        with_enum("b_object", json!([{"x": 1}])),
        with_enum("c_plain", json!(["x", 1, null])),
    ];
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let strict: Vec<bool> = sent_body(&server, 0)["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["strict"].as_bool().unwrap())
        .collect();
    assert_eq!(strict, [false, false, true]);
}

#[test]
fn past_twenty_strict_tools_the_rest_are_sent_not_strict() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut request = request();
    request.tools = (0..22)
        .map(|i| ToolDefinition {
            name: format!("tool_{i:02}"),
            ..weather_tool()
        })
        .collect();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let strict: Vec<bool> = sent_body(&server, 0)["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["strict"].as_bool().unwrap())
        .collect();
    assert_eq!(strict.iter().filter(|s| **s).count(), 20);
    assert_eq!(&strict[20..], [false, false]);
}

/// Four turns: user, assistant tool call, tool result, user. `previous_end`
/// names the boundary after the first two inputs, so its marker lands on the
/// assistant's tool-call block, not the system prompt or the final message
/// (`docs/prompt-cache.md`, "Cache markers and keys").
fn four_turn_conversation() -> Vec<Input> {
    vec![
        Input::User {
            text: "What is the weather in Paris?".into(),
            images: Vec::new(),
        },
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "get_weather".into(),
                arguments: json!({"city": "Paris"}),
                provider_id: Some(ProviderCallId("toolu_1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "anthropic/claude-sonnet-5-5".into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_1".into()),
            text: "18 C, clear".into(),
            is_error: false,
            images: Vec::new(),
        },
        Input::User {
            text: "And Rome?".into(),
            images: Vec::new(),
        },
    ]
}

#[test]
fn the_previous_end_marker_lands_on_the_boundary_it_names() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let request = ModelRequest {
        conversation: four_turn_conversation(),
        previous_end: Some(2),
        sent_tools: None,
        cache_lifetime: CacheLifetime::OneHour,
        ..request()
    };
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let hour = json!({"type": "ephemeral", "ttl": "1h"});
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                 "input": {"city": "Paris"}, "cache_control": hour}]},
            // The tool result and the next user turn share one message: both
            // are Anthropic's `user` role, and nothing in the conversation
            // starts a new one between them.
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18 C, clear"},
                {"type": "text", "text": "And Rome?", "cache_control": hour}]},
        ])
    );
}

#[test]
fn reasoning_goes_back_unchanged_only_to_the_model_reference_that_produced_it() {
    let exchanges = probes::read(&research("anthropic-messages-probe/raw/stream.json")).unwrap();
    let exchange = exchanges
        .into_iter()
        .find(|e| e.label.contains("thinking forced"))
        .unwrap();
    let Recorded::Stream(bytes) = exchange.response else {
        panic!("expected a stream");
    };
    let reply = decoded(&bytes).0.unwrap();
    let ReplyAction::Reasoning(ReasoningCompleted {
        provider_item: Some(item),
        ..
    }) = &reply.actions[0]
    else {
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
            model: "anthropic/claude-sonnet-5-5".into(),
            text: String::new(),
            provider_item: Some(item.clone()),
        },
        Input::Assistant {
            model: "anthropic/claude-sonnet-5-5".into(),
            text: reply.text().clone(),
            provider_item: None,
        },
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "get_weather".into(),
                arguments: json!({"city": "Paris"}),
                provider_id: Some(ProviderCallId("toolu_1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "anthropic/claude-sonnet-5-5".into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_1".into()),
            text: "18 C, clear".into(),
            is_error: false,
            images: Vec::new(),
        },
    ]);
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let request = ModelRequest {
        conversation,
        ..request()
    };
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": "What is the weather in Paris? Use the tool."},
            {"role": "assistant", "content": [item, {"type": "text", "text": reply.text()},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18 C, clear",
                 "cache_control": {"type": "ephemeral"}}]},
        ])
    );
    let sent = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert!(!sent.contains("another model"));
    assert!(!sent.contains("\"other\""));
    assert!(sent.contains(item["signature"].as_str().unwrap()));
}

#[test]
fn an_error_event_mid_stream_fails_the_call_and_drops_what_it_streamed() {
    let overloaded = stream(&[
        started(),
        text_block(0, "Checking")[0].clone(),
        text_block(0, "Checking")[1].clone(),
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
    ]);
    let server = ProviderServer::start([
        Response::stream(overloaded.clone()),
        Response::stream(overloaded).header("x-should-retry", "false"),
    ])
    .unwrap();
    let messages = Messages::new(endpoint(&server));
    let (result, deltas) = run(Box::new(messages.request(&request())));
    let Err(CallError::Failed {
        failure,
        should_retry: None,
        ..
    }) = result
    else {
        panic!("{result:?}");
    };
    let (vetoed, _) = run(Box::new(messages.request(&request())));
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
        ("anthropic", 200, "Overloaded")
    );
    assert_eq!(
        deltas,
        [Delta::Text(TextDelta {
            text: "Checking".into()
        })]
    );

    let error = |t: &str| stream(&[json!({"type": "error", "error": {"type": t, "message": ""}})]);
    assert_eq!(
        error_code(decoded(&error("rate_limit_error")).0).0,
        ErrorCode::RateLimited
    );
    assert_eq!(
        error_code(decoded(&error("api_error")).0).0,
        ErrorCode::ProviderUnavailable
    );
    assert_eq!(
        error_code(decoded(&error("not_found_error")).0).0,
        ErrorCode::StreamIncomplete
    );
}

#[test]
fn a_stream_cut_short_fails_the_call() {
    let (code, message) =
        error_code(decoded(&stream(&[started(), text_block(0, "a")[0].clone()])).0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
    assert!(message.contains("message_stop"), "{message}");
    let (code, _) = error_code(decoded(b"data: {not json}\n\n").0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
}

#[test]
fn each_stop_reason_maps_as_the_docs_say_and_an_unknown_one_fails() {
    let end = |stop_reason: &str| {
        decoded(&stream(&[
            started(),
            finished(stop_reason)[0].clone(),
            finished(stop_reason)[1].clone(),
        ]))
        .0
    };
    for reason in ["end_turn", "tool_use", "stop_sequence"] {
        assert_eq!(end(reason).unwrap().finish, Finish::Completed, "{reason}");
    }
    assert_eq!(end("max_tokens").unwrap().finish, Finish::OutputLimit);
    assert_eq!(
        error_code(end("model_context_window_exceeded")).0,
        ErrorCode::ContextOverflow
    );
    let (code, message) = error_code(end("refusal"));
    assert_eq!(code, ErrorCode::Refused);
    assert!(message.contains("refusal"), "{message}");
    let (code, message) = error_code(
        decoded(&stream(&[
            started(),
            json!({"type": "message_delta", "delta": {"stop_reason": "refusal",
                "stop_details": {"type": "refusal", "category": "cyber",
                "explanation": "This looks like malware."}}}),
            json!({"type": "message_stop"}),
        ]))
        .0,
    );
    assert_eq!(code, ErrorCode::Refused);
    assert!(message.contains("This looks like malware."), "{message}");
    // No doc says what Fiber does with a paused hosted-tool loop.
    let (code, message) = error_code(end("pause_turn"));
    assert_eq!(code, ErrorCode::UnknownStopReason);
    assert!(message.contains("pause_turn"), "{message}");
}

#[test]
fn a_status_other_than_2xx_fails_with_its_code_and_the_providers_words() {
    let body = |t: &str, message: &str| {
        json!({"type": "error", "error": {"type": t, "message": message}}).to_string()
    };
    let server = ProviderServer::start([
        Response::status(429, body("rate_limit_error", "Slow down.")).header("retry-after", "7"),
        Response::status(400, body("invalid_request_error", "Unsupported parameter.")),
        Response::status(401, "nope"),
        Response::status(529, "{}").header("x-should-retry", "false"),
    ])
    .unwrap();
    let messages = Messages::new(endpoint(&server));
    let mut failures = Vec::new();
    let mut should_retry = Vec::new();
    for _ in 0..4 {
        let Err(CallError::Failed {
            failure,
            should_retry: header,
            ..
        }) = run(Box::new(messages.request(&request()))).0
        else {
            panic!("expected a failure");
        };
        failures.push(failure);
        should_retry.push(header);
    }
    assert_eq!(should_retry, [None, None, None, Some(false)]);
    assert_eq!(failures[0].message, "anthropic answered HTTP 429.");
    assert_eq!(
        failures[2].message,
        "anthropic rejected the credential (HTTP 401). Check the key it is configured \
         with, or log in again with `fiber login anthropic`."
    );
    assert_eq!(failures[0].code, ErrorCode::RateLimited);
    assert_eq!(failures[0].retry_after_ms, Some(7000));
    assert_eq!(failures[0].provider.as_ref().unwrap().message, "Slow down.");
    assert_eq!(failures[1].code, ErrorCode::InvalidRequest);
    assert_eq!(failures[2].code, ErrorCode::AuthenticationFailed);
    // 529 is Anthropic's "overloaded", outside the mapped 4xx/5xx ranges the
    // shared status table already covers as a 5xx server error.
    assert_eq!(failures[3].code, ErrorCode::ProviderUnavailable);
}

#[test]
fn a_context_overflow_is_recognised_only_in_its_seen_shape() {
    let body = |message: &str| {
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": message}})
            .to_string()
    };
    let server = ProviderServer::start([
        Response::status(
            400,
            body("prompt is too long: 220000 tokens > 200000 maximum"),
        ),
        Response::status(400, body("Your prompt was flagged.")),
    ])
    .unwrap();
    let messages = Messages::new(endpoint(&server));
    let codes: Vec<ErrorCode> = (0..2)
        .map(|_| match run(Box::new(messages.request(&request()))).0 {
            Err(CallError::Failed { failure, .. }) => failure.code,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        codes,
        [ErrorCode::ContextOverflow, ErrorCode::InvalidRequest]
    );
}

#[test]
fn a_call_cancelled_before_it_runs_returns_without_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = Endpoint {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        direct: true,
        ..Endpoint::default()
    };
    let call = Messages::new(endpoint).request(&request());
    call.cancel();
    let Err(CallError::Cancelled { usage }) = run(Box::new(call)).0 else {
        panic!("a call cancelled before run returns cancelled");
    };
    // Nothing was read, so the call carries only the body it built.
    assert!(usage.input_size.bytes > 0);
    assert_eq!(*usage, CallUsage::unnamed(usage.input_size));
    let accepted = listener.accept().map(|_| ()).unwrap_err();
    assert_eq!(
        accepted.kind(),
        std::io::ErrorKind::WouldBlock,
        "nothing connected"
    );
}

#[test]
fn cancelling_from_another_thread_ends_a_blocked_read() {
    let open = format!("data: {}\n\n", text_block(0, "Hel")[0]);
    let event = format!("data: {}\n\n", text_block(0, "Hel")[1]);
    let payload = format!("{open}{event}").into_bytes();
    let server =
        ProviderServer::start([Response::stall(200, payload.clone(), payload.len() + 1024)
            .header("content-type", "text/event-stream")])
        .unwrap();
    let endpoint = Endpoint {
        base_url: server.url(),
        direct: true,
        ..Endpoint::default()
    };
    let call: Arc<dyn ModelCall> = Arc::from(Messages::new(endpoint).call(&request()));
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

    // The reader is now blocked waiting for the next bytes.
    call.cancel();
    let result = finished
        .recv_timeout(DEADLINE)
        .expect("waited for run to return after the cancel");
    let sent = InputSize {
        bytes: u64::try_from(server.requests()[0].body.len()).unwrap(),
        media: false,
    };
    assert_eq!(
        result,
        Err(CallError::Cancelled {
            usage: Box::new(CallUsage::unnamed(sent))
        })
    );
    assert!(
        server.await_closed(1, DEADLINE),
        "waited for the server to see the client close"
    );
}

#[test]
fn a_connection_closed_before_any_response_fails_the_call() {
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
        base_url: url,
        direct: true,
        ..Endpoint::default()
    };
    let (result, _) = run(Messages::new(endpoint).call(&request()));
    let Err(CallError::Failed { failure, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(failure.code, ErrorCode::ConnectionFailed);
}

#[test]
fn an_empty_reply_text_is_not_sent() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut request = request();
    request.conversation.push(Input::Assistant {
        model: "anthropic/claude-sonnet-5-5".into(),
        text: String::new(),
        provider_item: None,
    });
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([{"role": "user", "content": [
            {"type": "text", "text": "What is the weather in Paris? Use the tool.",
             "cache_control": {"type": "ephemeral"}}]}])
    );
}

#[test]
fn redacted_thinking_is_kept_and_sent_back_unchanged() {
    let redacted = json!({"type": "redacted_thinking", "data": "EmwKAhgBEgy3va3pzix"});
    let (reply, deltas) = decoded(&stream(&[
        started(),
        json!({"type": "content_block_start", "index": 0, "content_block": redacted}),
        stopped(0),
        finished("end_turn")[0].clone(),
        finished("end_turn")[1].clone(),
    ]));
    assert_eq!(deltas, []);
    let reply = reply.unwrap();
    assert_eq!(
        reply.actions,
        [ReplyAction::Reasoning(ReasoningCompleted {
            text: String::new(),
            provider_item: Some(redacted.clone()),
        })]
    );

    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut request = request();
    request.conversation.extend([
        Input::Reasoning {
            model: "anthropic/claude-sonnet-5-5".into(),
            text: String::new(),
            provider_item: Some(redacted.clone()),
        },
        Input::Assistant {
            model: "anthropic/claude-sonnet-5-5".into(),
            text: "Done.".into(),
            provider_item: None,
        },
    ]);
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(sent_body(&server, 0)["messages"][1]["content"][0], redacted);
}

#[test]
fn cache_writes_are_split_by_lifetime_from_message_start() {
    let (reply, _) = decoded(&stream(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {
            "input_tokens": 3, "cache_creation_input_tokens": 1500,
            "cache_read_input_tokens": 0,
            "cache_creation": {"ephemeral_5m_input_tokens": 500,
                "ephemeral_1h_input_tokens": 1000},
            "output_tokens": 1}}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {
            "input_tokens": 3, "cache_creation_input_tokens": 1500,
            "cache_read_input_tokens": 0, "output_tokens": 9}}),
        json!({"type": "message_stop"}),
    ]));
    assert_eq!(
        reply.unwrap().tokens,
        Tokens {
            input: 3,
            cache_read: 0,
            cache_write: BTreeMap::from([("1h".into(), 1000), ("5m".into(), 500)]),
            output: 9,
        }
    );
}

#[test]
fn text_around_a_tool_call_decodes_and_replays_in_that_order() {
    let (reply, _) = decoded(&stream(&[
        started(),
        text_block(0, "A")[0].clone(),
        text_block(0, "A")[1].clone(),
        stopped(0),
        json!({"type": "content_block_start", "index": 1, "content_block": {
            "type": "tool_use", "id": "t1", "name": "get_weather", "input": {"city": "Paris"}}}),
        stopped(1),
        text_block(2, "B")[0].clone(),
        text_block(2, "B")[1].clone(),
        stopped(2),
        finished("tool_use")[0].clone(),
        finished("tool_use")[1].clone(),
    ]));
    let reply = reply.unwrap();
    assert!(matches!(
        reply.actions.as_slice(),
        [
            ReplyAction::Text(a),
            ReplyAction::ToolCall(_),
            ReplyAction::Text(b),
        ] if a.text == "A" && a.provider_item.is_none() && b.text == "B"
    ));

    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut request = request();
    request.previous_end = Some(2);
    request.conversation.extend([
        Input::Assistant {
            model: "anthropic/claude-sonnet-5-5".into(),
            text: "A".into(),
            provider_item: None,
        },
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "get_weather".into(),
                arguments: json!({"city": "Paris"}),
                provider_id: Some(ProviderCallId("t1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "anthropic/claude-sonnet-5-5".into(),
        },
        Input::Assistant {
            model: "anthropic/claude-sonnet-5-5".into(),
            text: "B".into(),
            provider_item: None,
        },
    ]);
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let marker = json!({"type": "ephemeral"});
    assert_eq!(
        sent_body(&server, 0)["messages"][1],
        json!({"role": "assistant", "content": [
            {"type": "text", "text": "A", "cache_control": marker},
            {"type": "tool_use", "id": "t1", "name": "get_weather", "input": {"city": "Paris"}},
            {"type": "text", "text": "B", "cache_control": {"type": "ephemeral"}},
        ]})
    );
}

#[test]
fn an_empty_text_block_is_not_logged() {
    // A text block with no deltas, then one that has text. A part with
    // neither text nor a provider item is not logged.
    let (reply, _) = decoded(&stream(&[
        started(),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        stopped(0),
        text_block(1, "B")[0].clone(),
        text_block(1, "B")[1].clone(),
        stopped(1),
        finished("end_turn")[0].clone(),
        finished("end_turn")[1].clone(),
    ]));
    let reply = reply.unwrap();
    assert_eq!(reply.text(), "B");
    assert!(matches!(
        reply.actions.as_slice(),
        [ReplyAction::Text(part)] if part.text == "B" && part.provider_item.is_none()
    ));
}

#[test]
fn wire_tools_is_what_the_request_sends() {
    let tools = wire_tools::wire_tools_fixture();
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let messages = Messages::new(endpoint(&server));
    let wired: Vec<Value> = messages
        .wire_tools(&tools)
        .into_iter()
        .map(Value::Object)
        .collect();
    let mut request = request();
    request.tools = tools;
    run(Box::new(messages.request(&request))).0.unwrap();
    let sent = sent_body(&server, 0)["tools"].clone();
    assert_eq!(Value::Array(wired), sent);
    let strict: Vec<bool> = sent
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["strict"].as_bool().unwrap())
        .collect();
    assert_eq!(strict.iter().filter(|s| **s).count(), 20);
    let by_name = |name: &str| {
        sent.as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["strict"]
            .clone()
    };
    assert_eq!(by_name("a_loose"), json!(false));
    assert_eq!(by_name("z_enum"), json!(false));
}

#[test]
fn a_failed_tool_result_sends_is_error_and_a_success_sends_none() {
    // `is_error` is Anthropic's `tool_result` flag
    // (platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls).
    let conversation = |is_error: bool| {
        vec![
            Input::ToolCall {
                action_id: ActionId("a_1".into()),
                call: ToolCallRequested {
                    name: "get_weather".into(),
                    arguments: json!({"city": "Paris"}),
                    provider_id: Some(ProviderCallId("toolu_1".into())),
                    repair: None,
                    ran_by: None,
                    provider_item: None,
                },
                model: "anthropic/claude-sonnet-5-5".into(),
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
    let block = |is_error: bool| {
        let server = ProviderServer::start([completed_reply()]).unwrap();
        let request = ModelRequest {
            conversation: conversation(is_error),
            ..request()
        };
        run(Box::new(Messages::new(endpoint(&server)).request(&request)))
            .0
            .unwrap();
        sent_body(&server, 0)["messages"][1]["content"][0].clone()
    };
    assert_eq!(
        block(true),
        json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": "boom",
               "is_error": true, "cache_control": {"type": "ephemeral"}})
    );
    let success = block(false);
    assert_eq!(
        success,
        json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": "boom",
               "cache_control": {"type": "ephemeral"}})
    );
    assert!(success.get("is_error").is_none());
}

/// A conversation whose one tool result carries `images`.
fn image_conversation(is_error: bool, images: Vec<contract::provider::ImageRef>) -> Vec<Input> {
    vec![
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.png"}),
                provider_id: Some(ProviderCallId("toolu_1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "anthropic/claude-sonnet-5-5".into(),
        },
        Input::ToolResult {
            pdfs: Vec::new(),
            action_id: ActionId("a_1".into()),
            text: "Image: 2x1 image/png.\n".into(),
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

/// The request bodies the server saw for `request`, sent `times` times.
fn bodies_of(request: &ModelRequest, times: usize) -> Vec<Vec<u8>> {
    let server = ProviderServer::start((0..times).map(|_| completed_reply())).unwrap();
    for _ in 0..times {
        run(Box::new(Messages::new(endpoint(&server)).request(request)))
            .0
            .unwrap();
    }
    server.requests().into_iter().map(|r| r.body).collect()
}

#[test]
fn a_stored_image_is_sent_inside_the_tool_result_after_its_text() {
    let session = fakes::TempDir::new("fiber-anthropic-request-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(false, vec![png_ref("artifacts/i_1.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"][1]["content"][0],
        json!({
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            "content": [
                {"type": "text", "text": "Image: 2x1 image/png.\n"},
                {"type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "YWJjZA=="}},
            ],
            "cache_control": {"type": "ephemeral"},
        })
    );
}

#[test]
fn a_failed_result_with_an_image_still_sets_is_error() {
    let session = fakes::TempDir::new("fiber-anthropic-request-image");
    std::fs::write(session.path().join("i.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(true, vec![png_ref("i.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let block = sent_body(&server, 0)["messages"][1]["content"][0].clone();
    assert_eq!(block["is_error"], true);
    assert_eq!(block["content"][1]["type"], "image");
}

#[test]
fn an_image_that_cannot_be_read_is_named_in_the_text_and_not_sent() {
    let session = fakes::TempDir::new("fiber-anthropic-request-image");
    let request = ModelRequest {
        conversation: image_conversation(false, vec![png_ref("artifacts/gone.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"][1]["content"][0]["content"],
        "Image: 2x1 image/png.\n[Image artifacts/gone.png could not be read.]"
    );
}

#[test]
fn a_resume_sends_the_same_bytes() {
    let session = fakes::TempDir::new("fiber-anthropic-request-image");
    std::fs::write(
        session.path().join("i.png"),
        b"\x89PNG bytes that are not a png",
    )
    .unwrap();
    let request = ModelRequest {
        conversation: image_conversation(false, vec![png_ref("i.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let bodies = bodies_of(&request, 2);
    assert_eq!(bodies[0], bodies[1]);
}

#[test]
fn a_rewound_child_sends_its_parent_image_as_the_same_bytes() {
    let root = fakes::TempDir::new("fiber-anthropic-rewound-image");
    let sessions = root.path().join("sessions");
    let parent = sessions.join("s_parent");
    let child = sessions.join("s_child");
    std::fs::create_dir_all(parent.join("artifacts")).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(parent.join("artifacts/i_1.png"), b"abcd").unwrap();
    let absolute = parent.join("artifacts/i_1.png").display().to_string();
    let parent_request = ModelRequest {
        conversation: image_conversation(false, vec![png_ref("artifacts/i_1.png")]),
        session_dir: parent,
        ..request()
    };
    let child_request = ModelRequest {
        conversation: image_conversation(false, vec![png_ref(&absolute)]),
        session_dir: child,
        ..request()
    };
    let parent_body = bodies_of(&parent_request, 1).pop().unwrap();
    let child_body = bodies_of(&child_request, 1).pop().unwrap();
    assert_eq!(parent_body, child_body);
}

/// A conversation whose one tool result carries a PDF with two pages.
fn pdf_conversation(pdf: contract::provider::PdfRef) -> Vec<Input> {
    vec![
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.pdf"}),
                provider_id: Some(ProviderCallId("toolu_1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "anthropic/claude-sonnet-5-5".into(),
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

fn pdf_ref(
    path: &str,
    pages: Option<Vec<contract::provider::ImageRef>>,
) -> contract::provider::PdfRef {
    contract::provider::PdfRef {
        path: path.into(),
        page_count: 2,
        pages,
    }
}

#[test]
fn a_stored_pdf_is_sent_inside_the_tool_result_as_a_document() {
    let session = fakes::TempDir::new("fiber-anthropic-request-pdf");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/p_1.pdf"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: pdf_conversation(pdf_ref("artifacts/p_1.pdf", None)),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"][1]["content"][0],
        json!({
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            "content": [
                {"type": "text", "text": "PDF: 2 pages.\n"},
                {"type": "document", "source": {
                    "type": "base64", "media_type": "application/pdf", "data": "YWJjZA=="}},
            ],
            "cache_control": {"type": "ephemeral"},
        })
    );
}

#[test]
fn a_rewound_child_sends_its_parent_pdf_as_the_same_bytes() {
    let root = fakes::TempDir::new("fiber-anthropic-rewound-pdf");
    let sessions = root.path().join("sessions");
    let parent = sessions.join("s_parent");
    let child = sessions.join("s_child");
    std::fs::create_dir_all(parent.join("artifacts")).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(parent.join("artifacts/p_1.pdf"), b"abcd").unwrap();
    std::fs::write(parent.join("artifacts/i_1.png"), b"abcd").unwrap();
    std::fs::write(parent.join("artifacts/i_2.png"), b"abcd").unwrap();
    let absolute = |name: &str| parent.join(name).display().to_string();
    let parent_request = ModelRequest {
        conversation: pdf_conversation(pdf_ref(
            "artifacts/p_1.pdf",
            Some(vec![
                png_ref("artifacts/i_1.png"),
                png_ref("artifacts/i_2.png"),
            ]),
        )),
        session_dir: parent.clone(),
        ..request()
    };
    let child_request = ModelRequest {
        conversation: pdf_conversation(pdf_ref(
            &absolute("artifacts/p_1.pdf"),
            Some(vec![
                png_ref(&absolute("artifacts/i_1.png")),
                png_ref(&absolute("artifacts/i_2.png")),
            ]),
        )),
        session_dir: child,
        ..request()
    };
    let parent_body = bodies_of(&parent_request, 1).pop().unwrap();
    let child_body = bodies_of(&child_request, 1).pop().unwrap();
    assert_eq!(parent_body, child_body);
}

#[test]
fn a_result_without_an_image_keeps_a_string_content() {
    let request = ModelRequest {
        conversation: image_conversation(false, Vec::new()),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"][1]["content"][0]["content"],
        "Image: 2x1 image/png.\n"
    );
}

#[test]
fn a_text_only_model_gets_no_image_part_and_the_result_says_so() {
    let session = fakes::TempDir::new("fiber-anthropic-request-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(false, vec![png_ref("artifacts/i_1.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let endpoint = Endpoint {
        text_only: true,
        ..endpoint(&server)
    };
    run(Box::new(Messages::new(endpoint).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"][1]["content"][0]["content"],
        "Image: 2x1 image/png.\n[Image artifacts/i_1.png left out: this model does not take images.]"
    );
}

#[test]
fn web_search_requests_in_usage_become_the_reply_count() {
    // Count on message_delta.
    let (reply, _) = decoded(&stream(&[
        started(),
        text_block(0, "hi")[0].clone(),
        text_block(0, "hi")[1].clone(),
        stopped(0),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 9,
                "server_tool_use": {"web_search_requests": 2}}}),
        json!({"type": "message_stop"}),
    ]));
    assert_eq!(reply.unwrap().web_searches, Some(2));

    // Count only on message_start, untouched by the delta, is kept.
    let (reply, _) = decoded(&stream(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {
            "input_tokens": 3, "output_tokens": 1,
            "server_tool_use": {"web_search_requests": 3}}}}),
        text_block(0, "hi")[0].clone(),
        text_block(0, "hi")[1].clone(),
        stopped(0),
        finished("end_turn")[0].clone(),
        finished("end_turn")[1].clone(),
    ]));
    assert_eq!(reply.unwrap().web_searches, Some(3));

    // Zero and absent give None.
    let (reply, _) = decoded(&stream(&[
        started(),
        text_block(0, "hi")[0].clone(),
        text_block(0, "hi")[1].clone(),
        stopped(0),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 9,
                "server_tool_use": {"web_search_requests": 0}}}),
        json!({"type": "message_stop"}),
    ]));
    assert_eq!(reply.unwrap().web_searches, None);

    let (reply, _) = decoded(&stream(&[
        started(),
        text_block(0, "hi")[0].clone(),
        text_block(0, "hi")[1].clone(),
        stopped(0),
        finished("end_turn")[0].clone(),
        finished("end_turn")[1].clone(),
    ]));
    assert_eq!(reply.unwrap().web_searches, None);
}

fn search_call(index: u64, id: &str) -> Vec<Value> {
    vec![
        json!({"type": "content_block_start", "index": index, "content_block":
            {"type": "server_tool_use", "id": id, "name": "web_search", "input": {}}}),
        json!({"type": "content_block_delta", "index": index, "delta":
            {"type": "input_json_delta", "partial_json": "{\"query\":"}}),
        json!({"type": "content_block_delta", "index": index, "delta":
            {"type": "input_json_delta", "partial_json": "\"rust 1.90\"}"}}),
        stopped(index),
    ]
}

fn search_result(index: u64, tool_use_id: &str, content: Value) -> Vec<Value> {
    vec![
        json!({"type": "content_block_start", "index": index, "content_block":
            {"type": "web_search_tool_result", "tool_use_id": tool_use_id, "content": content}}),
        stopped(index),
    ]
}

fn two_results() -> Value {
    json!([
        {"type": "web_search_result", "url": "https://blog.rust-lang.org/",
         "title": "Rust Blog", "encrypted_content": "Eq", "page_age": null},
        {"type": "web_search_result", "url": "https://doc.rust-lang.org/",
         "title": "Docs", "encrypted_content": "Er", "page_age": "May 1, 2026"}
    ])
}

fn citation(url: &str) -> Value {
    json!({"type": "web_search_result_location", "url": url, "title": "T",
        "encrypted_index": "Eo", "cited_text": "c"})
}

fn hosted_stream(parts: Vec<Vec<Value>>) -> Vec<u8> {
    let mut events = vec![started()];
    events.extend(parts.into_iter().flatten());
    events.extend(finished("end_turn"));
    stream(&events)
}

#[test]
fn a_hosted_search_decodes_with_its_result_and_the_citing_text_around_it() {
    let mut events = search_call(0, "srvtoolu_01");
    events.extend(search_result(1, "srvtoolu_01", two_results()));
    events.push(json!({"type": "content_block_start", "index": 2,
        "content_block": {"type": "text", "text": ""}}));
    events.push(json!({"type": "content_block_delta", "index": 2,
        "delta": {"type": "text_delta", "text": "Rust 1.90 is out."}}));
    for url in ["https://blog.rust-lang.org/", "https://doc.rust-lang.org/"] {
        events.push(json!({"type": "content_block_delta", "index": 2,
            "delta": {"type": "citations_delta", "citation": citation(url)}}));
    }
    events.push(stopped(2));
    let (reply, deltas) = decoded(&hosted_stream(vec![events]));
    let reply = reply.unwrap();
    assert!(
        deltas
            .iter()
            .all(|d| !matches!(d, Delta::ToolCallArguments(_))),
        "a hosted call streams no arguments"
    );
    let [ReplyAction::Hosted(hosted), ReplyAction::Text(text)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(hosted.call.name, "web_search");
    assert_eq!(hosted.call.arguments, json!({"query": "rust 1.90"}));
    assert_eq!(
        hosted.call.provider_id,
        Some(ProviderCallId("srvtoolu_01".into()))
    );
    assert_eq!(
        hosted.call.provider_item,
        Some(json!({"type": "server_tool_use", "id": "srvtoolu_01",
            "name": "web_search", "input": {"query": "rust 1.90"}}))
    );
    assert_eq!(
        hosted.completed.status,
        contract::events::CallStatus::Completed
    );
    assert_eq!(
        hosted.completed.content,
        vec![contract::shapes::ContentPart::Text {
            text: "https://blog.rust-lang.org/\nhttps://doc.rust-lang.org/".into()
        }]
    );
    assert_eq!(
        hosted.completed.provider_item,
        Some(
            json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01",
            "content": two_results()})
        )
    );
    assert_eq!(text.text, "Rust 1.90 is out.");
    assert_eq!(
        text.provider_item,
        Some(
            json!({"type": "text", "text": "Rust 1.90 is out.", "citations": [
            citation("https://blog.rust-lang.org/"), citation("https://doc.rust-lang.org/")]})
        )
    );
    assert_eq!(reply.text(), "Rust 1.90 is out.");
}

#[test]
fn a_hosted_search_that_failed_completes_failed_with_the_vendors_code() {
    let mut events = search_call(0, "srvtoolu_01");
    let error = json!({"type": "web_search_tool_result_error", "error_code": "max_uses_exceeded"});
    events.extend(search_result(1, "srvtoolu_01", error.clone()));
    let (reply, _) = decoded(&hosted_stream(vec![events]));
    let reply = reply.unwrap();
    let [ReplyAction::Hosted(hosted)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    let done = &hosted.completed;
    assert_eq!(done.status, contract::events::CallStatus::Failed);
    let failure = done.error.as_ref().unwrap();
    assert_eq!(failure.code, ErrorCode::ToolError);
    assert_eq!(
        failure.message,
        "The provider's search failed: max_uses_exceeded."
    );
    assert_eq!(
        done.content,
        vec![contract::shapes::ContentPart::Text {
            text: failure.message.clone()
        }]
    );
    assert_eq!(
        done.provider_item,
        Some(
            json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01",
            "content": error})
        )
    );
}

#[test]
fn a_hosted_call_with_no_result_and_a_result_with_no_call_are_left_out() {
    // No result by `message_stop`: no search ran.
    let (reply, _) = decoded(&hosted_stream(vec![search_call(0, "srvtoolu_01")]));
    assert!(reply.unwrap().actions.is_empty());
    // A result whose call is unknown.
    let (reply, _) = decoded(&hosted_stream(vec![search_result(
        0,
        "srvtoolu_99",
        two_results(),
    )]));
    assert!(reply.unwrap().actions.is_empty());
    // A result pairs with its own call by id, not by position.
    let mut events = search_call(0, "srvtoolu_01");
    events.extend(search_call(1, "srvtoolu_02"));
    events.extend(search_result(2, "srvtoolu_02", two_results()));
    let (reply, _) = decoded(&hosted_stream(vec![events]));
    let reply = reply.unwrap();
    let [ReplyAction::Hosted(hosted)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(
        hosted.call.provider_id,
        Some(ProviderCallId("srvtoolu_02".into()))
    );
}

#[test]
fn a_text_block_without_citations_keeps_no_provider_item() {
    let (reply, _) = decoded(&hosted_stream(vec![
        text_block(0, "plain").to_vec(),
        vec![stopped(0)],
    ]));
    let reply = reply.unwrap();
    assert!(matches!(
        reply.actions.as_slice(),
        [ReplyAction::Text(part)] if part.text == "plain" && part.provider_item.is_none()
    ));
}

fn hosted_tool() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".into(),
        description: String::new(),
        input_schema: json!({}),
        deferred: false,
        hosted: Some("web_search_20250305".into()),
    }
}

#[test]
fn a_hosted_tool_is_sent_as_its_type_and_name_and_takes_no_strict_slot() {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let mut request = request();
    request.tools = (0..20)
        .map(|i| ToolDefinition {
            name: format!("tool_{i:02}"),
            ..weather_tool()
        })
        .collect();
    request.tools.insert(0, hosted_tool());
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    let tools = sent_body(&server, 0)["tools"].as_array().unwrap().clone();
    assert_eq!(tools.len(), 21);
    let search: Vec<&Value> = tools.iter().filter(|t| t["name"] == "web_search").collect();
    assert_eq!(
        search,
        [&json!({"name": "web_search", "type": "web_search_20250305"})]
    );
    let strict = tools.iter().filter(|t| t["strict"] == json!(true)).count();
    assert_eq!(strict, 20);
}

fn hosted_conversation(model: &str) -> Vec<Input> {
    let mut conversation = request().conversation;
    conversation.extend([
        Input::Assistant {
            model: model.into(),
            text: String::new(),
            provider_item: Some(json!({"type": "server_tool_use", "id": "srvtoolu_01",
                "name": "web_search", "input": {"query": "rust 1.90"}})),
        },
        Input::Assistant {
            model: model.into(),
            text: String::new(),
            provider_item: Some(json!({"type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_01", "content": []})),
        },
        Input::Assistant {
            model: model.into(),
            text: "Rust 1.90 is out.".into(),
            provider_item: Some(json!({"type": "text", "text": "Rust 1.90 is out.",
                "citations": [{"type": "web_search_result_location", "url": "u"}]})),
        },
        Input::User {
            text: "Thanks.".into(),
            images: Vec::new(),
        },
    ]);
    conversation
}

fn sent_messages(conversation: Vec<Input>) -> Value {
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let request = ModelRequest {
        conversation,
        ..request()
    };
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    sent_body(&server, 0)["messages"].clone()
}

#[test]
fn a_hosted_pair_goes_back_unchanged_to_the_model_that_produced_it() {
    let messages = sent_messages(hosted_conversation("anthropic/claude-sonnet-5-5"));
    assert_eq!(
        messages,
        json!([
            {"role": "user", "content": "What is the weather in Paris? Use the tool."},
            {"role": "assistant", "content": [
                {"type": "server_tool_use", "id": "srvtoolu_01", "name": "web_search",
                 "input": {"query": "rust 1.90"}},
                {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01", "content": []},
                {"type": "text", "text": "Rust 1.90 is out.",
                 "citations": [{"type": "web_search_result_location", "url": "u"}]}]},
            {"role": "user", "content": [
                {"type": "text", "text": "Thanks.", "cache_control": {"type": "ephemeral"}}]},
        ])
    );
}

#[test]
fn a_hosted_pair_is_left_out_for_another_model_and_its_citing_text_goes_as_plain_text() {
    let messages = sent_messages(hosted_conversation("openai/gpt-6-luna"));
    assert_eq!(
        messages,
        json!([
            {"role": "user", "content": "What is the weather in Paris? Use the tool."},
            {"role": "assistant", "content": "Rust 1.90 is out."},
            {"role": "user", "content": [
                {"type": "text", "text": "Thanks.", "cache_control": {"type": "ephemeral"}}]},
        ])
    );
}

#[test]
fn a_cache_marker_on_a_hosted_block_is_the_only_key_added() {
    let mut conversation = hosted_conversation("anthropic/claude-sonnet-5-5");
    conversation.truncate(3);
    let messages = sent_messages(conversation);
    assert_eq!(
        messages[1]["content"][1],
        json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01", "content": [],
            "cache_control": {"type": "ephemeral"}})
    );
    assert_eq!(
        messages[1]["content"][0],
        json!({"type": "server_tool_use", "id": "srvtoolu_01", "name": "web_search",
            "input": {"query": "rust 1.90"}})
    );
}

/// One live hosted search on `claude-sonnet-5-5`, recorded with the `record`
/// jig from the request Fiber builds for a model whose `web_search` is
/// `web_search_20250305`.
fn web_search_recording() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recordings/anthropic-web-search.sse"),
    )
    .unwrap()
}

#[test]
fn the_recorded_hosted_search_runs_through_the_seam_and_replays_its_blocks() {
    let bytes = web_search_recording();
    let server = ProviderServer::start([Response::stream(bytes), completed_reply()]).unwrap();
    let provider: Box<dyn Provider> = Box::new(Messages::new(endpoint(&server)));
    let (reply, _) = run(provider.call(&request()));
    let reply = reply.unwrap();
    assert_eq!(reply.finish, Finish::Completed);
    assert_eq!(reply.web_searches, Some(1));

    let hosted: Vec<_> = reply
        .actions
        .iter()
        .filter_map(|action| match action {
            ReplyAction::Hosted(hosted) => Some(hosted),
            ReplyAction::Text(_) | ReplyAction::Reasoning(_) | ReplyAction::ToolCall(_) => None,
        })
        .collect();
    let [hosted] = hosted.as_slice() else {
        panic!("expected one hosted call, got {:?}", reply.actions);
    };
    let call_item = hosted.call.provider_item.as_ref().unwrap();
    assert_eq!(hosted.call.name, "web_search");
    assert_eq!(call_item["type"], "server_tool_use");
    assert_eq!(
        hosted.call.provider_id.as_ref().unwrap().0,
        call_item["id"].as_str().unwrap()
    );
    assert!(hosted.call.arguments["query"].is_string());
    assert_eq!(call_item["input"], hosted.call.arguments);

    let result_item = hosted.completed.provider_item.as_ref().unwrap();
    assert_eq!(result_item["type"], "web_search_tool_result");
    assert_eq!(result_item["tool_use_id"], call_item["id"]);
    let urls: Vec<&str> = result_item["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["url"].as_str().unwrap())
        .collect();
    assert!(!urls.is_empty());
    assert_eq!(
        hosted.completed.status,
        contract::events::CallStatus::Completed
    );
    assert_eq!(
        hosted.completed.content,
        vec![contract::shapes::ContentPart::Text {
            text: urls.join("\n")
        }]
    );

    let cited: Vec<&Value> = reply
        .actions
        .iter()
        .filter_map(|action| match action {
            ReplyAction::Text(part) => part.provider_item.as_ref(),
            ReplyAction::Reasoning(_) | ReplyAction::ToolCall(_) | ReplyAction::Hosted(_) => None,
        })
        .collect();
    assert!(!cited.is_empty(), "the answer cites the search");
    assert!(
        cited
            .iter()
            .all(|item| item["citations"][0]["encrypted_index"].is_string())
    );

    // The next request to the same model sends both raw blocks back
    // unchanged, in order, in the assistant message.
    let reference = "anthropic/claude-sonnet-5-5".to_owned();
    let mut next = request();
    next.conversation.extend([
        Input::Assistant {
            model: reference.clone(),
            text: String::new(),
            provider_item: Some(call_item.clone()),
        },
        Input::Assistant {
            model: reference,
            text: String::new(),
            provider_item: Some(result_item.clone()),
        },
        Input::User {
            text: "Thanks.".into(),
            images: Vec::new(),
        },
    ]);
    let (second, _) = run(provider.call(&next));
    second.unwrap();
    let sent = sent_body(&server, 1);
    let blocks = sent["messages"][1]["content"].as_array().unwrap();
    assert_eq!(blocks, &vec![call_item.clone(), result_item.clone()]);
}

#[test]
fn thinking_levels_map_to_adaptive_thinking_and_effort() {
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
        run(Box::new(Messages::new(endpoint(&server)).request(&req)))
            .0
            .unwrap();
    }
    let none = sent_body(&server, 0);
    assert_eq!(none.get("thinking"), None);
    assert_eq!(none.get("output_config"), None);
    let off = sent_body(&server, 1);
    assert_eq!(off.get("thinking"), None);
    assert_eq!(off.get("output_config"), None);
    let low = sent_body(&server, 2);
    assert_eq!(low["thinking"], json!({"type": "adaptive"}));
    assert_eq!(low["output_config"], json!({"effort": "low"}));
    let xhigh = sent_body(&server, 3);
    assert_eq!(xhigh["thinking"], json!({"type": "adaptive"}));
    assert_eq!(xhigh["output_config"], json!({"effort": "xhigh"}));
}

#[test]
fn the_cache_key_goes_in_the_declared_header_and_nowhere_else_without_one() {
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let endpoint = endpoint(&server);
    run(Box::new(
        Messages::new(endpoint.clone()).request(&request()),
    ))
    .0
    .unwrap();
    run(Box::new(
        Messages::new(endpoint)
            .cache_key_header("x-opencode-session")
            .request(&request()),
    ))
    .0
    .unwrap();
    let sent = server.requests();
    assert_eq!(sent[0].header("x-opencode-session"), None);
    assert_eq!(sent[1].header("x-opencode-session"), Some("session_1"));
}

/// A conversation whose one user message carries `images`.
fn user_conversation(text: &str, images: Vec<contract::provider::ImageRef>) -> Vec<Input> {
    vec![Input::User {
        text: text.into(),
        images,
    }]
}

#[test]
fn a_users_image_is_sent_as_image_blocks_after_the_text() {
    let session = fakes::TempDir::new("fiber-anthropic-user-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: user_conversation("look", vec![png_ref("artifacts/i_1.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([{"role": "user", "content": [
            {"type": "text", "text": "look"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "YWJjZA=="},
             "cache_control": {"type": "ephemeral"}},
        ]}])
    );
}

#[test]
fn a_users_empty_text_sends_no_text_block() {
    let session = fakes::TempDir::new("fiber-anthropic-user-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: user_conversation("", vec![png_ref("artifacts/i_1.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([{"role": "user", "content": [
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "YWJjZA=="},
             "cache_control": {"type": "ephemeral"}},
        ]}])
    );
}

#[test]
fn a_users_image_is_left_out_for_a_text_only_model() {
    let session = fakes::TempDir::new("fiber-anthropic-user-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: user_conversation("look", vec![png_ref("artifacts/i_1.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let endpoint = Endpoint {
        text_only: true,
        ..endpoint(&server)
    };
    run(Box::new(Messages::new(endpoint).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([{"role": "user", "content": [
            {"type": "text",
             "text": "look\n[Image artifacts/i_1.png left out: this model does not take images.]",
             "cache_control": {"type": "ephemeral"}},
        ]}])
    );
}

#[test]
fn the_previous_end_marker_lands_on_a_multi_block_users_last_block() {
    let session = fakes::TempDir::new("fiber-anthropic-user-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: vec![
            Input::User {
                text: "look".into(),
                images: vec![png_ref("artifacts/i_1.png")],
            },
            Input::User {
                text: "again".into(),
                images: Vec::new(),
            },
        ],
        previous_end: Some(1),
        sent_tools: None,
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    // Both inputs share one user message; the marker for the boundary after
    // the first input lands on its last block, the image, and the last
    // block overall carries the other marker.
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([{"role": "user", "content": [
            {"type": "text", "text": "look"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "YWJjZA=="},
             "cache_control": {"type": "ephemeral"}},
            {"type": "text", "text": "again", "cache_control": {"type": "ephemeral"}},
        ]}])
    );
}

#[test]
fn a_reply_carries_the_size_of_the_body_it_sent() {
    let session = fakes::TempDir::new("fiber-anthropic-request-size");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    let request = ModelRequest {
        conversation: image_conversation(false, vec![png_ref("artifacts/i_1.png")]),
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let reply = run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        reply.input_size,
        InputSize {
            bytes: u64::try_from(server.requests()[0].body.len()).unwrap(),
            media: true,
        }
    );
    let endpoint = Endpoint {
        text_only: true,
        ..endpoint(&server)
    };
    let reply = run(Box::new(Messages::new(endpoint).request(&request)))
        .0
        .unwrap();
    assert_eq!(
        reply.input_size,
        InputSize {
            bytes: u64::try_from(server.requests()[1].body.len()).unwrap(),
            media: false,
        }
    );
}

#[test]
fn sent_tools_are_sent_verbatim_in_order() {
    // A rewound session's first request carries its parent's logged build,
    // not what its own tools would wire (`docs/events.md`, "Rewind").
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let sent = vec![
        json!({"name": "b_tool", "description": "Second.",
               "input_schema": {"type": "object"}, "strict": true}),
        json!({"name": "a_tool", "description": "First.",
               "input_schema": {"type": "object"}, "strict": false}),
    ];
    let mut request = request();
    request.sent_tools = Some(
        sent.iter()
            .map(|tool| tool.as_object().unwrap().clone())
            .collect(),
    );
    run(Box::new(Messages::new(endpoint(&server)).request(&request)))
        .0
        .unwrap();
    assert_eq!(sent_body(&server, 0)["tools"], Value::Array(sent));
}
