//! Native provider requests and session memory at the binary level
//! (`docs/state.md`, "Diagnostic logs"): the built `fiber` runs `ask`
//! against fake provider servers. With the default nothing debug is
//! written; a debug run writes one `provider_request` per attempt and a
//! `peak_memory` per turn and at the orderly end.

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
fn ask_debug_writes_one_provider_request_and_two_peak_memory_lines() {
    let setup = Setup::new();
    let body_len = hello().body.len();
    let server = ProviderServer::start([hello()]).unwrap();
    let run = ask_debug(&setup, &server, json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the model request arrived: a passing test really asked"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "provider_request")
        .collect();
    assert_eq!(requests.len(), 1, "one call writes one line");
    let line = requests[0];
    assert_eq!(line["level"], "debug");
    assert_eq!(line["process"], "ask");
    assert_eq!(line["session_id"], id.as_str());
    assert_eq!(line["message"], "A provider request ended.");
    let data = &line["data"];
    assert_eq!(data["provider"], "fake");
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
    let peaks: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "peak_memory")
        .collect();
    assert_eq!(peaks.len(), 2, "one turn and one orderly exit");
    for peak in &peaks {
        assert_eq!(peak["process"], "ask");
        assert_eq!(peak["session_id"], id.as_str());
        assert!(
            peak["data"]["peak_kib"].as_u64().unwrap() > 0,
            "positive on Linux and macOS: {peak}"
        );
    }
    assert_eq!(
        lines.last().unwrap()["code"],
        "peak_memory",
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
        server.await_requests(2, setup.deadline.left()),
        "both attempts arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "provider_request")
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
        server.await_requests(2, setup.deadline.left()),
        "both logical calls arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let attempts: Vec<u64> = lines
        .iter()
        .filter(|line| line["code"] == "provider_request")
        .map(|line| line["data"]["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(attempts, vec![1, 1], "each logical call starts at 1");
}

#[test]
fn at_default_no_provider_request_is_written() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the model request arrived: a passing test really asked"
    );
    let lines = log_lines(&setup);
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line["code"] != "provider_request" && line["code"] != "peak_memory"),
        "no native request or memory line at the default: {lines:?}"
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
        server.await_requests(1, setup.deadline.left()),
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
        lines
            .iter()
            .all(|line| line["code"] != "provider_request" && line["code"] != "peak_memory"),
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
        server.await_requests(1, setup.deadline.left()),
        "the model request arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "provider_request")
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
        server.await_requests(2, setup.deadline.left()),
        "both attempts arrived"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let attempts: Vec<u64> = lines
        .iter()
        .filter(|line| line["code"] == "provider_request")
        .map(|line| line["data"]["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(attempts, vec![1, 2]);
    let peaks: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "peak_memory")
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
        "peak_memory",
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
        lines.iter().all(|line| line["code"] != "provider_request"),
        "no request without a turn"
    );
    let peaks: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "peak_memory")
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
            .all(|line| line["code"] != "provider_request" && line["code"] != "peak_memory"),
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
        server.await_requests(2, setup.deadline.left()),
        "both asks arrived"
    );
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    for line in &lines {
        assert_eq!(line["process"], "ask");
        assert_eq!(line["session_id"], id.as_str());
    }
    let attempts: Vec<u64> = lines
        .iter()
        .filter(|line| line["code"] == "provider_request")
        .map(|line| line["data"]["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(attempts, vec![1, 1], "each ask run starts at 1");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["code"] == "peak_memory")
            .count(),
        4,
        "each ask run writes its turn and exit memory"
    );
    // The durable kinds never mention the diagnostic codes.
    assert!(
        second
            .kinds()
            .iter()
            .all(|kind| *kind != "provider_request" && *kind != "peak_memory"),
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
        parent.await_requests(1, setup.deadline.left())
            && child.await_requests(1, setup.deadline.left()),
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
        .filter(|line| line["code"] == "provider_request")
        .collect();
    assert_eq!(child_requests.len(), 1);
    assert_eq!(child_requests[0]["data"]["provider"], "pb");
    assert_eq!(child_requests[0]["data"]["model"], "m");
    assert!(
        child_lines.iter().any(|line| line["code"] == "peak_memory"),
        "the child writes its exit memory"
    );
    let parent_lines = diagnostic_file(&setup, &format!("ask-{parent_id}.log"));
    let parent_requests: Vec<&Value> = parent_lines
        .iter()
        .filter(|line| line["code"] == "provider_request")
        .collect();
    assert!(
        parent_requests
            .iter()
            .all(|line| line["data"]["provider"] == "pa"),
        "the parent's lines name only its provider"
    );
}
