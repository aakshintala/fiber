//! The `probe` jig through its built binary: the probe's own checks and
//! the binary's behaviour against fakes. The unit tests live in
//! `tests/support/probe_tests.rs`, included through `support/probe.rs`
//! below, so they run inside this binary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers outside a #[test]; a failure is the test's"
)]

#[path = "support/probe.rs"]
mod support;

#[path = "support/harness.rs"]
mod harness;

use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};

use fakes::{MUST_SUCCEED_WITHIN, ProviderServer, Request, Response};
use serde_json::{Value, json};

/// The fake key's value: redaction turns it into `[redacted]`, so any leak
/// of the value itself reads back in uppercase.
const KEY: &str = "[REDACTED]";

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/probe`), so the test starts no cargo and
/// waits on no build lock. Warming before returning means every exec site
/// holds only a warmed path.
fn example_path() -> fakes::WarmedJig {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    fakes::WarmedJig::warm(path.join("examples").join("probe"), &[], 2)
}

/// Runs the built `probe` binary with `args` and waits for it. The child
/// is in its own process group under a watchdog, so a stall fails naming
/// the wait and leaves no process behind.
#[track_caller]
fn run_jig(args: &[&str]) -> Output {
    let warmed = example_path();
    let child = Command::new(warmed.path())
        .args(args)
        .env("NO_PROXY", "*")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let output = fakes::wait_output(child, "the probe jig finished");
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    output
}

/// A `TempDir` holding a `key` file with the fake key, returning its path.
fn key_file(dir: &fakes::TempDir) -> String {
    let path = dir.path().join("key");
    std::fs::write(&path, KEY).unwrap();
    path.display().to_string()
}

/// The request body the server recorded at `n`, as JSON.
fn body(server: &ProviderServer, n: usize) -> Value {
    serde_json::from_slice(&server.requests()[n].body).unwrap()
}

/// The body's text, for marker checks.
fn body_text(server: &ProviderServer, n: usize) -> String {
    String::from_utf8_lossy(&server.requests()[n].body).into_owned()
}

/// Whether the body sends deferred tools: the planner's branching marker.
fn defers(body: &str) -> bool {
    body.contains("defer_loading")
}

/// Whether the body answers a search: the after-load marker.
fn answers(body: &str) -> bool {
    body.contains("tool_reference") || body.contains("tool_search_output")
}

fn protocols() -> [&'static str; 2] {
    ["anthropic-messages", "openai-responses"]
}

/// An Anthropic text reply reporting `input` tokens with `read` cached.
fn anthropic_text(input: u64, read: u64) -> Response {
    Response::stream(harness::anthropic_sse(&[
        json!({"type": "message_start", "message": {"id": "msg_1"}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "standup at nine."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"input_tokens": input, "cache_read_input_tokens": read, "output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]))
}

/// An Anthropic reply of tool calls reporting `input` tokens.
fn anthropic_calls(calls: &[(&str, &str, Value)], input: u64, read: u64) -> Response {
    let mut events = vec![json!({"type": "message_start", "message": {"id": "msg_1"}})];
    for (index, (name, id, arguments)) in calls.iter().enumerate() {
        events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": arguments}}));
        events.push(json!({"type": "content_block_stop", "index": index}));
    }
    events.push(json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"input_tokens": input, "cache_read_input_tokens": read, "output_tokens": 5}}));
    events.push(json!({"type": "message_stop"}));
    Response::stream(harness::anthropic_sse(&events))
}

/// A Responses text reply reporting `input` tokens with `cached` of them.
fn responses_text(input: u64, cached: u64) -> Response {
    Response::stream(harness::responses_sse(&[
        json!({"type": "response.output_item.done", "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": [{"type": "output_text", "text": "standup at nine."}]}}),
        responses_completed(input, cached),
    ]))
}

/// A Responses terminal event reporting `input` tokens with `cached` read.
fn responses_completed(input: u64, cached: u64) -> Value {
    json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed", "usage": {"input_tokens": input, "input_tokens_details": {"cached_tokens": cached}, "output_tokens": 5}}})
}

/// A Responses `tool_search` call.
fn responses_search() -> Value {
    json!({"type": "response.output_item.done", "item": {"type": "tool_search_call", "id": "ts_1", "call_id": "call_1", "execution": "client", "arguments": {"query": "calendar"}, "status": "completed"}})
}

/// A Responses function call.
fn responses_call(name: &str, call_id: &str) -> Value {
    json!({"type": "response.output_item.done", "item": {"type": "function_call", "id": "fc_1", "call_id": call_id, "name": name, "arguments": "{\"title\":\"standup\"}"}})
}

/// A Responses reasoning item, as the passing fake sends before the search.
fn responses_reasoning() -> Value {
    json!({"type": "response.output_item.done", "item": {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "Let me search the calendar tools."}]}})
}

/// The baseline reply: a text reply reporting 12000 input tokens.
fn baseline_reply(protocol: &str) -> Response {
    match protocol {
        "anthropic-messages" => anthropic_text(12000, 0),
        _ => responses_text(12000, 0),
    }
}

/// The deferral rejection: a 400 naming `defer_loading`.
fn rejection(protocol: &str) -> Response {
    let body = match protocol {
        "anthropic-messages" => {
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": "defer_loading is not supported"}})
        }
        _ => {
            json!({"error": {"message": "Unknown parameter: 'tools[1].defer_loading'.", "type": "invalid_request_error", "param": "tools[1].defer_loading"}})
        }
    };
    Response::status(400, body.to_string())
}

/// Runs the jig once against `responder`, returning its output, request
/// count and bodies. `extra` holds the trailing jig arguments.
fn probe_once(
    protocol: &str,
    responder: impl Fn(&Request) -> Response + Send + Sync + 'static,
    extra: &[&str],
) -> (Output, usize, Vec<String>) {
    let dir = fakes::TempDir::new("fiber-probe-jig");
    let key = key_file(&dir);
    let server = ProviderServer::start_responding(responder).unwrap();
    let url = format!("{}/v1", server.url());
    let mut args = vec![protocol, url.as_str(), "probe-model", key.as_str()];
    args.extend(extra.iter().copied());
    let out = run_jig(&args);
    let count = server.requests().len();
    let bodies = (0..count).map(|n| body_text(&server, n)).collect();
    (out, count, bodies)
}

/// The jig's stdout lines, asserting the key reaches neither stream.
fn stdout_lines(out: &Output) -> Vec<String> {
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(!stdout.contains(KEY), "{stdout}");
    assert!(!stderr.contains(KEY), "{stderr}");
    stdout.lines().map(str::to_owned).collect()
}

/// The output-token cap the request at `n` carries on `protocol`.
fn output_cap(server: &ProviderServer, protocol: &str, n: usize) -> u64 {
    let key = match protocol {
        "anthropic-messages" => "max_tokens",
        _ => "max_output_tokens",
    };
    body(server, n)[key].as_u64().unwrap()
}

#[test]
fn it_reports_usage_without_arguments() {
    let out = run_jig(&[]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("usage: cargo run -p provider --example probe --"),
        "{stderr}"
    );
}

#[test]
fn a_bad_option_is_rejected_before_any_request() {
    for protocol in protocols() {
        let dir = fakes::TempDir::new("fiber-probe-jig");
        let key = key_file(&dir);
        let server =
            ProviderServer::start_responding(|_| baseline_reply("anthropic-messages")).unwrap();
        let url = format!("{}/v1", server.url());
        let out = run_jig(&[protocol, &url, "probe-model", &key, "--store=maybe"]);
        assert_eq!(out.status.code(), Some(2));
        assert_eq!(server.requests().len(), 0);
    }
}

#[test]
fn rejects_deferral() {
    for protocol in protocols() {
        let responder = move |req: &Request| {
            let text = String::from_utf8_lossy(&req.body).into_owned();
            if !defers(&text) {
                baseline_reply(protocol)
            } else {
                rejection(protocol)
            }
        };
        let dir = fakes::TempDir::new("fiber-probe-jig");
        let key = key_file(&dir);
        let server = ProviderServer::start_responding(responder).unwrap();
        let url = format!("{}/v1", server.url());
        let out = run_jig(&[protocol, &url, "probe-model", &key]);
        assert_eq!(out.status.code(), Some(1), "{protocol}");
        let lines = stdout_lines(&out);
        let expected_load = match protocol {
            "anthropic-messages" => "load: fail (HTTP 400: defer_loading is not supported)",
            _ => "load: fail (HTTP 400: Unknown parameter: 'tools[1].defer_loading'.",
        };
        if protocol == "anthropic-messages" {
            assert_eq!(
                lines,
                vec![
                    "probe: anthropic-messages probe-model",
                    "load: fail (HTTP 400: defer_loading is not supported)",
                    "cache: fail (not established: load rejected)",
                    "probe: fail",
                    "spend: 12000 input, 3 output tokens",
                ]
            );
        } else {
            assert_eq!(lines.len(), 5);
            assert_eq!(lines[0], "probe: openai-responses probe-model");
            assert!(lines[1].starts_with(expected_load), "{}", lines[1]);
            assert_eq!(lines[2], "cache: fail (not established: load rejected)");
            assert_eq!(lines[3], "probe: fail");
            assert_eq!(lines[4], "spend: 12000 input, 5 output tokens");
        }
        assert_eq!(server.requests().len(), 2);
        let bodies: Vec<String> = (0..2).map(|n| body_text(&server, n)).collect();
        assert!(!defers(&bodies[0]));
        assert!(defers(&bodies[1]));
        assert_eq!(output_cap(&server, protocol, 0), 2000);
        assert_eq!(output_cap(&server, protocol, 1), 2000);
    }
}

#[test]
fn ignores_deferral() {
    for protocol in protocols() {
        let responder = move |req: &Request| {
            let text = String::from_utf8_lossy(&req.body).into_owned();
            if !defers(&text) {
                return baseline_reply(protocol);
            }
            if answers(&text) {
                return baseline_reply(protocol);
            }
            match protocol {
                "anthropic-messages" => anthropic_calls(
                    &[(
                        "probe_calendar_create",
                        "toolu_2",
                        json!({"title": "standup"}),
                    )],
                    5200,
                    0,
                ),
                _ => Response::stream(harness::responses_sse(&[
                    responses_call("probe_calendar_create", "call_2"),
                    responses_completed(5200, 0),
                ])),
            }
        };
        let (out, count, _) = probe_once(protocol, responder, &[]);
        assert_eq!(out.status.code(), Some(1), "{protocol}");
        let spend = match protocol {
            "anthropic-messages" => "spend: 17200 input, 8 output tokens",
            _ => "spend: 17200 input, 10 output tokens",
        };
        assert_eq!(
            stdout_lines(&out),
            vec![
                format!("probe: {protocol} probe-model"),
                "load: fail (called probe_calendar_create before loading it)".to_owned(),
                "cache: fail (not established: load did not complete)".to_owned(),
                "probe: fail".to_owned(),
                spend.to_owned(),
            ]
        );
        assert_eq!(count, 2);
    }
}

#[test]
fn passes() {
    for protocol in protocols() {
        let responder = move |req: &Request| {
            let text = String::from_utf8_lossy(&req.body).into_owned();
            if !defers(&text) {
                return baseline_reply(protocol);
            }
            if answers(&text) {
                return match protocol {
                    "anthropic-messages" => anthropic_calls(
                        &[(
                            "probe_calendar_create",
                            "toolu_2",
                            json!({"title": "standup"}),
                        )],
                        300,
                        5120,
                    ),
                    _ => Response::stream(harness::responses_sse(&[
                        responses_call("probe_calendar_create", "call_2"),
                        responses_completed(5420, 5120),
                    ])),
                };
            }
            match protocol {
                "anthropic-messages" => anthropic_calls(
                    &[("tool_search", "toolu_1", json!({"query": "calendar"}))],
                    5200,
                    0,
                ),
                _ => Response::stream(harness::responses_sse(&[
                    responses_reasoning(),
                    responses_search(),
                    responses_completed(5200, 0),
                ])),
            }
        };
        let dir = fakes::TempDir::new("fiber-probe-jig");
        let key = key_file(&dir);
        let server = ProviderServer::start_responding(responder).unwrap();
        let url = format!("{}/v1", server.url());
        let out = run_jig(&[protocol, &url, "probe-model", &key]);
        assert_eq!(out.status.code(), Some(0), "{protocol}");
        let spend = match protocol {
            "anthropic-messages" => "spend: 22620 input, 13 output tokens",
            _ => "spend: 22620 input, 15 output tokens",
        };
        assert_eq!(
            stdout_lines(&out),
            vec![
                format!("probe: {protocol} probe-model"),
                "load: pass (searched, loaded probe_calendar_create, then called it)".to_owned(),
                "cache: pass (cache read 5120 of 5420 input tokens)".to_owned(),
                "probe: pass".to_owned(),
                spend.to_owned(),
            ]
        );
        assert_eq!(server.requests().len(), 3);
        assert!(answers(&body_text(&server, 2)));
    }
}

#[test]
fn calls_unloaded_after_load() {
    for protocol in protocols() {
        let responder = move |req: &Request| {
            let text = String::from_utf8_lossy(&req.body).into_owned();
            if !defers(&text) {
                return baseline_reply(protocol);
            }
            if answers(&text) {
                return match protocol {
                    "anthropic-messages" => anthropic_calls(
                        &[
                            (
                                "probe_calendar_create",
                                "toolu_2",
                                json!({"title": "standup"}),
                            ),
                            (
                                "probe_calendar_delete",
                                "toolu_3",
                                json!({"title": "standup"}),
                            ),
                        ],
                        300,
                        5120,
                    ),
                    _ => Response::stream(harness::responses_sse(&[
                        responses_call("probe_calendar_create", "call_2"),
                        responses_call("probe_calendar_delete", "call_3"),
                        responses_completed(5420, 5120),
                    ])),
                };
            }
            match protocol {
                "anthropic-messages" => anthropic_calls(
                    &[("tool_search", "toolu_1", json!({"query": "calendar"}))],
                    5200,
                    0,
                ),
                _ => Response::stream(harness::responses_sse(&[
                    responses_reasoning(),
                    responses_search(),
                    responses_completed(5200, 0),
                ])),
            }
        };
        let (out, count, _) = probe_once(protocol, responder, &[]);
        assert_eq!(out.status.code(), Some(1), "{protocol}");
        let spend = match protocol {
            "anthropic-messages" => "spend: 22620 input, 13 output tokens",
            _ => "spend: 22620 input, 15 output tokens",
        };
        assert_eq!(
            stdout_lines(&out),
            vec![
                format!("probe: {protocol} probe-model"),
                "load: fail (called probe_calendar_delete before loading it)".to_owned(),
                "cache: pass (cache read 5120 of 5420 input tokens)".to_owned(),
                "probe: fail".to_owned(),
                spend.to_owned(),
            ]
        );
        assert_eq!(count, 3);
    }
}

#[test]
fn cache_miss() {
    for protocol in protocols() {
        let responder = move |req: &Request| {
            let text = String::from_utf8_lossy(&req.body).into_owned();
            if !defers(&text) {
                return baseline_reply(protocol);
            }
            if answers(&text) {
                return match protocol {
                    "anthropic-messages" => anthropic_calls(
                        &[(
                            "probe_calendar_create",
                            "toolu_2",
                            json!({"title": "standup"}),
                        )],
                        300,
                        0,
                    ),
                    _ => Response::stream(harness::responses_sse(&[
                        responses_call("probe_calendar_create", "call_2"),
                        responses_completed(300, 0),
                    ])),
                };
            }
            match protocol {
                "anthropic-messages" => anthropic_calls(
                    &[("tool_search", "toolu_1", json!({"query": "calendar"}))],
                    5200,
                    0,
                ),
                _ => Response::stream(harness::responses_sse(&[
                    responses_reasoning(),
                    responses_search(),
                    responses_completed(5200, 0),
                ])),
            }
        };
        let (out, count, _) = probe_once(protocol, responder, &[]);
        assert_eq!(out.status.code(), Some(1), "{protocol}");
        let lines = stdout_lines(&out);
        assert_eq!(lines[0], format!("probe: {protocol} probe-model"));
        assert_eq!(lines[2], "cache: fail (cache read 0 of 300 input tokens)");
        assert_eq!(lines[3], "probe: fail");
        let spend = match protocol {
            "anthropic-messages" => "spend: 17500 input, 13 output tokens",
            _ => "spend: 17500 input, 15 output tokens",
        };
        assert_eq!(lines[4], spend);
        assert_eq!(count, 3);
    }
}

#[test]
fn cache_one() {
    for protocol in protocols() {
        let responder = move |req: &Request| {
            let text = String::from_utf8_lossy(&req.body).into_owned();
            if !defers(&text) {
                return baseline_reply(protocol);
            }
            if answers(&text) {
                return match protocol {
                    "anthropic-messages" => anthropic_calls(
                        &[(
                            "probe_calendar_create",
                            "toolu_2",
                            json!({"title": "standup"}),
                        )],
                        300,
                        1,
                    ),
                    _ => Response::stream(harness::responses_sse(&[
                        responses_call("probe_calendar_create", "call_2"),
                        responses_completed(301, 1),
                    ])),
                };
            }
            match protocol {
                "anthropic-messages" => anthropic_calls(
                    &[("tool_search", "toolu_1", json!({"query": "calendar"}))],
                    5200,
                    0,
                ),
                _ => Response::stream(harness::responses_sse(&[
                    responses_reasoning(),
                    responses_search(),
                    responses_completed(5200, 0),
                ])),
            }
        };
        let (out, count, _) = probe_once(protocol, responder, &[]);
        assert_eq!(out.status.code(), Some(0), "{protocol}");
        let lines = stdout_lines(&out);
        assert_eq!(lines[2], "cache: pass (cache read 1 of 301 input tokens)");
        let spend = match protocol {
            "anthropic-messages" => "spend: 17501 input, 13 output tokens",
            _ => "spend: 17501 input, 15 output tokens",
        };
        assert_eq!(lines[4], spend);
        assert_eq!(count, 3);
    }
}

#[test]
fn refused_key() {
    for protocol in protocols() {
        let responder = move |_: &Request| {
            Response::status(
                401,
                json!({"error": {"message": format!("key {KEY} refused")}}).to_string(),
            )
        };
        let (out, count, _) = probe_once(protocol, responder, &[]);
        assert_eq!(out.status.code(), Some(4), "{protocol}");
        assert_eq!(
            stdout_lines(&out),
            vec![
                format!("probe: {protocol} probe-model"),
                "load: fail (not established: key refused)".to_owned(),
                "cache: fail (not established: key refused)".to_owned(),
                "probe: key refused (HTTP 401)".to_owned(),
            ]
        );
        assert_eq!(count, 1);
    }
}

#[test]
fn cannot_tell() {
    for (status, baseline) in [(404, "baseline: HTTP 404"), (429, "baseline: HTTP 429")] {
        for protocol in protocols() {
            let responder = move |_: &Request| {
                Response::status(
                    status,
                    json!({"error": {"message": "not here"}}).to_string(),
                )
            };
            let (out, count, _) = probe_once(protocol, responder, &[]);
            assert_eq!(out.status.code(), Some(3), "{protocol} {status}");
            assert_eq!(
                stdout_lines(&out),
                vec![
                    format!("probe: {protocol} probe-model"),
                    format!("load: fail (inconclusive: {baseline})"),
                    "cache: fail (not established: baseline inconclusive)".to_owned(),
                    format!("probe: cannot tell ({baseline})"),
                    "spend: 0 input, 0 output tokens".to_owned(),
                ]
            );
            assert_eq!(count, 1);
        }
    }
    for protocol in protocols() {
        let responder = move |req: &Request| {
            let text = String::from_utf8_lossy(&req.body).into_owned();
            if !defers(&text) {
                return baseline_reply(protocol);
            }
            match protocol {
                "anthropic-messages" => anthropic_text(5200, 0),
                _ => responses_text(5200, 0),
            }
        };
        let (out, count, _) = probe_once(protocol, responder, &[]);
        assert_eq!(out.status.code(), Some(3), "{protocol}");
        let spend = match protocol {
            "anthropic-messages" => "spend: 17200 input, 6 output tokens",
            _ => "spend: 17200 input, 10 output tokens",
        };
        assert_eq!(
            stdout_lines(&out),
            vec![
                format!("probe: {protocol} probe-model"),
                "load: fail (inconclusive: the model made no tool call)".to_owned(),
                "cache: fail (not established: load did not complete)".to_owned(),
                "probe: cannot tell (the model made no tool call)".to_owned(),
                spend.to_owned(),
            ]
        );
        assert_eq!(count, 2);
    }
}
