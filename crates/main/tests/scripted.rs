//! Binary-level tests of the built-in `scripted` provider
//! (`docs/model-routing.md`, "The scripted provider"): the built `fiber`
//! runs in its own process group with its own `FIBER_HOME`, a session names
//! `scripted/<path>`, and the script in its workspace answers its requests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::process::Output;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Setup, hello, run_to_exit, write_json};

/// One `fiber` run: its exit code, every stdout line and stderr.
struct Run {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        Self {
            code: output.status.code(),
            lines: stdout
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    /// The kinds of the durable lines, those carrying `seq`, in order.
    fn durable(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|line| line.get("seq").is_some())
            .map(|line| line["kind"].as_str().unwrap())
            .collect()
    }

    /// Every line of `kind`, in order.
    fn of(&self, kind: &str) -> Vec<&Value> {
        self.lines
            .iter()
            .filter(|line| line["kind"] == kind)
            .collect()
    }

    fn last(&self) -> &Value {
        self.lines.last().unwrap()
    }
}

/// Writes `script` as `w/<name>`.
fn script(setup: &Setup, name: &str, script: &Value) {
    write_json(&setup.workspace().join(name), script);
}

/// Writes `config.json` as `global` plus `reviewer.model` naming
/// `scripted/r.json`.
fn reviewed_by_script(setup: &Setup, mut global: Value) {
    global["reviewer"] = json!({"model": "scripted/r.json"});
    write_json(&setup.home().join("config.json"), &global);
}

/// `fiber <args>` run from the workspace to its exit.
fn fiber(setup: &Setup, args: &[&str]) -> Run {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    Run::from(run_to_exit(setup.deadline, "fiber", command))
}

/// `fiber ask --model scripted/s.json hi`.
fn ask(setup: &Setup) -> Run {
    fiber(setup, &["ask", "--model", "scripted/s.json", "hi"])
}

const TEXT_TURN: [&str; 13] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn a_scripted_text_step_answers_with_no_provider_installed() {
    let setup = Setup::new();
    script(
        &setup,
        "s.json",
        &json!({"steps": [{"text": ["Hel", "lo."], "usage": {"input": 12, "output": 2}}]}),
    );
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.durable(), TEXT_TURN);
    assert_eq!(run.of("text_completed")[0]["payload"]["text"], "Hello.");
    let usage = &run.of("usage_recorded")[0]["payload"];
    assert_eq!(usage["model"], "scripted/s.json");
    assert_eq!(usage["cost"], Value::Null);
    assert_eq!(run.of("assistant_message_delta").len(), 2);
}

#[test]
fn a_tool_call_step_runs_the_tool_and_the_next_request_takes_the_next_step() {
    let setup = Setup::new();
    std::fs::write(setup.workspace().join("notes.md"), "The note.\n").unwrap();
    script(
        &setup,
        "s.json",
        &json!({"steps": [
            {"tool_calls": [{"name": "read", "arguments": {"path": "notes.md"}}]},
            {"text": "Done."}
        ]}),
    );
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let completed = run.of("tool_call_completed");
    assert_eq!(completed.len(), 1);
    assert!(
        completed[0].to_string().contains("The note."),
        "{}",
        completed[0]
    );
    assert_eq!(run.of("usage_recorded").len(), 2);
    assert_eq!(run.of("text_completed")[0]["payload"]["text"], "Done.");
}

#[test]
fn an_exhausted_script_fails_the_turn_invalid_request() {
    let setup = Setup::new();
    script(
        &setup,
        "s.json",
        &json!({"steps": [{"tool_calls": [{"name": "read", "arguments": {"path": "x.md"}}]}]}),
    );
    let run = ask(&setup);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    let error = &run.last()["payload"]["error"];
    assert_eq!(error["code"], "invalid_request");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("s.json"), "{message}");
    assert!(message.contains("request 2"), "{message}");
}

