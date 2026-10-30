//! Native provider requests and session memory at the binary level
//! (`docs/state.md`, "Diagnostic logs"): the built `fiber` runs `ask`
//! against fake provider servers. With the default nothing debug is
//! written; a debug run writes one `http_request` per attempt and a
//! `resources` per turn and at the orderly end.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::{
    HELLO_KINDS, Run, Setup, function_call, hello, is_status, run_to_exit, stream, text_reply,
    write_json,
};

/// Runs `fiber ask` with `args` in the workspace to exit and parses its
/// stdout.
#[track_caller]
fn ask_with(setup: &Setup, args: &[&str]) -> Run {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn ask(setup: &Setup) -> Run {
    ask_with(setup, &["ask", "hi"])
}

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    let mut files: Vec<_> = fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

fn diagnostic_file(setup: &Setup, name: &str) -> Vec<Value> {
    let text = fs::read_to_string(setup.home().join("logs").join(name)).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Writes the home config with `model` and `diagnostics.level: debug`,
/// keeping the installed model's key.
fn debug_config(setup: &Setup, extra: Value) {
    let mut config = json!({"model": "fake/m", "diagnostics": {"level": "debug"}});
    let object = config.as_object_mut().expect("the config is an object");
    for (key, value) in extra.as_object().unwrap() {
        object.insert(key.clone(), value.clone());
    }
    write_json(&setup.home().join("config.json"), &config);
}

fn ask_debug(setup: &Setup, server: &ProviderServer, extra: Value) -> Run {
    setup.provider(server);
    debug_config(setup, extra);
    ask(setup)
}

#[test]
fn ask_debug_writes_one_http_request_and_two_resources_lines() {
    let setup = Setup::new();
    let body_len = hello().body.len();
    let server = ProviderServer::start([hello()]).unwrap();
    let run = ask_debug(&setup, &server, json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing test really asked"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert_eq!(requests.len(), 1, "one call writes one line");
    let line = requests[0];
    assert_eq!(line["level"], "debug");
    assert_eq!(line["process"], "ask");
    assert_eq!(line["session_id"], id.as_str());
    assert_eq!(line["message"], "An HTTP request ended.");
    let data = &line["data"];
    assert_eq!(data["provider"], "fake");
    assert!(
        data.get("extension").is_none(),
        "no extension on a provider purpose: {data}"
    );
    assert_eq!(data["model"], "m");
    assert_eq!(data["purpose"], "model_call");
    let url = server.url();
    let host = url.split_once("://").unwrap().1;
    assert_eq!(data["host"], host);
    assert_eq!(data["path"], "/v1/responses");
    assert_eq!(data["status"], 200);
    assert_eq!(data["attempt"], 1);
    let posted = server.requests();
    assert_eq!(posted.len(), 1);
    assert_eq!(data["request_bytes"], posted[0].body.len() as u64);
    assert!(data["request_bytes"].as_u64().unwrap() > 0);
    assert_eq!(data["response_bytes"], body_len as u64);
    let headers_ms = data["headers_ms"].as_u64().unwrap();
    let first_token_ms = data["first_token_ms"].as_u64().unwrap();
    let total_ms = data["total_ms"].as_u64().unwrap();
    assert!(
        headers_ms <= first_token_ms && first_token_ms <= total_ms,
        "headers <= first token <= total: {data}"
    );
    for sentinel in ["sk-test", "hi", "Hello.", "diagnostics"] {
        assert!(
            !serde_json::to_string(line).unwrap().contains(sentinel),
            "no content in the line: {sentinel}"
        );
    }
    let timings: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "op_timing" && line["data"]["op"] == "net.connect")
        .collect();
    assert_eq!(timings.len(), 1, "one connection writes one net scope");
    let timing = timings[0];
    assert_eq!(timing["level"], "debug");
    assert_eq!(timing["process"], "ask");
    assert_eq!(timing["session_id"], id.as_str());
    assert_eq!(timing["message"], "An operation ended.");
    let data = timing["data"]
        .as_object()
        .expect("the timing data is an object");
    let mut keys: Vec<&String> = data.keys().collect();
    keys.sort();
    assert_eq!(keys, ["ms", "op"], "the timing data holds only op and ms");
    assert!(
        timing["data"]["ms"].is_u64(),
        "whole milliseconds, never a value to compare: {timing}"
    );
    let wire = serde_json::to_string(timing).unwrap();
    assert!(!wire.contains("null"), "no null in the timing: {wire}");
    for sentinel in [host, "/v1/responses", "sk-test"] {
        assert!(
            !wire.contains(sentinel),
            "no request content in the timing: {sentinel}"
        );
    }
    let peaks: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "resources")
        .collect();
    assert_eq!(peaks.len(), 2, "one turn and one orderly exit");
    let mut disk_read = 0;
    let mut disk_write = 0;
    for peak in &peaks {
        assert_eq!(peak["process"], "ask");
        assert_eq!(peak["session_id"], id.as_str());
        assert!(
            peak["data"]["peak_kib"].as_u64().unwrap() > 0,
            "positive on Linux and macOS: {peak}"
        );
        assert!(
            peak["data"]["current_kib"].as_u64().unwrap() > 0,
            "positive on Linux and macOS: {peak}"
        );
        let read = peak["data"]["disk_read_bytes"].as_u64().unwrap();
        let written = peak["data"]["disk_write_bytes"].as_u64().unwrap();
        assert!(read >= disk_read, "counters never go back: {peak}");
        assert!(written >= disk_write, "counters never go back: {peak}");
        disk_read = read;
        disk_write = written;
    }
    assert_eq!(
        lines.last().unwrap()["code"],
        "resources",
        "the last diagnostic line is the final memory"
    );
}

#[test]
fn a_retried_call_writes_one_line_per_attempt() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(503, "try again"), hello()]).unwrap();
    let run = ask_debug(
        &setup,
        &server,
        json!({"retry": {"attempts": 1, "initial_delay_ms": 0, "max_delay_ms": 0}}),
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "both attempts arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["data"]["attempt"], 1);
    assert_eq!(requests[0]["data"]["status"], 503);
    assert!(
        requests[0]["data"].get("first_token_ms").is_none(),
        "no token before the retry: {}",
        requests[0]
    );
    assert_eq!(requests[1]["data"]["attempt"], 2);
    assert_eq!(requests[1]["data"]["status"], 200);
    assert!(requests[1]["data"].get("first_token_ms").is_some());
    let timings: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "op_timing" && line["data"]["op"] == "net.connect")
        .collect();
    assert_eq!(timings.len(), 2, "two connections write two net scopes");
    for timing in &timings {
        assert_eq!(timing["level"], "debug");
        assert_eq!(timing["process"], "ask");
        assert_eq!(timing["session_id"], id.as_str());
        assert!(
            timing["data"]["ms"].is_u64(),
            "whole milliseconds, never a value to compare: {timing}"
        );
        assert!(
            timing["data"].get("tool").is_none(),
            "a connector scope names no tool: {timing}"
        );
    }
}

