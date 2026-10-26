//! `diagnostics.level` at the binary level (`docs/state.md`, "Diagnostic
//! logs"): the built `fiber` runs `ask` against the fake provider server.
//! With the default nothing debug is written; a repository's value is
//! ignored with a `config_key_ignored` notice; a debug hub writes a
//! positive `peak_memory` through the Part 1 path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::{
    HELLO_KINDS, Run, Setup, connect_hub, function_call, hello, is_status, run_to_exit, stream,
    write_json,
};

/// Runs `fiber ask hi` in the workspace to exit and parses its stdout.
#[track_caller]
fn ask(setup: &Setup) -> Run {
    let mut command = setup.fiber(&["ask", "hi"]);
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

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    for entry in fs::read_dir(&logs).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

#[test]
fn at_default_no_debug_line_is_written() {
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
    assert!(!server.requests().is_empty());
    let lines = log_lines(&setup);
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "{lines:?}"
    );
}

#[test]
fn a_repository_diagnostics_level_is_ignored_with_a_notice() {
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
    assert!(run.kinds().contains(&"fiber_exited"), "{:?}", run.kinds());
    let mut want: Vec<&str> = HELLO_KINDS.into();
    want.insert(3, "notice");
    assert_eq!(
        run.kinds(),
        want,
        "the notice lands after extensions_loaded"
    );
    let notice = run
        .all("notice")
        .into_iter()
        .find(|notice| notice["payload"]["code"] == "config_key_ignored")
        .expect("one config_key_ignored notice");
    let message = notice["payload"]["message"].as_str().unwrap();
    assert!(message.contains("diagnostics.level"), "{message}");
    assert!(message.contains(".fiber/config.json"), "{message}");
    let lines = log_lines(&setup);
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "the repository value stays ignored: {lines:?}"
    );
}

/// Every `op_timing` line under the home's `logs/`, parsed.
fn op_lines(setup: &Setup) -> Vec<Value> {
    log_lines(setup)
        .into_iter()
        .filter(|line| line["code"] == "op_timing")
        .collect()
}

/// A foreground `shell` call saying hi, as `loop_timing` answers it: the
/// journey's first tool call.
fn shell_echo() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call", "id": "fc_1", "call_id": "call_1",
        "name": "shell", "arguments": "{\"command\":\"echo hi\"}"}})])
}

/// A `handoff` call with a note, as `loop_timing` answers it: the
/// journey's second tool call.
fn handoff() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call", "id": "fc_2", "call_id": "call_2",
        "name": "handoff", "arguments": "{\"note\":\"Continue with the tests.\"}"}})])
}

/// A background `shell` call carrying `sentinel`, as `jobs_timing`
/// starts it: the journey's third tool call.
fn background(command: &str) -> Response {
    stream(&[function_call(
        "call_shell",
        "shell",
        &json!({"command": command, "run_in_background": true}),
    )])
}

/// An `mcp__fx__echo` call, as `mcp_timing` answers it: the journey's
/// fourth tool call, once the background receipt lands.
fn echo() -> Response {
    stream(&[function_call("c1", "mcp__fx__echo", &json!({}))])
}

/// The journey's answers from the conversation's state, as `mcp_timing`
/// answers them: each request advances one call, and the answered turn
/// ends the run. The checks read the `input` items, never the raw body,
/// so the tool definitions elsewhere in the request cannot match.
fn journey(sentinel: String) -> ProviderServer {
    ProviderServer::start_responding(move |request| {
        let text = String::from_utf8_lossy(&request.body);
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let items: &[Value] = body["input"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let called = |name: &str| {
            items
                .iter()
                .any(|item| item["type"] == "function_call" && item["name"] == name)
        };
        // The handoff rewinds the conversation: the handed-off step's
        // input holds the note, never the calls before it. The note
        // therefore marks the journey's second half.
        if !text.contains("Continue with the tests.") {
            if !called("shell") {
                return shell_echo();
            }
            return handoff();
        }
        if !text.contains("Started in the background") {
            return background(&format!("echo {sentinel}"));
        }
        if !called("mcp__fx__echo") {
            return echo();
        }
        hello()
    })
    .unwrap()
}

/// A pass-through hook for `event`: every actual dispatch exists, and
/// none changes the call. The shape mirrors `tool_timing`'s pass hooks:
/// pass-through returning `{}`, `timeout = 30000`.
fn hook(event: &str) -> String {
    format!(
        "fiber.hook(\"{event}\", {{ timeout = 30000, on_failure = \"blocking\",\\n\\
           run = function(call) return {{}} end }})\\n"
    )
}