#[test]
fn a_missing_script_fails_before_any_session() {
    let setup = Setup::new();
    let run = ask(&setup);
    assert_eq!(run.code, Some(1));
    assert_eq!(run.lines.len(), 1, "{:?}", run.lines);
    let error = &run.last()["payload"]["error"];
    assert_eq!(error["code"], "io_failed");
    assert!(error["message"].as_str().unwrap().contains("s.json"));
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_malformed_script_fails_config_invalid_naming_the_step() {
    let setup = Setup::new();
    script(
        &setup,
        "s.json",
        &json!({"steps": [{"text": "a"}, {"text": "b", "colour": "red"}]}),
    );
    let run = ask(&setup);
    assert_eq!(run.code, Some(1));
    assert_eq!(run.lines.len(), 1, "{:?}", run.lines);
    let error = &run.last()["payload"]["error"];
    assert_eq!(error["code"], "config_invalid");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("step 2"), "{message}");
    assert!(message.contains("`colour`"), "{message}");
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn fiber_models_never_lists_the_scripted_provider() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    script(&setup, "s.json", &json!({"steps": [{"text": "a"}]}));
    let run = fiber(&setup, &["models", "--json"]);
    let listed = run
        .lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(listed.contains("fake/m"), "{listed}\n{}", run.stderr);
    assert!(!listed.contains("scripted"), "{listed}");
}

/// A `shell` call goes to the reviewer (`docs/permissions.md`, "Fast
/// paths"), whose own script answers it with its own steps.
#[test]
fn a_scripted_reviewer_answers_from_its_own_script() {
    let setup = Setup::new();
    script(
        &setup,
        "s.json",
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": "touch made && echo ran"}}]},
            {"text": "Done."}
        ]}),
    );
    script(&setup, "r.json", &json!({"steps": [{"text": "allow"}]}));
    reviewed_by_script(&setup, json!({}));
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(setup.workspace().join("made").exists());
    let models: Vec<&str> = run
        .of("usage_recorded")
        .iter()
        .map(|line| line["payload"]["model"].as_str().unwrap())
        .collect();
    assert_eq!(
        models,
        ["scripted/s.json", "scripted/r.json", "scripted/s.json"]
    );
    assert_eq!(
        run.of("text_completed").last().unwrap()["payload"]["text"],
        "Done."
    );
}

/// An exhausted reviewer script is a reviewer failure: the call escalates,
/// and with no answer possible under `fiber ask` it is refused
/// (`docs/permissions.md`, "Headless"), while the session's own steps
/// still serve its turn.
#[test]
fn an_empty_reviewer_script_refuses_the_call_and_the_session_keeps_its_steps() {
    let setup = Setup::new();
    script(
        &setup,
        "s.json",
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": "touch made && echo ran"}}]},
            {"text": "Done."}
        ]}),
    );
    script(&setup, "r.json", &json!({"steps": []}));
    reviewed_by_script(&setup, json!({}));
    let run = ask(&setup);
    let resolved = run.of("permission_resolved");
    assert_eq!(resolved.len(), 1, "{:?}", run.durable());
    let payload = &resolved[0]["payload"];
    assert_eq!(payload["decision"], "deny", "{payload}");
    assert_eq!(payload["decided_by"], "reviewer", "{payload}");
    assert_eq!(payload["reviewer"]["model"], "scripted/r.json", "{payload}");
    assert!(
        payload.to_string().contains("request 1"),
        "the reviewer's failure names its script's request: {payload}"
    );
    assert!(
        !setup.workspace().join("made").exists(),
        "the refused call ran"
    );
    assert_eq!(
        run.of("text_completed").last().unwrap()["payload"]["text"],
        "Done."
    );
    assert_eq!(run.of("turn_completed").len(), 1);
}

/// A session on an installed provider reviewed by a scripted model: startup
/// reads no credential for `scripted`, and the fake server sees only the
/// session's requests.
#[test]
fn a_vendor_session_with_a_scripted_reviewer_reads_no_scripted_credential() {
    let setup = Setup::new();
    let shell = support::stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call", "call_id": "c1", "name": "shell",
        "arguments": "{\"command\":\"touch made && echo ran\"}"
    }})]);
    let server = ProviderServer::start([shell, hello()]).unwrap();
    setup.provider(&server);
    script(&setup, "r.json", &json!({"steps": [{"text": "allow"}]}));
    reviewed_by_script(&setup, json!({"model": "fake/m"}));
    let run = fiber(&setup, &["ask", "hi"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(server.requests().len(), 2);
    let models: Vec<&str> = run
        .of("usage_recorded")
        .iter()
        .map(|line| line["payload"]["model"].as_str().unwrap())
        .collect();
    assert_eq!(models, ["fake/m", "scripted/r.json", "fake/m"]);
    assert!(
        run.of("tool_call_completed")[0].to_string().contains("ran"),
        "{:?}",
        run.of("tool_call_completed")
    );
}