#[test]
fn two_logical_calls_restart_attempts_at_one() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "nothing secret\n").unwrap();
    let server = ProviderServer::start([
        stream(&[function_call("c1", "read", &json!({"path": "note.txt"}))]),
        hello(),
    ])
    .unwrap();
    let run = ask_debug(&setup, &server, json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "both logical calls arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let attempts: Vec<u64> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .map(|line| line["data"]["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(attempts, vec![1, 1], "each logical call starts at 1");
}

#[test]
fn at_default_no_http_request_is_written() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing test really asked"
    );
    let lines = log_lines(&setup);
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|line| line["code"] != "http_request"
            && line["code"] != "resources"
            && line["code"] != "op_timing"),
        "no native request, memory or timing line at the default: {lines:?}"
    );
}

#[test]
fn a_repository_level_is_ignored_for_native_requests() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"diagnostics": {"level": "debug"}}),
    );
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing test really asked"
    );
    assert!(
        run.all("notice")
            .iter()
            .any(|notice| notice["payload"]["code"] == "config_key_ignored"),
        "the repository value is ignored with a notice"
    );
    let lines = log_lines(&setup);
    assert!(
        lines.iter().all(|line| line["code"] != "http_request"
            && line["code"] != "resources"
            && line["code"] != "op_timing"),
        "the repository value stays ignored: {lines:?}"
    );
}