/// One read-only offline MCP tool, as `mcp_timing` declares it, so its
/// calls take the permission fast path.
fn echo_tool() -> Value {
    json!({
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

/// The MCP fixture directory holding the echo tool, as `mcp_timing`
/// serves it, with a sentinel env value no timing line may ever carry.
fn mcp_fixture(setup: &Setup) -> Value {
    let dir = setup.root.path().join("fx");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tools.json"), json!([echo_tool()]).to_string()).unwrap();
    fs::write(
        dir.join("call-echo.json"),
        r#"{"content":[{"type":"text","text":"hi"}]}"#,
    )
    .unwrap();
    json!({
        "command": "/bin/bash",
        "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
        "env": {"MCP_TIMING_SENTINEL": "mcp-timing-sentinel"},
        "startup_timeout_ms": 600000,
    })
}

/// The journey's home configuration: the fake model, the MCP server,
/// debug diagnostics, and a session that idles long past the background
/// call, as `jobs_timing` holds it. No reviewer: every call is allowed
/// or read-only, so no review request can interleave with the journey.
fn journey_config(setup: &Setup, servers: Value) {
    let mut config = json!({"model": "fake/m"});
    config["diagnostics"] = json!({"level": "debug"});
    config["mcp"] = json!({"servers": servers});
    config["session"] = json!({"idle_exit_ms": 3600000});
    write_json(&setup.home().join("config.json"), &config);
}

/// Runs `fiber ask` with `prompt` in the workspace to exit and parses
/// its stdout, as [`ask`] does.
fn ask_prompt(setup: &Setup, prompt: &str) -> Run {
    let mut command = setup.fiber(&["ask", prompt]);
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

/// At `debug` one `ask` run exercising a tool call, a handoff, one MCP
/// server and one background job writes every session-core timer: each
/// listed operation has at least one line, and no `op_timing` line
/// carries a field outside `op`, `ms` and `tool`.
#[test]
fn a_debug_journey_times_the_session_core_matrix() {
    let setup = Setup::new();
    let sentinel = "sentinel-matrix-7q2x";
    let server = journey(sentinel.to_owned());
    setup.provider(&server);
    setup.lua("cov_before", &hook("before_tool"));
    setup.lua(
        "cov_check",
        "fiber.hook(\"before_tool\", { phase = \"check\", timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return {} end })\n",
    );
    setup.lua("cov_after", &hook("after_tool"));
    setup.lua("cov_start", &hook("turn_start"));
    journey_config(&setup, json!({"fx": mcp_fixture(&setup)}));
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": ""})
        ),
    )
    .unwrap();
    let run = ask_prompt(&setup, "please proceed");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(5, setup.deadline),
        "the journey asked five times: a passing test really called"
    );
    let lines = op_lines(&setup);
    for op in [
        "startup.config",
        "startup.extensions",
        "startup.preamble",
        "startup.routing",
        "turn.request",
        "turn.model_call",
        "turn.handoff",
        "tool.permission",
        "tool.hooks",
        "tool.run",
        "mcp.connect",
        "jobs.start",
        "hook.turn_start",
        "log.append",
        "log.fsync",
    ] {
        assert!(
            lines.iter().any(|line| line["data"]["op"] == op),
            "one {op} line"
        );
    }
    assert!(
        log_lines(&setup)
            .iter()
            .any(|line| line["code"] == "provider_request"),
        "one provider_request line"
    );
    assert!(
        log_lines(&setup)
            .iter()
            .any(|line| line["code"] == "peak_memory"),
        "one peak_memory line"
    );
    for line in &lines {
        let data = line["data"].as_object().expect("data is an object");
        for key in data.keys() {
            assert!(
                ["ms", "op", "tool"].contains(&key.as_str()),
                "no field outside op, ms and tool: {line}"
            );
        }
    }
    for line in &lines {
        let raw = line.to_string();
        for leaked in [
            "sk-test",
            "mcp-timing-sentinel",
            sentinel,
            "echo hi",
            "Continue with the tests.",
        ] {
            assert!(
                !raw.contains(leaked),
                "no credential, argument or content in {raw}"
            );
        }
    }
}

#[test]
fn a_debug_hub_writes_positive_peak_memory() {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"diagnostics": {"level": "debug"}}),
    );
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill("TERM");
    hub.wait();
    let lines = log_lines(&setup);
    let peak = lines
        .iter()
        .find(|line| line["code"] == "peak_memory")
        .expect("one peak_memory line");
    assert!(peak["data"]["peak_kib"].as_u64().unwrap() > 0, "{peak}");
}
