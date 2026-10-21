//! Part 7 operation timers at the binary level (`docs/state.md`,
//! "Diagnostic logs"): the built `fiber` runs `ask` against the fake
//! provider server. At `debug` the real main-to-loop shared writer emits
//! `startup.preamble`, `turn.request`, `turn.model_call` and `turn.handoff`
//! with only `op` and `ms`; at the default nothing debug is written.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::{HELLO_KINDS, Run, Setup, hello, is_status, run_to_exit, write_json};

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

/// Every `op_timing` line, in file order.
fn op_lines(setup: &Setup) -> Vec<Value> {
    log_lines(setup)
        .into_iter()
        .filter(|line| line["code"] == "op_timing")
        .collect()
}

fn ops(setup: &Setup, op: &str) -> Vec<Value> {
    op_lines(setup)
        .into_iter()
        .filter(|line| line["data"]["op"] == op)
        .collect()
}

/// The wire promise on every Part 7 line: the ask identity, exactly the
/// keys `op` and `ms` with an unsigned `ms`, and none of the session's
/// prompt, output, tool arguments or credential anywhere in the line.
/// Part 7's four operations only: Part 8's tool timers carry `tool` as a
/// third key and are asserted in `tool_timing.rs`.
#[track_caller]
fn assert_wire(lines: &[Value], session_id: &str) {
    assert!(!lines.is_empty(), "the test proved its lines exist first");
    for line in lines {
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        assert_eq!(line["process"], "ask", "{line}");
        assert_eq!(line["session_id"], session_id, "{line}");
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let data = line["data"].as_object().expect("data is an object");
        assert_eq!(data.keys().collect::<Vec<_>>(), vec!["ms", "op"], "{line}");
        assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
        let raw = line.to_string();
        for sentinel in ["hi", "Hello.", "echo hi", "sk-test", "null"] {
            assert!(
                !raw.contains(sentinel),
                "no prompt, output, argument, credential or null in {raw}"
            );
        }
    }
}

/// Only Part 7's four operations: `startup.preamble`, `turn.request`,
/// `turn.model_call` and `turn.handoff`. Tool timers land here too once
/// Part 8 times them, under their own exact keys.
fn p7_lines(setup: &Setup) -> Vec<Value> {
    op_lines(setup)
        .into_iter()
        .filter(|line| {
            matches!(
                line["data"]["op"].as_str(),
                Some("startup.preamble" | "turn.request" | "turn.model_call" | "turn.handoff")
            )
        })
        .collect()
}

fn debug_config(setup: &Setup, extra: Value) {
    let mut config = json!({"model": "fake/m", "diagnostics": {"level": "debug"}});
    for (key, value) in extra.as_object().unwrap() {
        config[key] = value.clone();
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// An `openai-responses` stream calling `shell` with `echo hi`: read-only,
/// so the call runs without a review.
fn shell_echo_call() -> Response {
    support::stream(&[json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1",
            "name": "shell", "arguments": "{\"command\":\"echo hi\"}"}})])
}

/// An `openai-responses` stream calling the `handoff` tool with a note.
fn handoff_call() -> Response {
    support::stream(&[json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1",
            "name": "handoff", "arguments": "{\"note\":\"Continue with the tests.\"}"}})])
}

#[test]
fn a_debug_ask_times_its_preamble_requests_and_attempts() {
    let setup = Setup::new();
    let server = ProviderServer::start([shell_echo_call(), hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup, json!({}));
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "both model requests arrived: a passing test really asked twice"
    );
    let session_id = run.session_id().to_owned();
    assert_eq!(ops(&setup, "startup.preamble").len(), 1);
    assert_eq!(
        ops(&setup, "turn.request").len(),
        2,
        "one scope per constructed request"
    );
    assert_eq!(
        ops(&setup, "turn.model_call").len(),
        2,
        "one scope per admitted attempt"
    );
    assert!(ops(&setup, "turn.handoff").is_empty());
    assert_wire(&p7_lines(&setup), &session_id);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
}

#[test]
fn at_default_no_part7_line_is_written() {
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
    assert!(op_lines(&setup).is_empty());
    assert!(
        log_lines(&setup)
            .iter()
            .all(|line| line["level"] != "debug"),
        "the default stays silent"
    );
}

#[test]
fn a_repository_debug_level_stays_silent_with_its_notice() {
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
    let mut repo_expected: Vec<&str> = HELLO_KINDS.into();
    repo_expected.insert(3, "notice");
    assert_eq!(run.kinds(), repo_expected);
    assert!(
        run.all("notice")
            .into_iter()
            .any(|notice| notice["payload"]["code"] == "config_key_ignored"),
        "the repository value stays ignored"
    );
    assert!(op_lines(&setup).is_empty());
}

#[test]
fn a_debug_retry_times_one_request_and_two_attempts() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        Response::status(429, "{}").header("retry-after", "0"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    debug_config(
        &setup,
        json!({"retry": {"attempts": 1, "initial_delay_ms": 0, "max_delay_ms": 0}}),
    );
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "both attempts arrived"
    );
    let session_id = run.session_id().to_owned();
    assert_eq!(ops(&setup, "startup.preamble").len(), 1);
    assert_eq!(
        ops(&setup, "turn.request").len(),
        1,
        "one logical request constructs once"
    );
    assert_eq!(ops(&setup, "turn.model_call").len(), 2);
    assert_wire(&p7_lines(&setup), &session_id);
    let attempts: Vec<u64> = log_lines(&setup)
        .iter()
        .filter(|line| line["code"] == "provider_request")
        .filter_map(|line| line["data"]["attempt"].as_u64())
        .collect();
    assert_eq!(attempts, vec![1, 2]);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "retry_scheduled",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
}

#[test]
fn a_debug_tool_handoff_times_one_handoff_with_no_note_request() {
    let setup = Setup::new();
    let server = ProviderServer::start([handoff_call(), hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup, json!({}));
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "the tool call and the final request arrived, with no note request"
    );
    assert_eq!(server.requests().len(), 2);
    let session_id = run.session_id().to_owned();
    assert_eq!(ops(&setup, "turn.handoff").len(), 1);
    assert_wire(&p7_lines(&setup), &session_id);
    assert!(
        !run.kinds().contains(&"handoff_started"),
        "a tool source writes no started: {:?}",
        run.kinds()
    );
    assert!(
        run.kinds().contains(&"handoff_completed"),
        "{:?}",
        run.kinds()
    );
    assert!(run.kinds().contains(&"fiber_exited"), "{:?}", run.kinds());
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "handoff_completed",
            "opening_message",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
}