/// Installs provider `fake` with model `m` on `protocol` at the fake
/// server, as [`Setup::provider`] does for `openai-responses`.
fn install_protocol(setup: &Setup, protocol: &str, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": protocol, "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m"}),
    );
}

/// A Gemini stream answering `Hello.`: the URL carries `?alt=sse`.
fn gemini_hello() -> Response {
    let chunk = json!({"candidates": [{"content": {"role": "model",
        "parts": [{"text": "Hello."}]}, "index": 0, "finishReason": "STOP"}],
        "responseId": "r1",
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 3}});
    let body = format!("data: {chunk}\r\n\r\n");
    Response::stream(body).header("content-type", "text/event-stream")
}

#[test]
fn a_query_bearing_url_is_logged_without_it() {
    let setup = Setup::new();
    let body_len = gemini_hello().body.len();
    let server = ProviderServer::start([gemini_hello()]).unwrap();
    install_protocol(&setup, "google-generative-ai", &server);
    debug_config(&setup, json!({}));
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert_eq!(requests.len(), 1);
    let data = &requests[0]["data"];
    assert_eq!(data["path"], "/v1/models/m:streamGenerateContent");
    assert_eq!(data["response_bytes"], body_len as u64);
    let text = serde_json::to_string(requests[0]).unwrap();
    assert!(!text.contains('?'), "no query in the line: {text}");
    assert!(!text.contains("alt=sse"), "no query in the line: {text}");
    // The transport itself is unchanged: the query went out as built.
    let posted = server.requests();
    assert_eq!(posted.len(), 1);
    assert!(
        posted[0].path.contains("alt=sse"),
        "the actual query is unchanged: {}",
        posted[0].path
    );
}

#[test]
fn an_orderly_model_failure_still_writes_final_memory() {
    let setup = Setup::new();
    let server =
        ProviderServer::start([Response::status(500, "boom"), Response::status(500, "boom")])
            .unwrap();
    let run = ask_debug(
        &setup,
        &server,
        json!({"retry": {"attempts": 1, "initial_delay_ms": 0, "max_delay_ms": 0}}),
    );
    assert_ne!(run.code, Some(0), "the model failure exits nonzero");
    assert!(
        run.kinds().contains(&"fiber_exited"),
        "the exit is orderly: {:?}",
        run.kinds()
    );
    assert!(
        server.await_requests(2, setup.deadline),
        "both attempts arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let attempts: Vec<u64> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .map(|line| line["data"]["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(attempts, vec![1, 2]);
    let peaks: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "resources")
        .collect();
    assert_eq!(
        peaks.len(),
        2,
        "the failed turn and the exit each write one"
    );
    assert!(
        peaks[0]["data"]["peak_kib"].as_u64().unwrap() > 0,
        "{}",
        peaks[0]
    );
    assert_eq!(
        lines.last().unwrap()["code"],
        "resources",
        "the last diagnostic line is the final memory"
    );
}

