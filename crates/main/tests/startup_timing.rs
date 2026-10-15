//! Startup timers at the binary level (`docs/state.md`, "Diagnostic
//! logs"): the built `fiber` runs `ask` against the fake provider server.
//! At `debug` the pre-attach writer emits `startup.config`,
//! `startup.extensions` and `startup.routing` with only `op` and `ms` in
//! the pid file; at the default level nothing debug is written.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::ProviderServer;
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

fn debug_config(setup: &Setup) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}),
    );
}

/// The log file holding `line`: every startup line is written before the
/// session attaches, so all three sit in the pid file, never the session
/// file.
#[track_caller]
fn file_holding(setup: &Setup, line: &Value) -> String {
    let logs = setup.home().join("logs");
    let mut found = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&logs).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let text = fs::read_to_string(entry.path()).unwrap();
        for raw in text.lines() {
            let value: Value = serde_json::from_str(raw).unwrap();
            if value == *line {
                found.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    assert_eq!(found.len(), 1, "one file holds {line}");
    found.into_iter().next().unwrap()
}

#[test]
fn a_debug_ask_writes_one_startup_line_per_operation_before_attach() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the model request arrived: a passing test really asked"
    );
    assert_eq!(run.kinds(), HELLO_KINDS);
    let session_id = run.session_id().to_owned();
    let startup = ["startup.config", "startup.extensions", "startup.routing"];
    for op in startup {
        assert_eq!(ops(&setup, op).len(), 1, "one {op} line");
    }
    let lines: Vec<Value> = startup
        .into_iter()
        .map(|op| ops(&setup, op).into_iter().next().unwrap())
        .collect();
    for line in &lines {
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        assert_eq!(line["process"], "ask", "{line}");
        assert!(
            line.get("session_id").is_none(),
            "written before attach: {line}"
        );
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let data = line["data"].as_object().expect("data is an object");
        assert_eq!(data.keys().collect::<Vec<_>>(), vec!["ms", "op"], "{line}");
        assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
        let raw = line.to_string();
        for sentinel in ["hi", "Hello.", "sk-test", "null"] {
            assert!(
                !raw.contains(sentinel),
                "no prompt, output, credential or null in {raw}"
            );
        }
    }
    let mut files = lines.iter().map(|line| file_holding(&setup, line));
    let first = files.next().unwrap();
    assert!(
        files.all(|file| file == first),
        "all three lines sit in one file"
    );
    let pid = first
        .strip_prefix("ask-")
        .and_then(|rest| rest.strip_suffix(".log"))
        .unwrap_or("");
    assert!(
        !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()),
        "the pid file holds them: {first}"
    );
    assert_ne!(first, format!("ask-{session_id}.log"));
}

#[test]
fn at_default_no_timing_or_debug_line_is_written() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the model request arrived: a passing test really asked"
    );
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(
        log_lines(&setup)
            .iter()
            .all(|line| line["code"] != "op_timing" && line["level"] != "debug"),
        "the default stays silent"
    );
}
