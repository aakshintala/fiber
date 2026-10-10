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
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{
    HubProc, SessionGuard, Setup, connect_hub, hello, recv_reply, run_to_exit, subscribe, until,
    write_json,
};

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

/// Installs `fiber.test/<short>` with entry script `init`.
fn install_lua(setup: &Setup, short: &str, init: &str) {
    let source = setup.root.path().join(format!("ext-{short}"));
    std::fs::create_dir_all(&source).unwrap();
    write_json(
        &source.join("extension.json"),
        &json!({"name": format!("fiber.test/{short}"), "version": "v2.0.0",
            "fiber": "0.1.0", "api": 1}),
    );
    std::fs::write(source.join("init.lua"), init).unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
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

/// The resumed form of [`TEXT_TURN`]: no `session_started` and no
/// `opening_message`.
const RESUMED_TURN: [&str; 11] = [
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The `sessions` directory of the workspace's project.
fn sessions(setup: &Setup) -> std::path::PathBuf {
    let workspace = std::fs::canonicalize(setup.workspace()).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    setup.home().join("projects").join(key).join("sessions")
}

/// The session `run` started.
fn session_id(run: &Run) -> &str {
    run.of("session_started")[0]["session_id"].as_str().unwrap()
}

/// The session's `events.jsonl` lines.
fn log_lines(setup: &Setup, id: &str) -> Vec<Value> {
    std::fs::read_to_string(sessions(setup).join(id).join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The kinds of the session's `events.jsonl`, in order.
fn log_kinds(setup: &Setup, id: &str) -> Vec<String> {
    log_lines(setup, id)
        .iter()
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// The event kinds of socket `lines`, in order, without `session_status`
/// or `attention`: an observer thread writes `session_status`, and the
/// hub's `attention` line derives from it, so neither's presence or
/// position is pinned.
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status" && line["kind"] != "attention")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// [`kinds`] without `clients`, after checking `lines` hold exactly one.
/// The hub sends a `start`'s first prompt once the requester's `full`
/// `subscribe` is accepted or 1 second after its answer, whichever comes
/// first (`docs/invocation.md`, "What the hub speaks"), so on a loaded
/// machine the ephemeral `clients` line can fall after the loop's first
/// lines: only its presence is pinned, not its position.
fn kinds_with_one_clients_line(lines: &[Value]) -> Vec<&str> {
    let clients = lines
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(clients, 1, "{lines:?}");
    kinds(lines)
        .into_iter()
        .filter(|kind| *kind != "clients")
        .collect()
}

/// A two-step script answering one line of text per request.
fn two_steps() -> Value {
    json!({"steps": [{"text": "One."}, {"text": "Two."}]})
}

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

/// A read-only `sed` with stderr discarded takes the permission fast path:
/// no reviewer call, so every usage names the session script and no
/// `permission_resolved` is decided by a reviewer.
#[test]
fn a_sed_read_with_stderr_discarded_takes_the_fast_path_with_no_reviewer_call() {
    let setup = Setup::new();
    let notes: String = (1..=7).map(|n| format!("line {n}\n")).collect();
    std::fs::write(setup.workspace().join("notes.md"), notes).unwrap();
    script(
        &setup,
        "s.json",
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": "sed -n '1,5p' notes.md 2>/dev/null"}}]},
            {"text": "Done."}
        ]}),
    );
    script(&setup, "r.json", &json!({"steps": []}));
    reviewed_by_script(&setup, json!({}));
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let completed = run.of("tool_call_completed");
    assert_eq!(completed.len(), 1);
    let payload = completed[0].to_string();
    assert!(payload.contains("line 5"), "{payload}");
    assert!(!payload.contains("line 6"), "{payload}");
    let models: Vec<&str> = run
        .of("usage_recorded")
        .iter()
        .map(|line| line["payload"]["model"].as_str().unwrap())
        .collect();
    assert_eq!(models, ["scripted/s.json", "scripted/s.json"]);
    for resolved in run.of("permission_resolved") {
        assert_ne!(resolved["payload"]["decided_by"], "reviewer", "{resolved}");
    }
    let durable = run.durable();
    assert!(!durable.contains(&"permission_requested"), "{durable:?}");
    assert!(!durable.contains(&"permission_resolved"), "{durable:?}");
    assert_eq!(
        durable,
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
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(
        run.of("text_completed").last().unwrap()["payload"]["text"],
        "Done."
    );
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

#[test]
fn a_scripted_session_records_no_credential_label() {
    let setup = Setup::new();
    script(&setup, "s.json", &json!({"steps": [{"text": "Hi."}]}));
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.durable(), TEXT_TURN);
    let built = run.of("preamble_built");
    assert_eq!(built.len(), 1, "{:?}", run.durable());
    assert!(
        built[0].get("payload").unwrap().get("credential").is_none(),
        "{}",
        built[0]
    );
}

#[test]
fn a_scripted_session_ignores_a_configured_label() {
    let setup = Setup::new();
    script(&setup, "s.json", &json!({"steps": [{"text": "Hi."}]}));
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "providers": {"scripted": {"credential": "work"}}}),
    );
    let run = fiber(&setup, &["ask", "hi"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.durable(), TEXT_TURN);
    let built = run.of("preamble_built");
    assert_eq!(built.len(), 1, "{:?}", run.durable());
    assert!(
        built[0].get("payload").unwrap().get("credential").is_none(),
        "{}",
        built[0]
    );
}

#[test]
fn a_scripted_resume_with_a_label_fails_credential_missing_before_the_session() {
    let setup = Setup::new();
    script(&setup, "s.json", &two_steps());
    let first = fiber(&setup, &["ask", "--model", "scripted/s.json", "one"]);
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    assert_eq!(first.durable(), TEXT_TURN);
    let id = session_id(&first).to_owned();
    let events = sessions(&setup).join(&id).join("events.jsonl");
    let before = std::fs::read(&events).unwrap();

    let run = fiber(
        &setup,
        &["ask", "--resume", &id, "--credential", "x", "two"],
    );
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(run.lines.len(), 1, "{:?}", run.lines);
    assert_eq!(
        run.lines
            .iter()
            .map(|line| line["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["fiber_exited"]
    );
    let error = &run.last()["payload"]["error"];
    assert_eq!(error["code"], "credential_missing");
    let message = error["message"].as_str().unwrap();
    assert!(message.ends_with("are: none"), "{message}");
    assert_eq!(std::fs::read(&events).unwrap(), before);
}

#[test]
fn a_scripted_resume_without_a_label_records_none() {
    let setup = Setup::new();
    script(&setup, "s.json", &two_steps());
    let first = fiber(&setup, &["ask", "--model", "scripted/s.json", "one"]);
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let id = session_id(&first).to_owned();

    let second = fiber(&setup, &["ask", "--resume", &id, "two"]);
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    assert_eq!(second.durable(), RESUMED_TURN);
    let built = second.of("preamble_built");
    assert_eq!(built.len(), 1, "{:?}", second.durable());
    assert!(
        built[0].get("payload").unwrap().get("credential").is_none(),
        "{}",
        built[0]
    );
}

#[test]
fn a_scripted_log_with_a_recorded_label_still_resumes() {
    let setup = Setup::new();
    script(&setup, "s.json", &two_steps());
    let first = fiber(&setup, &["ask", "--model", "scripted/s.json", "one"]);
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let id = session_id(&first).to_owned();
    let events = sessions(&setup).join(&id).join("events.jsonl");
    let text = std::fs::read_to_string(&events).unwrap();
    let rewritten: Vec<String> = text
        .lines()
        .map(|line| {
            let mut value: Value = serde_json::from_str(line).unwrap();
            if value["kind"] == "preamble_built" {
                value["payload"]["credential"] = json!("default");
            }
            serde_json::to_string(&value).unwrap()
        })
        .collect();
    std::fs::write(&events, rewritten.join("\n") + "\n").unwrap();

    let second = fiber(&setup, &["ask", "--resume", &id, "two"]);
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    let mut expected = vec!["fiber_started", "extensions_loaded", "model_changed"];
    expected.extend(RESUMED_TURN[2..].iter().copied());
    assert_eq!(second.durable(), expected);
    let built = second.of("preamble_built");
    assert_eq!(built.len(), 1, "{:?}", second.durable());
    assert!(
        built[0].get("payload").unwrap().get("credential").is_none(),
        "{}",
        built[0]
    );
    let changed = second.of("model_changed");
    assert_eq!(changed.len(), 1, "{:?}", second.durable());
    assert_eq!(changed[0]["payload"]["before"]["credential"], "default");
    assert!(
        changed[0]["payload"]["after"].get("credential").is_none(),
        "{}",
        changed[0]
    );
}

/// The hub stream of a scripted start turn through `turn_completed`: the
/// full ordered list without any retry lines and without `clients`, which
/// [`kinds_with_one_clients_line`] checks.
const SCRIPTED_START_KINDS: [&str; 13] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

#[test]
fn the_credential_command_on_a_scripted_session_is_rejected() {
    let setup = Setup::new();
    script(&setup, "s.json", &two_steps());
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json"}),
    );
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    client.send(
        &json!({
            "id": "c_start",
            "command": "start",
            "args": {"workspace": workspace, "content": [{"type": "text", "text": "hi"}]},
        })
        .to_string(),
    );
    let started = recv_reply(&client, "the start acknowledgement");
    assert_eq!(started["kind"], "command_accepted", "{started}");
    let id = started["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    subscribe(&client, &id);
    let mut stream = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert_eq!(kinds_with_one_clients_line(&stream), SCRIPTED_START_KINDS);

    client.send(
        &json!({"id": "c_cred", "session_id": id, "command": "credential", "args": {"label": "x"}})
            .to_string(),
    );
    // Stop on any reply to `c_cred`, then assert it is the rejection:
    // in red the reply is `command_accepted`, and failing on it at once
    // keeps no run waiting the shared budget for a rejection that never
    // arrives.
    let waited = until(&client, "the credential rejection", |line| {
        (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
            && line["payload"]["command_id"] == "c_cred"
    });
    // `session_status` is an observer-thread line, and `attention` derives
    // from it; neither is pinned (as `kinds` filters them): drop them, then
    // exactly the reply remains.
    let rejected: Vec<Value> = waited
        .into_iter()
        .filter(|line| line["kind"] != "session_status" && line["kind"] != "attention")
        .collect();
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert_eq!(rejected[0]["kind"], "command_rejected", "{rejected:?}");
    assert_eq!(rejected[0]["payload"]["code"], "credential_missing");
    let message = rejected[0]["payload"]["message"].as_str().unwrap();
    assert!(message.ends_with("are: none"), "{message}");
    stream.extend(rejected);

    client.send(&json!({"id": "c_close", "session_id": id, "command": "close"}).to_string());
    let tail = until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    stream.extend(tail);
    let mut expected: Vec<&str> = SCRIPTED_START_KINDS.to_vec();
    expected.push("command_rejected");
    expected.extend(["command_accepted", "fiber_exited"]);
    assert_eq!(kinds_with_one_clients_line(&stream), expected);

    guard.wait_gone();
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
    let kinds = log_kinds(&setup, &id);
    assert_eq!(
        kinds.iter().map(String::as_str).collect::<Vec<_>>(),
        TEXT_TURN
    );
}

#[test]
fn a_live_scripted_resume_with_a_label_is_rejected() {
    let setup = Setup::new();
    script(&setup, "s.json", &two_steps());
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json"}),
    );
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    client.send(
        &json!({
            "id": "c_start",
            "command": "start",
            "args": {"workspace": workspace, "content": [{"type": "text", "text": "hi"}]},
        })
        .to_string(),
    );
    let started = recv_reply(&client, "the start acknowledgement");
    assert_eq!(started["kind"], "command_accepted", "{started}");
    let id = started["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    subscribe(&client, &id);
    let stream = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert_eq!(kinds_with_one_clients_line(&stream), SCRIPTED_START_KINDS);

    let run = fiber(&setup, &["ask", "--resume", &id, "--credential", "x", "hi"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(run.lines.len(), 1, "{:?}", run.lines);
    assert_eq!(run.lines[0]["kind"], "fiber_exited");
    let error = &run.lines[0]["payload"]["error"];
    assert_eq!(error["code"], "credential_missing");
    let message = error["message"].as_str().unwrap();
    assert!(message.ends_with("are: none"), "{message}");

    client.send(&json!({"id": "c_close", "session_id": id, "command": "close"}).to_string());
    let _tail = until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    guard.wait_gone();
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
    let kinds = log_kinds(&setup, &id);
    assert_eq!(
        kinds.iter().map(String::as_str).collect::<Vec<_>>(),
        TEXT_TURN
    );
}

/// A reviewed shell call that reaches stage 2, then an automatic handoff,
/// with an extension loaded throughout: the saved log holds exactly three
/// reviewer `usage_recorded` lines, and no other line carries `reviewer`.
/// An extension's own model call needs `host.model`, which this build does
/// not offer yet, so no extension usage line can exist; the extension still
/// loads and its hook runs on the reviewed call, and the exclusion is
/// asserted over every usage line in the log.
#[test]
fn a_reviewed_call_then_a_handoff_marks_only_the_reviewer_lines() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "tag",
        "fiber.hook(\"after_tool\", { timeout = 10000, on_failure = \"non-blocking\",\n\
           run = function(call) host.log(\"seen\") end })\n",
    );
    script(
        &setup,
        "s.json",
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": "touch made && echo ran"}}],
             "usage": {"input": 100, "output": 10}},
            {"text": "Handing off."},
            {"text": "Done."}
        ]}),
    );
    script(
        &setup,
        "r.json",
        &json!({"steps": [
            {"text": "check"},
            {"text": "allow: fine"},
            {"text": "1"}
        ]}),
    );
    reviewed_by_script(
        &setup,
        json!({"model": "scripted/s.json", "handoff": {"tokens": 5}}),
    );
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    // The stage-2 allow ran the call, and the extension's hook saw it.
    assert!(setup.workspace().join("made").exists());
    let seen: Vec<&Value> = run
        .of("extension_log")
        .into_iter()
        .filter(|line| line["payload"]["extension"] == "fiber.test/tag")
        .collect();
    assert_eq!(seen.len(), 1, "{:?}", run.of("extension_log"));
    assert_eq!(
        run.of("handoff_completed")[0]["payload"]["outcome"],
        "completed"
    );

    let id = session_id(&run).to_owned();
    let log = log_lines(&setup, &id);
    // The whole log in order, so a duplicated, missing or reordered
    // event fails (`docs/testing.md`, "Event streams").
    assert_eq!(
        log_kinds(&setup, &id),
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
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "reviewer_kept",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let usages: Vec<&Value> = log
        .iter()
        .filter(|line| line["kind"] == "usage_recorded")
        .collect();
    // The reviewed call, decided at stage 2.
    let resolved = log
        .iter()
        .find(|line| {
            line["kind"] == "permission_resolved" && line["payload"]["reviewer"]["stage"] == 2
        })
        .unwrap();
    assert_eq!(resolved["payload"]["decision"], "allow");
    let call = resolved.get("action_id").unwrap().clone();
    // Exactly the reviewer's three calls carry `reviewer`.
    let review: Vec<&&Value> = usages
        .iter()
        .filter(|line| line["payload"]["model"] == "scripted/r.json")
        .collect();
    assert_eq!(review.len(), 3);
    let stage = |purpose: &str| {
        review
            .iter()
            .find(|line| line["payload"]["reviewer"]["purpose"] == purpose)
            .unwrap()
    };
    for purpose in ["stage_1", "stage_2"] {
        let line = stage(purpose);
        assert_eq!(
            line["payload"]["reviewer"],
            json!({"purpose": purpose, "action_id": call}),
            "{line}"
        );
        assert!(line.get("action_id").is_none(), "{line}");
    }
    let handoff = stage("handoff");
    assert_eq!(
        handoff["payload"]["reviewer"],
        json!({"purpose": "handoff"})
    );
    assert!(handoff.get("action_id").is_none(), "{handoff}");
    // Nothing else does: neither the session's own calls nor any other
    // line in the log.
    for line in &usages {
        if line["payload"]["model"] != "scripted/r.json" {
            assert!(line["payload"].get("reviewer").is_none(), "{line}");
        }
    }
}