#[test]
fn a_session_close_without_a_turn_writes_only_exit_memory() {
    let setup = Setup::new();
    let server = ProviderServer::start(Vec::<Response>::new()).unwrap();
    setup.provider(&server);
    debug_config(
        &setup,
        json!({"session": {"idle_exit_ms": 0}, "model": "fake/m"}),
    );
    let id = doors::mint("s_");
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let output = run_to_exit(
        setup.deadline,
        "the session",
        setup.fiber(&["session", "--id", &id, "--workspace", &workspace]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(server.requests().is_empty(), "no turn ran");
    let lines = diagnostic_file(&setup, &format!("session-{id}.log"));
    assert!(
        lines.iter().all(|line| line["code"] != "http_request"),
        "no request without a turn"
    );
    let peaks: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "resources")
        .collect();
    assert_eq!(peaks.len(), 1, "only the exit memory: {lines:?}");
    assert_eq!(peaks[0]["process"], "session");
    assert_eq!(peaks[0]["session_id"], id.as_str());
}

#[test]
fn a_startup_failure_writes_no_request_or_memory() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup, json!({"model": "fake/nope"}));
    let run = ask(&setup);
    assert_ne!(run.code, Some(0), "the startup failure exits nonzero");
    assert!(server.requests().is_empty(), "no request was built");
    let lines = log_lines(&setup);
    assert!(
        lines
            .iter()
            .all(|line| line["code"] != "http_request" && line["code"] != "resources"),
        "no request, turn or exit memory without a session: {lines:?}"
    );
}

#[test]
fn a_resumed_ask_keeps_the_ask_process_and_restarts_attempts() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    let first = ask_debug(&setup, &server, json!({}));
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();
    let second = ask_with(&setup, &["ask", "--resume", &id, "again"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "both asks arrived"
    );
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    for line in &lines {
        assert_eq!(line["process"], "ask");
        assert_eq!(line["session_id"], id.as_str());
    }
    let attempts: Vec<u64> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .map(|line| line["data"]["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(attempts, vec![1, 1], "each ask run starts at 1");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["code"] == "resources")
            .count(),
        4,
        "each ask run writes its turn and exit memory"
    );
    // The durable kinds never mention the diagnostic codes.
    assert!(
        second
            .kinds()
            .iter()
            .all(|kind| *kind != "http_request" && *kind != "resources"),
        "{:?}",
        second.kinds()
    );
}

/// Installs provider `name` with model `m` on `protocol` at `url`.
fn install_url(setup: &Setup, name: &str, url: &str, protocol: &str) {
    let source = setup.root.path().join(format!("src-{name}"));
    write_json(
        &source.join("extension.json"),
        &json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {name: [format!("{url}/v1")]}}),
    );
    write_json(
        &source.join(format!("providers/{name}.json")),
        &json!({
            "name": name,
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": protocol, "base_url": format!("{url}/v1"), "context_window": 100000}]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

fn spawn_call(model: &str) -> Value {
    function_call(
        "call_delegate",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello", "model": model}),
    )
}

#[test]
fn a_delegate_child_uses_its_own_process_and_id() {
    let setup = Setup::new();
    let parent = ProviderServer::start_responding(|request| {
        let text = String::from_utf8_lossy(&request.body);
        // Reviewer requests carry no tools; the first session request
        // spawns the delegate; every request after the receipt is done.
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if !text.contains("Started delegate") {
            return stream(&[spawn_call("fiber:pb/m")]);
        }
        text_reply("done")
    })
    .unwrap();
    let child = ProviderServer::start([text_reply("hi from child")]).unwrap();
    install_url(&setup, "pa", &parent.url(), "openai-responses");
    install_url(&setup, "pb", &child.url(), "openai-responses");
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "diagnostics": {"level": "debug"}, "session": {"idle_exit_ms": 3600000}}),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""})
        ),
    )
    .unwrap();
    let run = ask_with(&setup, &["ask", "scan the tree"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        parent.await_requests(1, setup.deadline) && child.await_requests(1, setup.deadline),
        "both sides asked"
    );
    let parent_id = run.session_id().to_owned();
    let started = run
        .all("delegate_started")
        .into_iter()
        .next()
        .expect("one delegate started")
        .clone();
    let child_id = started["payload"]["delegate_session_id"]
        .as_str()
        .expect("the start names the child")
        .to_owned();
    assert_ne!(child_id, parent_id);
    // The child diagnostics use the child's process and id, not the
    // parent's.
    let child_lines = diagnostic_file(&setup, &format!("session-{child_id}.log"));
    assert!(!child_lines.is_empty());
    for line in &child_lines {
        assert_eq!(line["process"], "session");
        assert_eq!(line["session_id"], child_id.as_str());
    }
    let child_requests: Vec<&Value> = child_lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert_eq!(child_requests.len(), 1);
    assert_eq!(child_requests[0]["data"]["provider"], "pb");
    assert_eq!(child_requests[0]["data"]["model"], "m");
    assert!(
        child_lines.iter().any(|line| line["code"] == "resources"),
        "the child writes its exit memory"
    );
    let parent_lines = diagnostic_file(&setup, &format!("ask-{parent_id}.log"));
    let parent_requests: Vec<&Value> = parent_lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert!(
        parent_requests
            .iter()
            .all(|line| line["data"]["provider"] == "pa"),
        "the parent's lines name only its provider"
    );
}

