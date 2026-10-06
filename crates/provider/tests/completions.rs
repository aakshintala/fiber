//! `openai-completions` through the provider crate's public API: the probe
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

use std::collections::BTreeMap;
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
use fakes::{ProviderServer, Response};
use provider::openai_completions::{Completions, decode};
use provider::{Compat, Endpoint};
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
    let reply = decode(bytes, &CacheLifetime::FiveMinutes, &mut |d| deltas.push(d));
    (reply, deltas)
}

fn endpoint(server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: "openrouter".into(),
        model: "z-ai/glm-5.3-flash".into(),
        base_url: format!("{}/v1", server.url()),
        key: Some("sk-secret".into()),
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
        system_prompt: "You are terse.".into(),
        tools: vec![weather_tool(), loose_tool()],
        effort: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: "s_root".into(),
        conversation: vec![Input::User {
            text: "What is the weather in Paris? Use the tool.".into(),
        }],
        previous_end: None,
        max_output_tokens: None,
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

/// A stream of `data:` chunks, one per JSON value, then `[DONE]`.
fn stream(chunks: &[Value]) -> Vec<u8> {
    let mut out: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    out.push_str("data: [DONE]\n\n");
    out.into_bytes()
}

fn chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({"id": "gen-1", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]})
}

fn usage(prompt: u64, cached: u64, output: u64) -> Value {
    json!({"id": "gen-1", "choices": [], "usage": {"prompt_tokens": prompt,
        "completion_tokens": output, "prompt_tokens_details": {"cached_tokens": cached}}})
}

fn completed_reply() -> Response {
    Response::stream(stream(&[
        chunk(json!({"role": "assistant", "content": "hi"}), None),
        chunk(json!({}), Some("stop")),
        usage(10, 4, 3),
    ]))
}

fn send(endpoint: Endpoint, request: &ModelRequest) {
    run(Box::new(Completions::new(endpoint).request(request)))
        .0
        .unwrap();
}

fn error_code(result: Result<Reply, provider::Error>) -> (ErrorCode, String) {
    let error = result.unwrap_err();
    (error.code(), error.to_string())
}

/// What a recording holds: its text, its tool calls as (id, name, raw
/// arguments), the reasoning text, its last finish reason and its usage.
#[derive(Default)]
struct Expected {
    text: String,
    calls: BTreeMap<u64, (String, String, String)>,
    reasoning: String,
    finish: String,
    usage: Value,
}

fn expected(bytes: &[u8]) -> Expected {
    let mut want = Expected::default();
    for line in String::from_utf8_lossy(bytes).lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if chunk["usage"].is_object() {
            want.usage = chunk["usage"].clone();
        }
        let Some(choice) = chunk["choices"].get(0) else {
            continue;
        };
        let delta = &choice["delta"];
        want.text.push_str(delta["content"].as_str().unwrap_or(""));
        want.reasoning
            .push_str(delta["reasoning"].as_str().unwrap_or(""));
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let slot = want
                .calls
                .entry(call["index"].as_u64().unwrap())
                .or_default();
            slot.0.push_str(call["id"].as_str().unwrap_or(""));
            slot.1
                .push_str(call["function"]["name"].as_str().unwrap_or(""));
            slot.2
                .push_str(call["function"]["arguments"].as_str().unwrap_or(""));
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            want.finish = reason.to_owned();
        }
    }
    want
}

/// `usage` as Fiber's tokens: OpenAI and OpenRouter count cache reads and
/// writes inside `prompt_tokens` (`docs/model-routing.md`, "openai-completions
/// facts"), and a write is the request's cache lifetime.
fn tokens(usage: &Value, lifetime: &str) -> Tokens {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let read = count("/prompt_tokens_details/cached_tokens");
    let write = count("/prompt_tokens_details/cache_write_tokens");
    Tokens {
        input: count("/prompt_tokens") - read - write,
        cache_read: read,
        cache_write: if write > 0 {
            BTreeMap::from([(lifetime.to_owned(), write)])
        } else {
            BTreeMap::new()
        },
        output: count("/completion_tokens"),
    }
}

#[test]
fn every_probe_recording_decodes_into_the_actions_and_usage_it_holds() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(research("openai-completions-probe/raw"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();

    let (mut streams, mut calls_seen, mut reasoned, mut statuses) = (0, 0, 0, 0);
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
            let want = expected(&bytes);
            let (reply, deltas) = decoded(&bytes);
            let reply = reply.unwrap_or_else(|e| panic!("{label}: {e}"));
            let finish = match want.finish.as_str() {
                "length" => Finish::OutputLimit,
                _ => Finish::Completed,
            };
            assert_eq!(reply.finish, finish, "{label}");
            assert_eq!(reply.text(), want.text, "{label}");
            assert_eq!(reply.tokens, tokens(&want.usage, "5m"), "{label}");
            assert!(!reply.generation_id.0.is_empty(), "{label}");

            // Text deltas add up to the reply's text.
            let text: String = deltas
                .iter()
                .filter_map(|d| match d {
                    Delta::Text(t) => Some(t.text.as_str()),
                    Delta::Reasoning(_) | Delta::ToolCallArguments(_) => None,
                })
                .collect();
            assert_eq!(text, reply.text(), "{label}");

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
            for (n, (call, (id, name, raw))) in calls.iter().zip(want.calls.values()).enumerate() {
                calls_seen += 1;
                assert_eq!(&call.name, name, "{label}");
                assert_eq!(
                    call.provider_id,
                    Some(ProviderCallId(id.clone())),
                    "{label}"
                );
                let parsed: Value = serde_json::from_str(raw).unwrap();
                assert_eq!(call.arguments, parsed, "{label}");
                let streamed: String = deltas
                    .iter()
                    .filter_map(|d| match d {
                        Delta::ToolCallArguments(a) if a.index as usize == n => {
                            assert_eq!(a.name.as_deref(), Some(name.as_str()), "{label}");
                            Some(a.text.as_str())
                        }
                        Delta::Text(_) | Delta::Reasoning(_) | Delta::ToolCallArguments(_) => None,
                    })
                    .collect();
                assert_eq!(&streamed, raw, "{label}");
            }

            let reasoning: Vec<&ReasoningCompleted> = reply
                .actions
                .iter()
                .filter_map(|a| match a {
                    ReplyAction::Reasoning(r) => Some(r),
                    ReplyAction::ToolCall(_) | ReplyAction::Text(_) | ReplyAction::Hosted(_) => {
                        None
                    }
                })
                .collect();
            if want.reasoning.is_empty() {
                assert!(reasoning.is_empty(), "{label}");
            } else {
                reasoned += 1;
                assert_eq!(reasoning.len(), 1, "{label}");
                assert_eq!(reasoning[0].text, want.reasoning, "{label}");
            }
        }
    }
    // 29 replies (8 real streams, 21 non-streamed bodies), 2 tool calls,
    // 13 with reasoning, and 2 HTTP 400s.
    assert_eq!((streams, calls_seen, reasoned, statuses), (29, 2, 13, 2));
}

