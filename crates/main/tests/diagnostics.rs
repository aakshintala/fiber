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

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{HELLO_KINDS, Run, Setup, connect_hub, hello, is_status, run_to_exit, write_json};

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