/// A debug ask with the cache-diagnostics setting on.
fn ask_debug_diagnostics(setup: &Setup, server: &ProviderServer, extra: Value) -> Run {
    let mut merged = json!({"cache": {"diagnostics": true}});
    let object = merged
        .as_object_mut()
        .expect("the merged config is an object");
    for (key, value) in extra.as_object().unwrap() {
        object.insert(key.clone(), value.clone());
    }
    ask_debug(setup, server, merged)
}

/// The single `http_request` line of a one-call debug run.
#[allow(clippy::indexing_slicing, reason = "len checked on the line above")]
fn the_request(setup: &Setup, run: &Run) -> Value {
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert_eq!(requests.len(), 1, "one call writes one line");
    let line = *requests.first().expect("one call writes one line");
    assert_eq!(line["session_id"], id.as_str());
    assert_eq!(line["data"]["attempt"], 1);
    (*line).clone()
}

/// A 64-character lowercase hex digest, as the flagged line holds them.
fn hex64(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[test]
fn with_cache_diagnostics_a_turns_line_carries_hashes_and_reported_use() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    let run = ask_debug_diagnostics(&setup, &server, json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived"
    );
    let line = the_request(&setup, &run);
    let data = &line["data"];
    assert_eq!(data["status"], 200);
    let hashes = data.get("prefix_hashes").expect("hashes are set");
    assert!(hex64(&hashes["system"]), "the system segment hashes");
    assert!(hex64(&hashes["tools"]), "the tools list hashes");
    let messages = hashes["messages"].as_array().expect("messages hash");
    assert!(!messages.is_empty(), "the conversation hashes");
    for digest in messages {
        assert!(hex64(digest), "each message boundary hashes");
    }
    assert_eq!(data["cache_read"], 4, "the reported read");
    assert!(data["cache_write"].is_object(), "the reported write");
    // The flagged line stays redacted: neither the asked text, the
    // reply text nor the key reaches it. The envelope still carries the
    // session id: that is the canary's join key.
    let wire = serde_json::to_string(&line).unwrap();
    for sentinel in ["hi", "Hello.", "sk-test"] {
        assert!(
            !wire.contains(sentinel),
            "no content in the line: {sentinel}"
        );
    }
}

#[test]
fn without_the_setting_the_line_holds_what_it_always_held() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    let run = ask_debug(&setup, &server, json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived"
    );
    let line = the_request(&setup, &run);
    let data = line["data"].as_object().expect("the data is an object");
    for field in ["prefix_hashes", "cache_read", "cache_write"] {
        assert!(
            !data.contains_key(field),
            "nothing new with the setting off: {field}"
        );
    }
}