#[test]
fn cache_reads_and_writes_from_openrouter_to_anthropic_are_split_out_of_input() {
    let dir = research("openai-completions-probe/raw/openrouter-anthropic-cache");
    let mut runs = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let recorded: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let hour = path.to_string_lossy().contains("1h");
        let lifetime = if hour {
            CacheLifetime::OneHour
        } else {
            CacheLifetime::FiveMinutes
        };
        for run in recorded["runs"].as_array().unwrap() {
            let bytes = stream(&[
                chunk(json!({"content": "ok"}), Some("stop")),
                json!({"id": "gen-1", "choices": [], "usage": run["usage"]}),
            ]);
            let reply = decode(&bytes[..], &lifetime, &mut |_| {}).unwrap();
            let key = if hour { "1h" } else { "5m" };
            assert_eq!(
                reply.tokens,
                tokens(&run["usage"], key),
                "{}",
                path.display()
            );
            runs += 1;
        }
    }
    assert_eq!(runs, 10);
}

#[test]
fn a_recording_served_by_the_fake_server_runs_through_the_seam() {
    let exchange = probes::read(&research(
        "openai-completions-probe/raw/openrouter-stream-tool.json",
    ))
    .unwrap()
    .pop()
    .unwrap();
    let Recorded::Stream(bytes) = exchange.response else {
        panic!("expected a stream");
    };
    let server = ProviderServer::start([Response::stream(bytes.clone())]).unwrap();
    let provider: Box<dyn Provider> = Box::new(Completions::new(endpoint(&server)));
    let (reply, deltas) = run(provider.call(&request()));
    let (want, want_deltas) = decoded(&bytes);
    assert_eq!(reply.unwrap(), want.unwrap());
    assert_eq!(deltas, want_deltas);

    let sent = &server.requests()[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/v1/chat/completions");
    assert_ne!(sent.header("authorization"), None);
    assert!(!String::from_utf8_lossy(&sent.body).contains("sk-secret"));
    assert!(sent.header("user-agent").unwrap().starts_with("fiber/"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
}

#[test]
fn two_requests_built_from_the_same_inputs_are_the_same_bytes() {
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    let mut reordered = request();
    reordered.tools.reverse();
    for request in [request(), request(), reordered] {
        send(endpoint(&server), &request);
    }
    let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(bodies[0], bodies[2], "tools are sorted by name");
    let body = sent_body(&server, 0);
    assert_eq!(
        body,
        json!({
            "model": "z-ai/glm-5.3-flash",
            "messages": [
                {"role": "system", "content": "You are terse."},
                {"role": "user", "content": "What is the weather in Paris? Use the tool."},
            ],
            "tools": [
                {"type": "function", "function": {"name": "f",
                    "description": "Takes a and maybe b.",
                    "parameters": loose_tool().input_schema, "strict": false}},
                {"type": "function", "function": {"name": "get_weather",
                    "description": "Weather for a city.",
                    "parameters": weather_tool().input_schema, "strict": true}},
            ],
            "tool_choice": "auto",
            "prompt_cache_key": "s_root",
            "stream": true,
            "stream_options": {"include_usage": true},
        })
    );
}

#[test]
fn tool_choice_is_left_out_without_tools_and_names_a_tool_otherwise() {
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    let mut bare = request();
    bare.tools.clear();
    let mut named = request();
    named.tool_choice = "get_weather".into();
    let mut required = request();
    required.tool_choice = "required".into();
    for request in [bare, named, required] {
        send(endpoint(&server), &request);
    }
    let bare = sent_body(&server, 0);
    assert_eq!((bare.get("tools"), bare.get("tool_choice")), (None, None));
    assert_eq!(
        sent_body(&server, 1)["tool_choice"],
        json!({"type": "function", "function": {"name": "get_weather"}})
    );
    assert_eq!(sent_body(&server, 2)["tool_choice"], "required");
}

#[test]
fn compat_flags_and_extra_fields_come_from_model_data_only() {
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    let mut thinking = request();
    thinking.effort = Some("medium".into());
    // The same base URL with and without the flags: only the data decides.
    send(endpoint(&server), &thinking);
    let declared = Endpoint {
        compat: Compat {
            store: Some(false),
            max_tokens: true,
            reasoning_object: true,
            ..Compat::default()
        },
        max_output_tokens: Some(4096),
        extra_body: json!({"provider": {"sort": "price"}})
            .as_object()
            .unwrap()
            .clone(),
        ..endpoint(&server)
    };
    send(declared, &thinking);
    let limited = Endpoint {
        max_output_tokens: Some(4096),
        ..endpoint(&server)
    };
    send(limited, &thinking);

    let undeclared = sent_body(&server, 0);
    assert_eq!(undeclared["reasoning_effort"], "medium");
    for key in [
        "store",
        "provider",
        "reasoning",
        "max_tokens",
        "max_completion_tokens",
    ] {
        assert_eq!(undeclared.get(key), None, "{key}");
    }
    let body = sent_body(&server, 1);
    assert_eq!(body["store"], false);
    assert_eq!(body["provider"], json!({"sort": "price"}));
    assert_eq!(body["reasoning"], json!({"effort": "medium"}));
    assert_eq!(body.get("reasoning_effort"), None);
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body.get("max_completion_tokens"), None);
    assert_eq!(sent_body(&server, 2)["max_completion_tokens"], 4096);
}

#[test]
fn the_output_limit_never_exceeds_the_models_limit() {
    let server =
        ProviderServer::start([completed_reply(), completed_reply(), completed_reply()]).unwrap();
    for extra in [
        json!({}),
        json!({"max_completion_tokens": 1024}),
        json!({"max_completion_tokens": 200_000}),
    ] {
        let limited = Endpoint {
            max_output_tokens: Some(64_000),
            extra_body: extra.as_object().unwrap().clone(),
            ..endpoint(&server)
        };
        send(limited, &request());
    }
    let sent: Vec<Value> = (0..3)
        .map(|n| sent_body(&server, n)["max_completion_tokens"].clone())
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
    send(limited.clone(), &low);
    send(limited, &high);
    assert_eq!(sent_body(&server, 0)["max_completion_tokens"], 1);
    assert_eq!(sent_body(&server, 1)["max_completion_tokens"], 4096);
}

/// Four turns: user, assistant tool call, tool result, user.
fn four_turn_conversation() -> Vec<Input> {
    vec![
        Input::User {
            text: "What is the weather in Paris?".into(),
        },
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
            model: "openrouter/z-ai/glm-5.3-flash".into(),
        },
        Input::ToolResult {
            action_id: ActionId("a_1".into()),
            text: "18 C, clear".into(),
            is_error: false,
            images: Vec::new(),
        },
        Input::User {
            text: "And Rome?".into(),
        },
    ]
}

fn markers() -> Endpoint {
    Endpoint {
        compat: Compat {
            anthropic: true,
            ..Compat::default()
        },
        direct: true,
        ..Endpoint::default()
    }
}

#[test]
fn declared_cache_markers_go_on_the_system_prompt_the_previous_end_and_the_new_end() {
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let request = ModelRequest {
        conversation: four_turn_conversation(),
        previous_end: Some(3),
        cache_lifetime: CacheLifetime::OneHour,
        ..request()
    };
    send(
        Endpoint {
            base_url: endpoint(&server).base_url,
            ..markers()
        },
        &request,
    );
    send(endpoint(&server), &request);
    let hour = json!({"type": "ephemeral", "ttl": "1h"});
    let part = |text: &str| json!([{"type": "text", "text": text, "cache_control": hour}]);
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([
            {"role": "system", "content": part("You are terse.")},
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "tool_calls": [{"id": "call_1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": part("18 C, clear")},
            {"role": "user", "content": part("And Rome?")},
        ])
    );
    let unmarked = sent_body(&server, 1);
    assert!(!unmarked.to_string().contains("cache_control"));
    assert_eq!(unmarked["messages"][3]["content"], "18 C, clear");
}

#[test]
fn reasoning_goes_back_unchanged_only_to_the_model_reference_that_produced_it() {
    let exchange = probes::read(&research(
        "openai-completions-probe/raw/openrouter-stream-reasoning.json",
    ))
    .unwrap()
    .pop()
    .unwrap();
    let Recorded::Stream(bytes) = exchange.response else {
        panic!("expected a stream");
    };
    let (reply, deltas) = decoded(&bytes);
    let reply = reply.unwrap();
    assert_eq!(
        deltas[..2],
        [
            Delta::Reasoning(TextDelta { text: "39".into() }),
            Delta::Reasoning(TextDelta { text: "1".into() }),
        ]
    );
    let ReplyAction::Reasoning(reasoning) = &reply.actions[0] else {
        panic!("{:?}", reply.actions);
    };
    // The streamed entries, merged by index into one.
    let item = json!({"reasoning_details": [
        {"type": "reasoning.text", "text": "391", "format": "unknown", "index": 0}]});
    assert_eq!(
        reasoning,
        &ReasoningCompleted {
            text: "391".into(),
            provider_item: Some(item.clone()),
        }
    );

    let mut conversation = request().conversation;
    conversation.extend([
        Input::Reasoning {
            model: "openai/gpt-6-luna".into(),
            text: "another model's thoughts".into(),
            provider_item: Some(json!({"reasoning_content": "other"})),
        },
        Input::Reasoning {
            model: "openrouter/z-ai/glm-5.3-flash".into(),
            text: "391".into(),
            provider_item: Some(item),
        },
        Input::Assistant {
            model: "openrouter/z-ai/glm-5.3-flash".into(),
            text: "Checking.".into(),
            provider_item: None,
        },
        Input::ToolCall {
            action_id: ActionId("a_1".into()),
            call: ToolCallRequested {
                name: "get_weather".into(),
                arguments: json!({"city": "Paris"}),
                provider_id: None,
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "openrouter/z-ai/glm-5.3-flash".into(),
        },
        Input::ToolResult {
            action_id: ActionId("a_1".into()),
            text: "18 C, clear".into(),
            is_error: false,
            images: Vec::new(),
        },
        Input::Assistant {
            model: "openrouter/z-ai/glm-5.3-flash".into(),
            text: String::new(),
            provider_item: None,
        },
    ]);
    let server = ProviderServer::start([completed_reply()]).unwrap();
    send(
        endpoint(&server),
        &ModelRequest {
            conversation,
            ..request()
        },
    );
    let body = sent_body(&server, 0);
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "You are terse."},
            {"role": "user", "content": "What is the weather in Paris? Use the tool."},
            {"role": "assistant", "content": "Checking.",
             "reasoning_details": [{"type": "reasoning.text", "text": "391",
                "format": "unknown", "index": 0}],
             "tool_calls": [{"id": "a_1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
            {"role": "tool", "tool_call_id": "a_1", "content": "18 C, clear"},
        ])
    );
    assert!(!body.to_string().contains("another model"));
}

#[test]
fn each_finish_reason_maps_as_the_docs_say_and_an_unknown_one_fails() {
    let end = |reason: &str| decoded(&stream(&[chunk(json!({"content": "a"}), Some(reason))])).0;
    for reason in ["stop", "tool_calls", "function_call"] {
        assert_eq!(end(reason).unwrap().finish, Finish::Completed, "{reason}");
    }
    assert_eq!(end("length").unwrap().finish, Finish::OutputLimit);
    assert_eq!(error_code(end("content_filter")).0, ErrorCode::Refused);
    let (code, message) = error_code(end("network_error"));
    assert_eq!(code, ErrorCode::UnknownStopReason);
    assert!(message.contains("network_error"), "{message}");
    // OpenRouter's `error` with no error body.
    assert_eq!(error_code(end("error")).0, ErrorCode::StreamIncomplete);
}

#[test]
fn a_stream_cut_short_fails_the_call() {
    let no_done = format!("data: {}\n\n", chunk(json!({"content": "a"}), Some("stop")));
    let (code, message) = error_code(decoded(no_done.as_bytes()).0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
    assert!(message.contains("[DONE]"), "{message}");
    let (code, _) = error_code(decoded(&stream(&[chunk(json!({"content": "a"}), None)])).0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
    let (code, _) = error_code(decoded(b"data: {not json}\n\n").0);
    assert_eq!(code, ErrorCode::StreamIncomplete);
}

#[test]
fn a_policy_refusal_fails_the_call_as_refused() {
    let (code, message) = error_code(
        decoded(&stream(&[
            chunk(json!({"refusal": "I can't help"}), None),
            chunk(json!({"refusal": " with that."}), Some("stop")),
        ]))
        .0,
    );
    assert_eq!(code, ErrorCode::Refused);
    assert!(message.contains("I can't help with that."), "{message}");
}

#[test]
fn an_error_chunk_mid_stream_fails_the_call_and_drops_what_it_streamed() {
    // research/provider-errors/raw/or-completions-luna.overflow-dense-stream.json
    let overflow = json!({"id": "gen-1", "object": "chat.completion.chunk", "choices": [],
        "error": {"code": 400, "message": "Your input exceeds the context window of this model. Please adjust your input and try again.",
        "metadata": {"error_type": "invalid_request", "provider_code": "context_length_exceeded"}}});
    let mut bytes = b": OPENROUTER PROCESSING\n\n".to_vec();
    bytes.extend(stream(&[
        chunk(json!({"content": "Checking"}), None),
        overflow,
    ]));
    let server = ProviderServer::start([Response::stream(bytes)]).unwrap();
    let (result, deltas) = run(Box::new(
        Completions::new(endpoint(&server)).request(&request()),
    ));
    let Err(CallError::Failed { failure, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(failure.code, ErrorCode::ContextOverflow);
    assert_eq!(failure.provider.unwrap().status, 200);
    assert_eq!(
        deltas,
        [Delta::Text(TextDelta {
            text: "Checking".into()
        })]
    );
    let failed = |code: Value| {
        let error = json!({"choices": [], "error": {"code": code, "message": "m"}});
        error_code(decoded(&stream(&[error])).0).0
    };
    assert_eq!(
        failed(json!("server_error")),
        ErrorCode::ProviderUnavailable
    );
    assert_eq!(failed(json!(502)), ErrorCode::StreamIncomplete);
}

#[test]
fn a_status_other_than_2xx_fails_with_its_code_and_the_providers_words() {
    let recorded = |name: &str| -> String {
        let path = research(&format!("provider-errors/raw/{name}.json"));
        let wrapper: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        wrapper["body"].as_str().unwrap().to_owned()
    };
    let server = ProviderServer::start([
        // OpenRouter's own size check: an input too long, then a `max_tokens`
        // too large, which its numbers tell apart (`docs/errors.md`,
        // "Recognising a context overflow").
        Response::status(400, recorded("or-completions-luna.overflow")),
        Response::status(400, recorded("or-completions-luna.max-tokens-huge")),
        Response::status(
            400,
            json!({"error": {"code": "context_length_exceeded", "message": "Too long."}})
                .to_string(),
        ),
        Response::status(429, "{}").header("retry-after", "7"),
    ])
    .unwrap();
    let completions = Completions::new(endpoint(&server));
    let failures: Vec<_> = (0..4)
        .map(|_| match run(Box::new(completions.request(&request()))).0 {
            Err(CallError::Failed { failure, .. }) => failure,
            other => panic!("{other:?}"),
        })
        .collect();
    let codes: Vec<ErrorCode> = failures.iter().map(|f| f.code.clone()).collect();
    assert_eq!(
        codes,
        [
            ErrorCode::ContextOverflow,
            ErrorCode::InvalidRequest,
            ErrorCode::ContextOverflow,
            ErrorCode::RateLimited,
        ]
    );
    assert_eq!(failures[3].retry_after, Some(7.0));
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
    let call = Completions::new(endpoint).request(&request());
    call.cancel();
    assert_eq!(run(Box::new(call)).0, Err(CallError::Cancelled));
    let accepted = listener.accept().map(|_| ()).unwrap_err();
    assert_eq!(
        accepted.kind(),
        std::io::ErrorKind::WouldBlock,
        "nothing connected"
    );
}

/// A server that answers one request with response headers and one text
/// chunk, then holds the socket open until `hold` is dropped.
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
        let payload = format!("data: {}\n\n", chunk(json!({"content": "Hel"}), None));
        write!(
            socket,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
             transfer-encoding: chunked\r\n\r\n{:x}\r\n{payload}\r\n",
            payload.len()
        )
        .unwrap();
        socket.flush().unwrap();
        // Hold the socket open, sending nothing more.
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
    let call: Arc<dyn ModelCall> = Arc::from(Completions::new(endpoint).call(&request()));
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
fn a_connection_closed_before_any_response_fails_the_call() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
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
    let (result, _) = run(Completions::new(endpoint).call(&request()));
    let Err(CallError::Failed { failure, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(failure.code, ErrorCode::ConnectionFailed);
}

#[test]
fn an_assistants_calls_fold_into_one_message_and_reasoning_alone_keeps_a_content() {
    let call = |id: &str| Input::ToolCall {
        action_id: ActionId(id.into()),
        call: ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({"city": id}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        },
        model: "openrouter/z-ai/glm-5.3-flash".into(),
    };
    let result = |id: &str| Input::ToolResult {
        action_id: ActionId(id.into()),
        text: "ok".into(),
        is_error: false,
        images: Vec::new(),
    };
    let reasoning = Input::Reasoning {
        model: "openrouter/z-ai/glm-5.3-flash".into(),
        text: "hm".into(),
        provider_item: Some(json!({"reasoning_content": "hm"})),
    };
    let mut conversation = request().conversation;
    conversation.extend([call("a"), call("b"), result("a"), result("b"), reasoning]);
    let server = ProviderServer::start([completed_reply()]).unwrap();
    send(
        endpoint(&server),
        &ModelRequest {
            conversation,
            ..request()
        },
    );
    let messages = sent_body(&server, 0)["messages"].clone();
    let ids: Vec<&str> = messages[2]["tool_calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(messages[2].get("content"), None);
    assert_eq!(
        messages[5],
        json!({"role": "assistant", "content": "", "reasoning_content": "hm"})
    );
}

#[test]
fn reasoning_in_its_own_field_is_kept_under_that_field() {
    let (reply, deltas) = decoded(&stream(&[
        chunk(json!({"reasoning_content": "Think"}), None),
        chunk(
            json!({"reasoning_content": "ing.", "content": "Done."}),
            Some("stop"),
        ),
    ]));
    assert_eq!(
        reply.unwrap().actions,
        [
            ReplyAction::Reasoning(ReasoningCompleted {
                text: "Thinking.".into(),
                provider_item: Some(json!({"reasoning_content": "Thinking."})),
            }),
            ReplyAction::Text(TextCompleted {
                text: "Done.".into(),
                provider_item: None,
            }),
        ]
    );
    assert_eq!(
        deltas[0],
        Delta::Reasoning(TextDelta {
            text: "Think".into()
        })
    );
}

#[test]
fn reasoning_details_merge_by_index_and_type() {
    let details = |entries: Value| chunk(json!({"reasoning_details": entries}), None);
    let (reply, deltas) = decoded(&stream(&[
        details(json!([{"type": "reasoning.text", "index": 0, "text": "a"},
            {"type": "reasoning.encrypted", "index": 0, "data": "x"}])),
        details(json!([{"type": "reasoning.text", "index": 0, "text": "b", "signature": "sig"}])),
        details(json!([{"type": "reasoning.text", "index": 0, "signature": null}])),
        chunk(json!({}), Some("stop")),
    ]));
    let ReplyAction::Reasoning(reasoning) = &reply.unwrap().actions[0] else {
        panic!("expected reasoning");
    };
    assert_eq!(
        reasoning.provider_item,
        Some(json!({"reasoning_details": [
            {"type": "reasoning.text", "index": 0, "text": "ab", "signature": "sig"},
            {"type": "reasoning.encrypted", "index": 0, "data": "x"},
        ]}))
    );
    assert_eq!(reasoning.text, "ab");
    assert_eq!(deltas.len(), 2);
}

#[test]
fn tool_call_deltas_without_an_index_are_told_apart_by_id() {
    let calls = |calls: Value| chunk(json!({"tool_calls": calls}), None);
    let (reply, deltas) = decoded(&stream(&[
        calls(
            json!([{"id": "c1", "function": {"name": "get_weather", "arguments": "{\"city\":"}}]),
        ),
        calls(json!([{"function": {"arguments": "\"Paris\"}"}}])),
        calls(json!([{"id": "c2", "function": {"name": "f", "arguments": "{}"}}])),
        chunk(json!({}), Some("tool_calls")),
    ]));
    let actions = reply.unwrap().actions;
    assert_eq!(
        actions,
        [
            ReplyAction::ToolCall(ToolCallRequested {
                name: "get_weather".into(),
                arguments: json!({"city": "Paris"}),
                provider_id: Some(ProviderCallId("c1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            }),
            ReplyAction::ToolCall(ToolCallRequested {
                name: "f".into(),
                arguments: json!({}),
                provider_id: Some(ProviderCallId("c2".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            }),
        ]
    );
    let indices: Vec<u32> = deltas
        .iter()
        .map(|d| match d {
            Delta::ToolCallArguments(a) => a.index,
            Delta::Text(_) | Delta::Reasoning(_) => panic!("{d:?}"),
        })
        .collect();
    assert_eq!(indices, [0, 0, 1]);
}

#[test]
fn compat_flags_are_read_from_the_models_data_and_unset_when_absent() {
    let data = json!({"store": false, "max_tokens": true, "reasoning_object": "yes",
        "anthropic": true, "cache_key_field": "session_id"});
    assert_eq!(
        Compat::from_data(data.as_object().unwrap()),
        Compat {
            store: Some(false),
            max_tokens: true,
            reasoning_object: false,
            anthropic: true,
            cache_key_field: Some("session_id".into()),
        }
    );
    assert_eq!(
        Compat::from_data(&serde_json::Map::new()),
        Compat::default()
    );
}

#[test]
fn an_anthropic_model_gets_anthropics_strict_limits() {
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let mut tools: Vec<ToolDefinition> = (0..21)
        .map(|i| ToolDefinition {
            name: format!("tool_{i:02}"),
            ..weather_tool()
        })
        .collect();
    tools.push(ToolDefinition {
        name: "a_enum".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"pick": {"type": "string", "enum": [["x"]]}},
            "required": ["pick"],
            "additionalProperties": false
        }),
        ..weather_tool()
    });
    let request = ModelRequest { tools, ..request() };
    let strict = |n: usize| -> Vec<bool> {
        sent_body(&server, n)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["strict"].as_bool().unwrap())
            .collect()
    };
    send(
        Endpoint {
            base_url: endpoint(&server).base_url,
            ..markers()
        },
        &request,
    );
    send(endpoint(&server), &request);
    let anthropic = strict(0);
    assert!(!anthropic[0], "an enum with an array value");
    assert_eq!(anthropic.iter().filter(|s| **s).count(), 20);
    assert!(!anthropic[21]);
    assert!(strict(1).iter().all(|s| *s), "OpenAI's own limits only");
}

#[test]
fn reasoning_details_without_an_index_are_kept_as_they_came() {
    let details = |entries: Value| chunk(json!({"reasoning_details": entries}), None);
    let first = json!({"type": "reasoning.encrypted", "id": "r1", "data": "x"});
    let second = json!({"type": "reasoning.encrypted", "id": "r2", "data": "y"});
    let (reply, _) = decoded(&stream(&[
        details(json!([first])),
        details(json!([second])),
        details(json!([{"type": "reasoning.text", "index": 0, "id": "a", "text": "p"}])),
        details(json!([{"type": "reasoning.text", "index": 0, "id": "b", "text": "q"}])),
        // A piece without an id continues the one with its index.
        details(json!([{"type": "reasoning.text", "index": 0, "text": "r"}])),
        chunk(json!({}), Some("stop")),
    ]));
    let ReplyAction::Reasoning(reasoning) = &reply.unwrap().actions[0] else {
        panic!("expected reasoning");
    };
    assert_eq!(
        reasoning.provider_item,
        Some(json!({"reasoning_details": [first, second,
            {"type": "reasoning.text", "index": 0, "id": "a", "text": "p"},
            {"type": "reasoning.text", "index": 0, "id": "b", "text": "qr"}]}))
    );
}

#[test]
fn a_declared_cache_key_field_carries_the_cache_key() {
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    send(
        Endpoint {
            compat: Compat {
                cache_key_field: Some("session_id".into()),
                ..Compat::default()
            },
            ..endpoint(&server)
        },
        &request(),
    );
    send(endpoint(&server), &request());
    let body = sent_body(&server, 0);
    assert_eq!(body["session_id"], "s_root");
    assert_eq!(body["prompt_cache_key"], "s_root");
    assert_eq!(sent_body(&server, 1).get("session_id"), None);
}

#[test]
fn a_cache_write_counts_under_the_requests_cache_lifetime() {
    let written = || {
        Response::stream(stream(&[
            chunk(json!({"content": "hi"}), Some("stop")),
            json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 1,
                "prompt_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 8}}}),
        ]))
    };
    let server = ProviderServer::start([written(), written(), written()]).unwrap();
    let hour = ModelRequest {
        cache_lifetime: CacheLifetime::OneHour,
        ..request()
    };
    let five = ModelRequest {
        cache_lifetime: CacheLifetime::FiveMinutes,
        ..request()
    };
    let base_url = endpoint(&server).base_url;
    // Fiber's own markers, at the request's lifetime: 1 hour, then 5
    // minutes. Without markers the write counts under the request's
    // lifetime, 1 hour here.
    let marked_hour = Endpoint {
        base_url: base_url.clone(),
        ..markers()
    };
    let marked_five = Endpoint {
        base_url: base_url.clone(),
        ..markers()
    };
    let mut keys = Vec::new();
    for (endpoint, request) in [
        (marked_hour, &hour),
        (marked_five, &five),
        (endpoint(&server), &hour),
    ] {
        let reply = run(Box::new(Completions::new(endpoint).request(request)))
            .0
            .unwrap();
        keys.push(reply.tokens.cache_write.into_keys().collect::<Vec<_>>());
    }
    assert_eq!(keys, [["1h"], ["5m"], ["1h"]]);
}

#[test]
fn wire_tools_is_what_the_request_sends() {
    let tools = wire_tools::wire_tools_fixture();
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let endpoint = Endpoint {
        compat: provider::Compat {
            anthropic: true,
            ..Default::default()
        },
        ..endpoint(&server)
    };
    let completions = Completions::new(endpoint.clone());
    let wired: Vec<Value> = completions
        .wire_tools(&tools)
        .into_iter()
        .map(Value::Object)
        .collect();
    let mut request = request();
    request.tools = tools;
    send(endpoint, &request);
    let sent = sent_body(&server, 0)["tools"].clone();
    assert_eq!(Value::Array(wired), sent);
    let strict: Vec<bool> = sent
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["strict"].as_bool().unwrap())
        .collect();
    assert_eq!(strict.iter().filter(|s| **s).count(), 20);
    let by_name = |name: &str| {
        sent.as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == name)
            .unwrap()["function"]["strict"]
            .clone()
    };
    assert_eq!(by_name("a_loose"), json!(false));
    assert_eq!(by_name("z_enum"), json!(false));
}

#[test]
fn a_failed_tool_result_sends_the_same_bytes_as_a_success() {
    // Chat Completions defines no error field on a tool message
    // (`ChatCompletionRequestToolMessage`: `role, content, tool_call_id`),
    // so the flag is ignored.
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
                model: "openrouter/z-ai/glm-5.3-flash".into(),
            },
            Input::ToolResult {
                action_id: ActionId("a_1".into()),
                text: "boom".into(),
                is_error,
                images: Vec::new(),
            },
        ]
    };
    let messages = |is_error: bool| {
        let server = ProviderServer::start([completed_reply()]).unwrap();
        send(
            endpoint(&server),
            &ModelRequest {
                conversation: conversation(is_error),
                ..request()
            },
        );
        sent_body(&server, 0)["messages"].clone()
    };
    assert_eq!(
        messages(true),
        json!([
            {"role": "system", "content": "You are terse."},
            {"role": "assistant", "tool_calls": [{"id": "call_1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "boom"},
        ])
    );
    assert_eq!(messages(true), messages(false));
}

/// A call and its result, the result carrying `images`.
fn image_turn(
    action: &str,
    call_id: &str,
    text: &str,
    images: Vec<contract::provider::ImageRef>,
) -> Vec<Input> {
    vec![
        Input::ToolCall {
            action_id: ActionId(action.into()),
            call: ToolCallRequested {
                name: "get_weather".into(),
                arguments: json!({"city": "Paris"}),
                provider_id: Some(ProviderCallId(call_id.into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "openrouter/z-ai/glm-5.3-flash".into(),
        },
        Input::ToolResult {
            action_id: ActionId(action.into()),
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

fn image_session() -> fakes::TempDir {
    let session = fakes::TempDir::new("fiber-completions-request-image");
    std::fs::create_dir(session.path().join("artifacts")).unwrap();
    // "abcd" is `YWJjZA==`, "wxyz" is `d3h5eg==`.
    std::fs::write(session.path().join("artifacts/i_1.png"), b"abcd").unwrap();
    std::fs::write(session.path().join("artifacts/i_2.png"), b"wxyz").unwrap();
    session
}

fn image_url(data: &str) -> Value {
    json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{data}")}})
}

#[test]
fn a_tool_results_image_goes_in_a_user_message_after_the_tool_message() {
    let session = image_session();
    let mut conversation = vec![Input::User {
        text: "What is the weather in Paris?".into(),
    }];
    conversation.extend(image_turn(
        "a_1",
        "call_1",
        "Image: 2x1 image/png.\n",
        vec![png_ref("artifacts/i_1.png")],
    ));
    let request = ModelRequest {
        conversation,
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    let endpoint = endpoint(&server);
    send(endpoint.clone(), &request);
    send(endpoint, &request);
    let bodies: Vec<Vec<u8>> = server.requests().into_iter().map(|r| r.body).collect();
    assert_eq!(bodies[0], bodies[1], "a resume sends the same bytes");
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([
            {"role": "system", "content": "You are terse."},
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "tool_calls": [{"id": "call_1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "Image: 2x1 image/png.\n"},
            {"role": "user", "content": [image_url("YWJjZA==")]},
        ])
    );
}

#[test]
fn images_from_parallel_results_share_one_user_message_in_order() {
    let session = image_session();
    // Two parallel calls, then their results: one run of tool messages.
    let (first, second) = (
        image_turn(
            "a_1",
            "call_1",
            "Image: 2x1 image/png.\n",
            vec![png_ref("artifacts/i_1.png")],
        ),
        image_turn(
            "a_2",
            "call_2",
            "Image: 2x1 image/png.\n",
            vec![png_ref("artifacts/i_2.png")],
        ),
    );
    let conversation = vec![
        first[0].clone(),
        second[0].clone(),
        first[1].clone(),
        second[1].clone(),
    ];
    let request = ModelRequest {
        conversation,
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    send(endpoint(&server), &request);
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([
            {"role": "system", "content": "You are terse."},
            {"role": "assistant", "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}},
                {"id": "call_2", "type": "function",
                 "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "Image: 2x1 image/png.\n"},
            {"role": "tool", "tool_call_id": "call_2", "content": "Image: 2x1 image/png.\n"},
            {"role": "user", "content": [image_url("YWJjZA=="), image_url("d3h5eg==")]},
        ])
    );
}

#[test]
fn a_user_turn_after_a_tool_result_comes_after_the_image_message() {
    let session = image_session();
    let mut conversation = vec![Input::User {
        text: "What is the weather in Paris?".into(),
    }];
    conversation.extend(image_turn(
        "a_1",
        "call_1",
        "Image: 2x1 image/png.\n",
        vec![png_ref("artifacts/i_1.png")],
    ));
    conversation.push(Input::User {
        text: "And Rome?".into(),
    });
    let request = ModelRequest {
        conversation,
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    send(endpoint(&server), &request);
    let messages = sent_body(&server, 0)["messages"].clone();
    let roles: Vec<&str> = messages
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        ["system", "user", "assistant", "tool", "user", "user"]
    );
    assert_eq!(messages[3]["content"], "Image: 2x1 image/png.\n");
    assert_eq!(messages[4]["content"], json!([image_url("YWJjZA==")]));
    assert_eq!(messages[5]["content"], "And Rome?");
}

#[test]
fn a_text_only_model_sends_no_image_message_and_the_result_says_so() {
    let session = image_session();
    let mut conversation = vec![Input::User {
        text: "What is the weather in Paris?".into(),
    }];
    conversation.extend(image_turn(
        "a_1",
        "call_1",
        "Image: 2x1 image/png.\n",
        vec![png_ref("artifacts/i_1.png")],
    ));
    let request = ModelRequest {
        conversation,
        session_dir: session.path().to_path_buf(),
        ..request()
    };
    let server = ProviderServer::start([completed_reply()]).unwrap();
    let endpoint = Endpoint {
        text_only: true,
        ..endpoint(&server)
    };
    send(endpoint, &request);
    assert_eq!(
        sent_body(&server, 0)["messages"],
        json!([
            {"role": "system", "content": "You are terse."},
            {"role": "user", "content": "What is the weather in Paris?"},
            {"role": "assistant", "tool_calls": [{"id": "call_1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1",
             "content": "Image: 2x1 image/png.\n[Image artifacts/i_1.png left out: this model does not take images.]"},
        ])
    );
}

#[test]
fn the_image_message_carries_the_cache_marker_as_the_new_and_previous_end() {
    let session = image_session();
    let mut conversation = vec![Input::User {
        text: "What is the weather in Paris?".into(),
    }];
    conversation.extend(image_turn(
        "a_1",
        "call_1",
        "18 C, clear",
        vec![png_ref("artifacts/i_1.png")],
    ));
    let hour = json!({"type": "ephemeral", "ttl": "1h"});
    let marked_image = |lifetime: &Value| {
        json!([{"type": "image_url",
            "image_url": {"url": "data:image/png;base64,YWJjZA=="},
            "cache_control": lifetime}])
    };
    let send_with = |server: &ProviderServer, previous_end: Option<usize>| {
        send(
            Endpoint {
                base_url: endpoint(server).base_url,
                ..markers()
            },
            &ModelRequest {
                conversation: conversation.clone(),
                session_dir: session.path().to_path_buf(),
                previous_end,
                cache_lifetime: CacheLifetime::OneHour,
                ..request()
            },
        );
    };
    // As the last message, the image message carries the new end's marker
    // on its last part.
    let server = ProviderServer::start([completed_reply(), completed_reply()]).unwrap();
    send_with(&server, None);
    send_with(&server, Some(3));
    let new_end = sent_body(&server, 0);
    assert_eq!(
        new_end["messages"][4]["content"],
        marked_image(&hour),
        "the image message is the last message"
    );
    assert_eq!(new_end["messages"][3]["content"], "18 C, clear");
    // With `previous_end` on the tool result, the marker lands on the image
    // message the run ends with, not the tool message.
    let previous_end = sent_body(&server, 1);
    assert_eq!(previous_end["messages"][4]["content"], marked_image(&hour));
    assert_eq!(previous_end["messages"][3]["content"], "18 C, clear");
}

#[test]
fn a_dropped_input_after_a_tool_result_does_not_break_the_previous_end() {
    // A reasoning input for another model reference adds no message, so its
    // `ends` entry still points at the tool message when the image message
    // is flushed. The previous-end marker must land on the image message
    // the run ends with, not the tool message.
    let session = image_session();
    let mut conversation = image_turn(
        "a_1",
        "call_1",
        "18 C, clear",
        vec![png_ref("artifacts/i_1.png")],
    );
    conversation.push(Input::Reasoning {
        model: "openai/gpt-6-luna".into(),
        text: "another model's thoughts".into(),
        provider_item: Some(json!({"reasoning_content": "other"})),
    });
    let server = ProviderServer::start([completed_reply()]).unwrap();
    send(
        Endpoint {
            base_url: endpoint(&server).base_url,
            ..markers()
        },
        &ModelRequest {
            conversation,
            session_dir: session.path().to_path_buf(),
            previous_end: Some(3),
            ..request()
        },
    );
    let messages = sent_body(&server, 0)["messages"].clone();
    assert_eq!(messages[2]["content"], "18 C, clear");
    assert_eq!(
        messages[3]["content"],
        json!([{"type": "image_url",
            "image_url": {"url": "data:image/png;base64,YWJjZA=="},
            "cache_control": {"type": "ephemeral"}}])
    );
}

/// A usage chunk with `cost` added: `usage()` carries none.
fn usage_cost(cost: Value) -> Value {
    let mut chunk = usage(15, 14, 9);
    chunk["usage"]["cost"] = cost;
    chunk
}

fn costed_stream(costs: &[Value]) -> Vec<u8> {
    let mut chunks = vec![chunk(json!({"content": "ok"}), Some("stop"))];
    chunks.extend(costs.iter().cloned());
    stream(&chunks)
}

#[test]
fn usage_without_a_cost_carries_no_inline_cost() {
    let bytes = costed_stream(&[usage(15, 14, 9)]);
    let (reply, _) = decoded(&bytes);
    assert_eq!(reply.unwrap().cost, None);
}

#[test]
fn a_null_string_or_negative_cost_carries_no_inline_cost() {
    for cost in [json!(null), json!("0.1"), json!(-1)] {
        let bytes = costed_stream(&[usage_cost(cost.clone())]);
        let (reply, _) = decoded(&bytes);
        assert_eq!(reply.unwrap().cost, None, "{cost}");
    }
}

#[test]
fn a_zero_cost_is_a_reported_cost() {
    let bytes = costed_stream(&[usage_cost(json!(0))]);
    let (reply, _) = decoded(&bytes);
    assert_eq!(reply.unwrap().cost, Some(0.0));
}

#[test]
fn where_usage_repeats_the_last_chunks_cost_wins() {
    let bytes = costed_stream(&[usage_cost(json!(0.5)), usage_cost(json!(0.000123))]);
    let (reply, _) = decoded(&bytes);
    assert_eq!(reply.unwrap().cost, Some(0.000123));
}

#[test]
fn an_openrouter_stream_decodes_its_inline_cost() {
    let recorded: Value = serde_json::from_slice(
        &std::fs::read(research("openrouter-cost/raw/results.json")).unwrap(),
    )
    .unwrap();
    let run = recorded
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["stream"].as_bool().unwrap_or(false))
        .unwrap();
    let mut sse = String::new();
    for chunk in run["chunks"].as_array().unwrap() {
        if let Some(done) = chunk.as_str() {
            sse.push_str(&format!("data: {done}\n\n"));
        } else {
            sse.push_str(&format!("data: {chunk}\n\n"));
        }
    }
    let (reply, _) = decoded(sse.as_bytes());
    let reply = reply.unwrap();
    assert_eq!(
        reply.cost,
        run["inline_usage"]["cost"].as_f64(),
        "the stream's own cost"
    );
    assert_eq!(reply.tokens, tokens(&run["inline_usage"], "5m"));
}
