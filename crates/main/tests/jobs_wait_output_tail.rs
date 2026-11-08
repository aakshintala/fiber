//! A real background shell completion carries its output tail through `jobs wait`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use fakes::{ProviderServer, Request};
use serde_json::{Value, json};
use support::{Setup, StdoutRun, function_call, hello, run_to_exit, stream};

/// Returns a tool result's text from its completed call.
fn tool_result(lines: &[Value], name: &str) -> String {
    let call = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested" && line["payload"]["name"] == name)
        .unwrap_or_else(|| {
            let kinds_and_names: Vec<_> = lines
                .iter()
                .map(|line| {
                    (
                        line["kind"].as_str().unwrap_or("?"),
                        line["payload"]["name"].as_str().unwrap_or(""),
                    )
                })
                .collect();
            panic!("tool call {name:?} was not requested; kinds/names: {kinds_and_names:?}");
        });
    let action = &call["action_id"];
    lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == *action)
        .expect("tool call completed")["payload"]["content"][0]["text"]
        .as_str()
        .expect("text result")
        .to_owned()
}

/// Parses the production receipt prefix and returns its job id.
fn receipt_id(output: &str) -> &str {
    let rest = output
        .strip_prefix("Started in the background.\nJob ")
        .expect("production background receipt prefix");
    rest.split_once('.')
        .expect("receipt job id ends before the period")
        .0
}

/// A completion writer in `background::to_completed_reported` reads this
/// shell's real output file; the next production `jobs wait` reads that
/// stored completion from the registry.
#[test]
fn a_real_background_completion_returns_its_output_tail_from_jobs_wait() {
    let setup = Setup::new();
    let command = "printf 'one\\ntwo\\nthree\\n'";
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let recorded = Arc::clone(&requests);
    let provider = ProviderServer::start_responding(move |request: &Request| {
        let body: Value = serde_json::from_slice(&request.body).expect("provider request JSON");
        let input = body["input"].as_array().expect("provider input array");
        recorded.lock().unwrap().push(body.clone());

        let background_output = input.iter().find(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_background"
        });
        let Some(background_output) = background_output else {
            return stream(&[function_call(
                "call_background",
                "shell",
                &json!({"command": command, "run_in_background": true}),
            )]);
        };

        let waited = input
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == "call_wait");
        if waited {
            return hello();
        }

        let output = background_output["output"]
            .as_str()
            .expect("shell background receipt output");
        let id = receipt_id(output);
        stream(&[function_call(
            "call_wait",
            "jobs",
            &json!({"action": "wait", "job_id": id, "timeout_ms": 30000}),
        )])
    })
    .unwrap();
    setup.provider(&provider);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": command})
        ),
    )
    .unwrap();

    let output = run_to_exit(
        setup.deadline,
        "fiber ask",
        setup.fiber(&["ask", "start the background command and wait for it"]),
    );
    let run = StdoutRun::from(output);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.lines.iter().any(|line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "shell"
        }),
        "shell tool call was not requested"
    );
    assert!(
        run.lines.iter().any(|line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "jobs"
        }),
        "jobs tool call was not requested; decoded provider request: {:#}",
        requests
            .lock()
            .unwrap()
            .last()
            .expect("at least one provider request")
    );
    let waited = tool_result(&run.lines, "jobs");
    assert!(waited.contains("Job "), "{waited}");
    assert!(waited.contains("Last output:\none\ntwo\nthree"), "{waited}");
}