#[test]
fn a_retried_call_with_diagnostics_joins_by_session_and_attempt() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(503, "try again"), hello()]).unwrap();
    let run = ask_debug_diagnostics(
        &setup,
        &server,
        json!({"retry": {"attempts": 1, "initial_delay_ms": 0, "max_delay_ms": 0}}),
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "both attempts arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert_eq!(requests.len(), 2, "one line per attempt");
    for line in &requests {
        assert_eq!(line["session_id"], id.as_str(), "joined by session");
    }
    assert_eq!(requests[0]["data"]["attempt"], 1);
    assert_eq!(requests[0]["data"]["status"], 503);
    assert!(
        requests[0]["data"].get("cache_read").is_none(),
        "no usage on the failure: {}",
        requests[0]
    );
    assert_eq!(requests[1]["data"]["attempt"], 2);
    assert_eq!(requests[1]["data"]["status"], 200);
    assert_eq!(requests[1]["data"]["cache_read"], 4, "usage on the success");
    // The retry reposts the same bytes, so both lines hash them.
    assert_eq!(
        requests[0]["data"]["prefix_hashes"], requests[1]["data"]["prefix_hashes"],
        "the retry rejoins on the same prefix"
    );
}

/// One protocol's answered `Hello.` with the reported cache counts the
/// diagnostics line asserts on.
fn protocol_hello(protocol: &str) -> Response {
    match protocol {
        "anthropic-messages" => Response::stream(
            [
                json!({"type": "message_start", "message": {"id": "msg_1"}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello."}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"input_tokens": 10, "cache_read_input_tokens": 4, "output_tokens": 3}}),
                json!({"type": "message_stop"}),
            ]
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>(),
        ),
        "openai-completions" => Response::stream({
            let mut body: String = [
                json!({"id": "gen-1", "object": "chat.completion.chunk",
                    "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hello."}, "finish_reason": null}]}),
                json!({"id": "gen-1", "object": "chat.completion.chunk",
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
                json!({"id": "gen-1", "choices": [], "usage": {"prompt_tokens": 10,
                    "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 4}}}),
            ]
            .iter()
            .map(|chunk| format!("data: {chunk}\n\n"))
            .collect();
            body.push_str("data: [DONE]\n\n");
            body
        }),
        "google-generative-ai" => gemini_hello(),
        _ => hello(),
    }
}

#[test]
fn a_second_turn_keeps_the_first_turns_prefix_hashes() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    let first = ask_debug_diagnostics(&setup, &server, json!({}));
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();
    let second = ask_with(&setup, &["ask", "--resume", &id, "again"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "both turns arrived"
    );
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert_eq!(requests.len(), 2, "one line per turn");
    for line in &requests {
        assert_eq!(line["session_id"], id.as_str(), "joined by session");
    }
    let first_hashes = requests[0]["data"]
        .get("prefix_hashes")
        .expect("hashes are set on the first turn");
    let second_hashes = requests[1]["data"]
        .get("prefix_hashes")
        .expect("hashes are set on the second turn");
    assert_eq!(
        first_hashes["system"], second_hashes["system"],
        "the unchanged system prompt hashes the same"
    );
    assert_eq!(
        first_hashes["tools"], second_hashes["tools"],
        "the unchanged tools hash the same"
    );
    let first_messages = first_hashes["messages"]
        .as_array()
        .expect("messages hash on the first turn");
    let second_messages = second_hashes["messages"]
        .as_array()
        .expect("messages hash on the second turn");
    assert!(
        second_messages.len() > first_messages.len(),
        "the second turn appends messages: {second_messages:?} over {first_messages:?}"
    );
    assert_eq!(
        &second_messages[..first_messages.len()],
        first_messages.as_slice(),
        "the second turn keeps the first turn's prefix"
    );
    for line in &requests {
        let wire = serde_json::to_string(line).unwrap();
        for sentinel in ["hi", "again", "Hello.", "sk-test"] {
            assert!(
                !wire.contains(sentinel),
                "no content in the line: {sentinel}"
            );
        }
    }
}

#[test]
fn a_changed_system_prompt_shifts_the_system_digest() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    let first = ask_debug_diagnostics(&setup, &server, json!({}));
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let first_line = the_request(&setup, &first);
    let first_system = first_line["data"]["prefix_hashes"]["system"].clone();
    assert!(hex64(&first_system), "the system segment hashes");
    fs::write(setup.home().join("APPEND_SYSTEM.md"), "Be terse.\n").unwrap();
    let second = ask(&setup);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "both runs arrived"
    );
    let second_line = the_request(&setup, &second);
    let second_system = &second_line["data"]["prefix_hashes"]["system"];
    assert!(hex64(second_system), "the system segment hashes");
    assert_ne!(
        &first_system, second_system,
        "the changed system prompt shifts the first hash"
    );
    for line in [&first_line, &second_line] {
        let wire = serde_json::to_string(line).unwrap();
        for sentinel in ["hi", "Hello.", "sk-test", "Be terse."] {
            assert!(
                !wire.contains(sentinel),
                "no content in the line: {sentinel}"
            );
        }
    }
}

#[test]
fn each_protocol_writes_one_diagnostics_line() {
    for protocol in [
        "openai-responses",
        "openai-completions",
        "anthropic-messages",
        "google-generative-ai",
    ] {
        let setup = Setup::new();
        let server = ProviderServer::start([protocol_hello(protocol)]).unwrap();
        if protocol == "openai-responses" {
            setup.provider(&server);
            debug_config(&setup, json!({"cache": {"diagnostics": true}}));
        } else {
            install_protocol(&setup, protocol, &server);
            debug_config(&setup, json!({"cache": {"diagnostics": true}}));
        }
        let run = ask(&setup);
        assert_eq!(run.code, Some(0), "stderr on {protocol}: {}", run.stderr);
        assert!(
            server.await_requests(1, setup.deadline),
            "the model request arrived on {protocol}"
        );
        let line = the_request(&setup, &run);
        let data = &line["data"];
        assert_eq!(data["status"], 200, "the status on {protocol}");
        let hashes = data.get("prefix_hashes").expect("hashes are set");
        let messages = hashes["messages"].as_array().expect("messages hash");
        assert!(
            !messages.is_empty(),
            "the conversation hashes on {protocol}"
        );
        for digest in messages {
            assert!(hex64(digest), "each message boundary hashes on {protocol}");
        }
        assert!(
            data.get("cache_read").is_some(),
            "the reported read on {protocol}"
        );
        assert!(
            data.get("cache_write").is_some(),
            "the reported write on {protocol}"
        );
        let wire = serde_json::to_string(&line).unwrap();
        for sentinel in ["hi", "Hello.", "sk-test"] {
            assert!(
                !wire.contains(sentinel),
                "no content in the line on {protocol}: {sentinel}"
            );
        }
    }
}

/// A debug turn grows the session's `events.jsonl`: the file the session
/// writes, never filesystem free space, measured before and after the
/// turn's own completion.
#[test]
fn a_debug_turn_grows_the_session_events_file() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    let run = ask_debug(&setup, &server, json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let events = support::session_dir(&setup, run.session_id()).join("events.jsonl");
    let after = fs::metadata(&events).unwrap().len();
    assert!(
        after > 0,
        "the turn wrote and fsynced the session log: {}",
        events.display()
    );
    let lines = diagnostic_file(&setup, &format!("ask-{}.log", run.session_id()));
    let peaks: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "resources")
        .collect();
    assert_eq!(peaks.len(), 2, "one turn and one orderly exit");
    for peak in &peaks {
        for field in [
            "peak_kib",
            "current_kib",
            "disk_read_bytes",
            "disk_write_bytes",
        ] {
            assert!(
                peak["data"][field].is_u64(),
                "all four fields present: {peak}"
            );
        }
    }
}
