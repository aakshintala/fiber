//! The `model` driver command through the session socket
//! (`docs/testing.md`, "Levels"): switching model or thinking level at the
//! next turn boundary, writing `model_changed` then `preamble_built`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::*;

/// Installs a provider `fake` with `m` and `n` on `openai-responses` at
/// the fake server, and makes `fake/m` the configured model. `n` takes
/// `low` and `high`, defaulting to `low`.
fn install_switch_provider(setup: &Setup, server: &ProviderServer) {
    install_models(
        setup,
        &json!([
            {"id": "m", "protocol": "openai-responses",
             "base_url": format!("{}/v1", server.url()), "context_window": 100000},
            {"id": "n", "protocol": "openai-responses",
             "base_url": format!("{}/v1", server.url()), "context_window": 100000,
             "thinking_levels": ["low", "high"], "thinking_default": "low"},
        ]),
        "fake/m",
    );
}

/// Installs a provider `fake` with `m` on `openai-responses` and `w` on
/// `anthropic-messages` with the hosted search `web_search_20250305`, both
/// at the fake server, and makes `configured` the configured model.
fn install_hosted_provider(setup: &Setup, server: &ProviderServer, configured: &str) {
    install_models(
        setup,
        &json!([
            {"id": "m", "protocol": "openai-responses",
             "base_url": format!("{}/v1", server.url()), "context_window": 100000},
            {"id": "w", "protocol": "anthropic-messages",
             "base_url": format!("{}/v1", server.url()), "context_window": 100000,
             "web_search": "web_search_20250305"},
        ]),
        configured,
    );
}

/// Installs a provider `fake` whose models are `models`, reading its key
/// from `FIBER_TEST_FAKE_KEY`, and makes `configured` the configured model.
fn install_models(setup: &Setup, models: &Value, configured: &str) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": models,
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
        &json!({"model": configured}),
    );
}

/// The session's directory, from its id.
fn session_dir(setup: &Setup, id: &str) -> PathBuf {
    log::sessions_dir(&setup.home(), &doors::project(&setup.workspace())).join(id)
}

/// Starts `fiber session --id <id> --workspace <workspace>` with `extra`
/// appended, in its own process group, its stdout drained on a thread and
/// its stderr kept for a failure.
fn start_session(setup: &Setup, id: &str, extra: &[&str]) -> Running {
    let workspace = setup.workspace();
    let mut args = vec!["session", "--id", id, "--workspace"];
    args.push(workspace.to_str().unwrap());
    args.extend(extra);
    let mut command = setup.fiber(&args);
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let stderr_pipe = child.stderr.take().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match tx.send(line.unwrap()) {
                Ok(()) => {}
                Err(mpsc::SendError(_)) => break,
            }
        }
    });
    let (err_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        match std::io::Read::read_to_string(&mut BufReader::new(stderr_pipe), &mut text) {
            Ok(_) | Err(_) => {}
        }
        match err_tx.send(text) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    Running {
        child,
        watchdog,
        group,
        guard,
        lines,
        stderr: stderr_rx,
        deadline: setup.deadline,
    }
}

/// Kills process group `group` on drop, without waiting on the kill. After
/// the child is reaped and the group is empty, [`std::mem::forget`] skips
/// that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        support::kill_group_detached(self.0, "KILL");
    }
}

/// A running `fiber session`: its stdout drained line by line, its stderr
/// kept for a failure.
struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    guard: KillGroup,
    lines: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
    deadline: Deadline,
}

impl Running {
    /// Waits under the test's [`Deadline`] for the child's first stdout line, then
    /// connects once. The socket is bound in `Session::open` before the
    /// loop writes that line, so the line is the signal the socket
    /// accepts (`docs/testing.md`, "Waits and timeouts").
    fn connect(&self, socket: &Path) -> Socket {
        match self.lines.recv_timeout(self.deadline.left()) {
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's first stdout line")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session exited before its first stdout line")
            }
        }
        Socket::connect(self.deadline, socket)
    }

    /// Reads stdout lines until one of `kind` arrives, each taking what
    /// remains of the test's deadline. Only kinds the loop writes before waiting for a prompt
    /// qualify: `preamble_built` and later need a turn, which needs the
    /// test's prompt.
    fn wait_for(&self, kind: &str) {
        loop {
            let line = match self.lines.recv_timeout(self.deadline.left()) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for {kind} on the session's stdout")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the session's stdout closed before {kind}")
                }
            };
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["kind"] == kind {
                return;
            }
        }
    }

    /// Waits under the test's [`Deadline`] for the process to exit, drains its stdout
    /// to EOF, and asserts that nothing it started is left in its group.
    fn wait(mut self) -> (ExitStatus, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match finished.recv_timeout(self.deadline.left()) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                expired(self.deadline, self.group, &finished, "the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session's wait thread ended before the session exited")
            }
        };
        assert!(
            !group_alive(self.deadline, self.group),
            "the session left a process in its group behind"
        );
        std::mem::forget(self.guard);
        // The process is gone, so its stdout is closed: the drain ends,
        // each line taking what remains of the test's deadline.
        loop {
            match self.lines.recv_timeout(self.deadline.left()) {
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for the session's stdout to close")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let stderr = match self.stderr.recv_timeout(self.deadline.left()) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's stderr")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stderr reader ended without its text")
            }
        };
        self.watchdog.stand_down(self.deadline.cleanup());
        (status, stderr)
    }
}

/// The event kinds of `lines`, in order, without `session_status`: an
/// observer thread writes it, so where it falls among the loop's own
/// lines is not what these tests pin.
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// An `openai-responses` stream thinking `thought`, then answering
/// `Hello.`.
fn reasoning_hello(thought: &str) -> Response {
    stream(&[
        json!({"type": "response.reasoning_summary_text.delta", "delta": thought}),
        json!({"type": "response.output_item.done", "item": {
            "type": "reasoning", "id": "rs_1",
            "summary": [{"type": "summary_text", "text": thought}]
        }}),
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// An `openai-responses` stream calling the `shell` tool.
fn shell_call() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_call_1",
        "call_id": "call_1",
        "name": "shell",
        "arguments": json!({"command": "echo hi"}).to_string()
    }})])
}

fn subscribe(client: &Socket) -> Value {
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let sub = until(client, "the subscribe acknowledgement", |line| {
        line["kind"] == "command_accepted"
    });
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0]["payload"]["command_id"], "c_sub");
    sub.into_iter().next().unwrap()
}

fn prompt(client: &Socket, id: &str, text: &str) {
    client.send(&format!(
        r#"{{"id":"{id}","command":"prompt","args":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
    ));
}

fn model(client: &Socket, id: &str, reference: &str, thinking: Option<&str>) {
    let thinking = thinking.map_or(String::new(), |level| format!(r#","thinking":"{level}""#));
    client.send(&format!(
        r#"{{"id":"{id}","command":"model","args":{{"model":"{reference}"{thinking}}}}}"#
    ));
}

fn close(client: &Socket) {
    client.send(r#"{"id":"c_close","command":"close"}"#);
}

/// The request bodies the fake server saw, in arrival order.
fn bodies(server: &ProviderServer) -> Vec<Value> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/responses")
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

/// The durable kinds of the session log: every line with a `seq`.
fn log_kinds(setup: &Setup, id: &str) -> Vec<String> {
    let log = fs::read_to_string(session_dir(setup, id).join("events.jsonl")).unwrap();
    log.lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line.get("seq").is_some())
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_model_sent_between_turns_applies_at_the_next_turn_boundary() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    assert!(
        stream
            .iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the fake model's text arrived: {stream:?}"
    );

    model(&client, "c_model", "fake/n", None);
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    let accepted = stream
        .iter()
        .find(|line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_model"
        })
        .unwrap();
    assert_eq!(accepted["payload"]["command_id"], "c_model");
    let changed = stream.last().unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/m");
    assert_eq!(changed["payload"]["after"]["model"], "fake/n");
    assert_eq!(changed["payload"]["after"]["thinking"], "low");
    assert_eq!(changed["payload"]["source"], "driver");

    prompt(&client, "c_again", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "switch");
    assert_eq!(built["payload"]["model"], "fake/n");
    assert_eq!(built["payload"]["thinking"], "low");

    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    let seen = bodies(&server);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0]["model"], "m");
    assert_eq!(seen[1]["model"], "n");
}

#[test]
fn a_model_then_a_prompt_in_one_batch_runs_the_prompt_on_the_new_model() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    // A switch admitted while idle applies at once, so the prompt that
    // follows in the same drain starts its turn on the new model.
    model(&client, "c_model", "fake/n", None);
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    assert!(
        stream
            .iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the new model's text arrived: {stream:?}"
    );
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    let changed = stream
        .iter()
        .find(|line| line["kind"] == "model_changed")
        .unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/m");
    assert_eq!(changed["payload"]["after"]["model"], "fake/n");
    let seen = bodies(&server);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["model"], "n");
}

#[test]
fn a_model_sent_during_a_turn_applies_after_turn_completed() {
    let setup = Setup::new();
    let server = ProviderServer::start([shell_call(), hello(), hello()]).unwrap();
    install_switch_provider(&setup, &server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "run it");
    stream.extend(until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    }));
    let request = stream.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // During the approval wait the switch is accepted but waits for the
    // next turn boundary.
    model(&client, "c_model", "fake/n", None);
    stream.extend(until(&client, "the model acknowledgement", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_model"
    }));
    client.send(&format!(
        r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request}","decision":"allow"}}}}"#
    ));
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    // The switch is accepted before the turn completes, and applies after.
    let changed = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "model_changed")
        .unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/m");
    assert_eq!(changed["payload"]["after"]["model"], "fake/n");
    // The turn that took the switch finishes on the old model; the next
    // turn runs on the new one.
    let during = bodies(&server);
    assert_eq!(during.len(), 2, "{during:?}");
    assert_eq!(during[0]["model"], "m");
    assert_eq!(during[1]["model"], "m");
    prompt(&client, "c_after", "after the switch");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "switch");
    assert_eq!(built["payload"]["model"], "fake/n");
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "command_accepted",
            "command_accepted",
            "permission_resolved",
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
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    let seen = bodies(&server);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(seen[0]["model"], "m");
    assert_eq!(seen[1]["model"], "m");
    assert_eq!(seen[2]["model"], "n");
}

#[test]
fn reasoning_stays_with_the_model_that_produced_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        reasoning_hello("thought-alpha-xyz"),
        reasoning_hello("thought-beta-xyz"),
        hello(),
    ])
    .unwrap();
    install_switch_provider(&setup, &server);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_first", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    assert!(
        stream
            .iter()
            .any(|line| line["kind"] == "reasoning_completed"),
        "the first turn reasoned: {stream:?}"
    );
    // Switching model drops the earlier model's reasoning from the next
    // request.
    model(&client, "c_model", "fake/n", None);
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    prompt(&client, "c_second", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let seen = bodies(&server);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[1]["model"], "n");
    assert!(
        !seen[1].to_string().contains("thought-alpha-xyz"),
        "no reasoning item from the earlier model is sent: {}",
        seen[1]
    );
    // A thinking-only switch keeps the same model's reasoning.
    model(&client, "c_level", "fake/n", Some("high"));
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    let changed = stream.last().unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/n");
    assert_eq!(changed["payload"]["after"]["model"], "fake/n");
    assert_eq!(changed["payload"]["before"]["thinking"], "low");
    assert_eq!(changed["payload"]["after"]["thinking"], "high");
    prompt(&client, "c_third", "once more");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let seen = bodies(&server);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(
        seen[2].to_string().contains("thought-beta-xyz"),
        "the same model's reasoning is still sent: {}",
        seen[2]
    );
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "reasoning_started",
            "reasoning_delta",
            "assistant_message_delta",
            "assistant_message_delta",
            "reasoning_completed",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "reasoning_started",
            "reasoning_delta",
            "assistant_message_delta",
            "assistant_message_delta",
            "reasoning_completed",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
}

#[test]
fn an_unknown_model_is_rejected_and_changes_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    model(&client, "c_model", "nope/x", None);
    stream.extend(until(&client, "command_rejected", |line| {
        line["kind"] == "command_rejected"
    }));
    let rejected = stream.last().unwrap();
    assert_eq!(rejected["payload"]["command_id"], "c_model");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_rejected",
            "command_accepted",
            "fiber_exited",
        ]
    );
    assert!(
        !log_kinds(&setup, &id).contains(&"model_changed".to_owned()),
        "a rejected model writes no line"
    );
}

#[test]
fn an_unsupported_thinking_level_is_rejected() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    // `m` takes no thinking level.
    model(&client, "c_model", "fake/m", Some("high"));
    stream.extend(until(&client, "command_rejected", |line| {
        line["kind"] == "command_rejected"
    }));
    let rejected = stream.last().unwrap();
    assert_eq!(rejected["payload"]["command_id"], "c_model");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_rejected",
            "command_accepted",
            "fiber_exited",
        ]
    );
    assert!(
        !log_kinds(&setup, &id).contains(&"model_changed".to_owned()),
        "a rejected level writes no line"
    );
}

#[test]
fn a_resume_restores_the_switched_model_and_level() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_first", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    model(&client, "c_model", "fake/n", Some("high"));
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    prompt(&client, "c_second", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let built = stream
        .iter()
        .find(|line| line["kind"] == "preamble_built" && line["payload"]["reason"] == "switch")
        .unwrap();
    assert_eq!(built["payload"]["model"], "fake/n");
    assert_eq!(built["payload"]["thinking"], "high");
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );

    let resumed = start_session(&setup, &id, &["--resume"]);
    let again = resumed.connect(&setup.session_socket(&id));
    let mut second = vec![subscribe(&again)];
    second.extend(until(&again, "resumed fiber_started", |line| {
        line["kind"] == "fiber_started" && line["payload"]["resumed"] == true
    }));
    prompt(&again, "c_third", "a third turn");
    second.extend(until(&again, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    // The resumed session replays the first session's durable lines to the
    // new subscriber before its own events.
    assert_eq!(
        kinds(&second),
        [
            "command_accepted",
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
            "model_changed",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = second
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["model"], "fake/n");
    assert_eq!(built["payload"]["thinking"], "high");
    close(&again);
    second.extend(until_close(&again));
    drop(again);
    let (status, stderr) = resumed.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&second),
        [
            "command_accepted",
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
            "model_changed",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    let seen = bodies(&server);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(seen[2]["model"], "n");
    assert_eq!(seen[2]["reasoning"]["effort"], "high");
}

/// An `anthropic-messages` reply of `Hello.`.
fn anthropic_hello() -> Response {
    let events = [
        json!({"type": "message_start", "message": {"id": "msg_1"}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hello."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"input_tokens": 10, "output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ];
    Response::stream(
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>(),
    )
}

/// Each request body the fake server saw, by path, in arrival order.
fn all_bodies(server: &ProviderServer) -> Vec<(String, Value)> {
    server
        .requests()
        .into_iter()
        .map(|request| (request.path, serde_json::from_slice(&request.body).unwrap()))
        .collect()
}

/// The `web_search` entries of a request body's `tools`.
fn searches(body: &Value) -> Vec<Value> {
    body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| tool["name"] == "web_search")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Asks for the `tools` answer as `id` and returns its lines and its
/// `web_search` entries.
fn tools_answer(client: &Socket, id: &str) -> (Vec<Value>, Vec<Value>) {
    client.send(&format!(r#"{{"id":"{id}","command":"tools"}}"#));
    let lines = until(client, "the tools answer", |line| {
        line["payload"]["command_id"] == id
    });
    let answer = lines.last().unwrap();
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    let tools = answer["payload"]["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let listed = names.clone();
    names.sort_unstable();
    assert!(
        names.windows(2).all(|pair| pair[0] != pair[1]),
        "{listed:?}"
    );
    let found = tools
        .iter()
        .filter(|tool| tool["name"] == "web_search")
        .cloned()
        .collect();
    (lines, found)
}

/// A switched session's stream, its two `web_search` answers and its
/// request bodies by path.
type Switched = (Vec<Value>, [Vec<Value>; 2], Vec<(String, Value)>);

/// Runs one session started on `from`: a turn, the `tools` answer, a
/// switch to `to`, a turn, the `tools` answer, then `close`. Returns the
/// stream, the two `web_search` answers and the request bodies.
fn switch_and_list(from: &str, to: &str, replies: [Response; 2]) -> Switched {
    let setup = Setup::new();
    let server = ProviderServer::start(replies).unwrap();
    install_hosted_provider(&setup, &server, from);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);
    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let (lines, before) = tools_answer(&client, "c_t0");
    stream.extend(lines);
    model(&client, "c_model", to, None);
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    // The switch applied before the next turn starts, so the answer after
    // that turn shows it.
    prompt(&client, "c_again", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let (lines, after) = tools_answer(&client, "c_t1");
    stream.extend(lines);
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let reply: &[&str] = &[
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
    ];
    let openai_reply: &[&str] = &[
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
    ];
    let (first, second) = if from.ends_with("/w") {
        (reply, openai_reply)
    } else {
        (openai_reply, reply)
    };
    let expected = [
        &[
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
        ] as &[&str],
        first,
        &[
            "turn_completed",
            "command_accepted",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
        ],
        second,
        &[
            "turn_completed",
            "command_accepted",
            "command_accepted",
            "fiber_exited",
        ],
    ]
    .concat();
    assert_eq!(kinds(&stream), expected);
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "switch");
    (stream, [before, after], all_bodies(&server))
}

#[test]
fn a_switch_to_a_model_without_hosted_search_withdraws_it() {
    let (stream, [before, after], seen) =
        switch_and_list("fake/w", "fake/m", [anthropic_hello(), hello()]);
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0]["source"], "builtin");
    assert!(after.is_empty(), "{after:?}");
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert!(
        built["payload"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["name"] != "web_search"),
        "{built}"
    );
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].0, "/v1/messages");
    assert_eq!(
        searches(&seen[0].1),
        vec![json!({"type": "web_search_20250305", "name": "web_search"})]
    );
    assert_eq!(seen[1].0, "/v1/responses");
    assert!(searches(&seen[1].1).is_empty(), "{:?}", seen[1].1);
}

#[test]
fn a_switch_to_a_model_with_hosted_search_declares_it() {
    let (stream, [before, after], seen) =
        switch_and_list("fake/m", "fake/w", [hello(), anthropic_hello()]);
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0]["source"], "builtin");
    assert_eq!(after[0]["state"], "full");
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    let declared: Vec<&Value> = built["payload"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["name"] == "web_search")
        .collect();
    assert_eq!(declared.len(), 1, "{built}");
    assert_eq!(declared[0]["registered_by"], "builtin");
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].0, "/v1/responses");
    assert!(searches(&seen[0].1).is_empty(), "{:?}", seen[0].1);
    assert_eq!(seen[1].0, "/v1/messages");
    assert_eq!(
        searches(&seen[1].1),
        vec![json!({"type": "web_search_20250305", "name": "web_search"})]
    );
}

/// Installs extension `short` from source: the provider data `data`, and
/// `init.lua` when given.
fn install_extension(setup: &Setup, short: &str, data: &Value, init: Option<&str>) {
    let source = setup.root.path().join(format!("src-{short}"));
    write_json(
        &source.join("extension.json"),
        &json!({"name": short, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    let provider = data["name"].as_str().unwrap();
    write_json(&source.join(format!("providers/{provider}.json")), data);
    if let Some(init) = init {
        fs::write(source.join("init.lua"), init).unwrap();
    }
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

/// Provider `other` with `om` on `openai-responses` at the fake server,
/// whose key `credential` gives.
fn install_other(setup: &Setup, server: &ProviderServer, credential: &Value) {
    install_extension(
        setup,
        "other",
        &json!({
            "name": "other",
            "credential": credential,
            "models": [{"id": "om", "protocol": "openai-responses",
                        "base_url": format!("{}/v1", server.url()), "context_window": 100000}],
        }),
        None,
    );
}

/// Waits under the test's deadline for `file` to hold a whole line, and
/// returns it trimmed.
fn ready_line(setup: &Setup, file: &Path) -> String {
    let file = file.to_path_buf();
    fakes::within("the ready file", setup.deadline.left(), move || {
        loop {
            if let Ok(text) = fs::read_to_string(&file)
                && text.ends_with('\n')
            {
                return text.trim().to_owned();
            }
            thread::yield_now();
        }
    })
}

/// The bodies of the requests the fake server saw, with their
/// `authorization` header.
fn authorized(server: &ProviderServer) -> Vec<Option<String>> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/responses")
        .map(|request| request.header("authorization").map(str::to_owned))
        .collect()
}

#[test]
fn a_switch_to_an_unloaded_lua_provider_sends_its_token() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&setup, &server);
    install_extension(
        &setup,
        "luax",
        &json!({
            "name": "lp",
            "models": [{"id": "lm", "protocol": "openai-responses",
                        "base_url": format!("{}/v1", server.url()), "context_window": 100000}],
        }),
        Some(
            "fiber.provider(\"lp\", { credential = { timeout = 5000,\n\
               run = function() return { token = \"tok-lp\", expires_at = 4102444800 } end } })\n",
        ),
    );
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);
    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    model(&client, "c_model", "lp/lm", None);
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    let changed = stream
        .iter()
        .find(|line| line["kind"] == "model_changed")
        .unwrap();
    assert_eq!(changed["payload"]["after"]["model"], "lp/lm");
    assert_eq!(
        authorized(&server),
        vec![Some(fakes::fingerprint("Bearer tok-lp"))]
    );
}

#[test]
fn a_command_source_runs_once_across_switches() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let marker = setup.root.path().join("ran");
    install_other(
        &setup,
        &server,
        &json!({"command": ["sh", "-c", format!("echo x >> '{}'; echo other-key", marker.display())]}),
    );
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);
    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    for (command, reference) in [("c_1", "other/om"), ("c_2", "fake/m"), ("c_3", "other/om")] {
        model(&client, command, reference, None);
        stream.extend(until(&client, "model_changed", |line| {
            line["kind"] == "model_changed"
        }));
    }
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "command_accepted",
            "model_changed",
            "command_accepted",
            "model_changed",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "x\n", "one run");
    assert_eq!(
        authorized(&server),
        vec![Some(fakes::fingerprint("Bearer other-key"))]
    );
}

#[test]
fn a_failed_read_is_rejected_with_its_code_and_read_again_later() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    install_switch_provider(&setup, &server);
    let fixed = setup.root.path().join("fixed");
    let script = format!(
        "if [ -e '{}' ]; then echo other-key; else exit 1; fi",
        fixed.display()
    );
    install_other(&setup, &server, &json!({"command": ["sh", "-c", script]}));
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);
    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    model(&client, "c_1", "other/om", None);
    stream.extend(until(&client, "command_rejected", |line| {
        line["kind"] == "command_rejected"
    }));
    let rejected = stream.last().unwrap().clone();
    assert_eq!(rejected["payload"]["command_id"], "c_1");
    assert_eq!(rejected["payload"]["code"], "credential_missing");
    let message = rejected["payload"]["message"].as_str().unwrap();
    assert!(message.contains("`sh`"), "{message}");
    assert!(!message.contains("exit 1"), "{message}");
    let before = log_kinds(&setup, &id);
    // The complete ordered kinds so far: startup only, since a rejected
    // switch writes no durable line.
    assert_eq!(
        before,
        ["session_started", "fiber_started", "extensions_loaded"].map(str::to_owned),
        "{before:?}"
    );
    fs::write(&fixed, "").unwrap();
    model(&client, "c_2", "other/om", None);
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "command_rejected",
            "command_accepted",
            "model_changed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    // No turn ever started, so closing the session leaves no log behind.
    assert!(
        !session_dir(&setup, &id).exists(),
        "a session never prompted keeps no log"
    );
}

#[test]
fn sigterm_during_a_switchs_read_exits_143_and_kills_the_command() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let ready = setup.root.path().join("ready");
    let script = format!("echo $$ > '{}'; exec sleep 30", ready.display());
    install_other(&setup, &server, &json!({"command": ["sh", "-c", script]}));
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);
    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let _subscribed = subscribe(&client);
    // A turn first: a session never prompted leaves no log behind.
    prompt(&client, "c_prompt", "hi");
    until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    model(&client, "c_model", "other/om", None);
    let command: u32 = ready_line(&setup, &ready).parse().unwrap();
    let watchdog = Watchdog::group(command);
    let session = running.group;
    assert!(kill_pid(setup.deadline, session, "TERM").unwrap());
    let rejected = until(&client, "command_rejected", |line| {
        line["kind"] == "command_rejected"
    });
    assert_eq!(rejected.last().unwrap()["payload"]["code"], "closing");
    drop(client);
    // Under the 5 second shutdown bound, which would end the process with
    // no `fiber_exited`.
    let (status, stderr) = fakes::within("the session's exit", Duration::from_secs(4), move || {
        running.wait()
    });
    assert_eq!(status.code(), Some(143), "stderr: {stderr}");
    assert!(
        !group_alive(setup.deadline, command),
        "the command was killed"
    );
    watchdog.stand_down(setup.deadline.cleanup());
    // The complete ordered kinds of the session log: the turn, then the
    // exit, with no `model_changed`, since the switch died in its read.
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
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
}

/// An `openai-responses` stream calling `read` on `path`.
fn read_call(path: &Path) -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_call_1",
        "call_id": "call_1",
        "name": "read",
        "arguments": json!({"path": path}).to_string()
    }})])
}

#[test]
fn the_file_a_switch_read_is_denied_after_its_link_moves() {
    let setup = Setup::new();
    let server =
        ProviderServer::start([read_call(&setup.root.path().join("keys/a")), hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let keys = setup.root.path().join("keys");
    fs::create_dir_all(&keys).unwrap();
    for (name, text) in [("a", "sk-file-a"), ("b", "sk-file-b"), ("c", "sk-file-c")] {
        fs::write(keys.join(name), text).unwrap();
    }
    let link = keys.join("link");
    std::os::unix::fs::symlink(keys.join("c"), &link).unwrap();
    install_other(&setup, &server, &json!({"file": link}));
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);
    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    // Started against `c`; the switch reads `a`; the link then moves on.
    let repoint = |target: &str| {
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(keys.join(target), &link).unwrap();
    };
    repoint("a");
    model(&client, "c_model", "other/om", None);
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    repoint("b");
    prompt(&client, "c_prompt", "read it");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    let resolved = stream
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(resolved["payload"]["decided_by"], "credential_deny");
    let completed = stream
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "denied");
    let log = fs::read_to_string(session_dir(&setup, &id).join("events.jsonl")).unwrap();
    assert!(
        !log.contains("sk-file-a"),
        "no byte of the key reached the log"
    );
    assert_eq!(
        authorized(&server)[0],
        Some(fakes::fingerprint("Bearer sk-file-a"))
    );
}

/// Installs a provider `fake` with `m` on `openai-responses` at the fake
/// server and no declared credential source, stores the `work` and `home`
/// labels, and makes `fake/m` on `work` the configured model.
fn install_labeled_provider(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": format!("{}/v1", server.url()), "context_window": 100000}],
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
    for label in ["work", "home"] {
        config::store_credential(
            &setup.home(),
            "fake",
            label,
            &config::Secret::new(format!("{label}-key\n")),
        )
        .unwrap();
    }
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "providers": {"fake": {"credential": "work"}}}),
    );
}

fn credential(client: &Socket, id: &str, label: &str) {
    client.send(&format!(
        r#"{{"id":"{id}","command":"credential","args":{{"label":"{label}"}}}}"#
    ));
}

/// The global `config.json` and every per-project one: their bytes, so a
/// test proves a command wrote no configuration.
fn config_snapshot(setup: &Setup) -> (Vec<u8>, Vec<(PathBuf, Vec<u8>)>) {
    let global = fs::read(setup.home().join("config.json")).unwrap();
    let mut projects = Vec::new();
    let dir = setup.home().join("projects");
    if dir.is_dir() {
        let mut keys: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        keys.sort();
        for key in keys {
            let file = key.join("config.json");
            projects.push((file.clone(), fs::read(&file).unwrap_or_default()));
        }
    }
    (global, projects)
}

fn assert_config_unchanged(setup: &Setup, before: &(Vec<u8>, Vec<(PathBuf, Vec<u8>)>)) {
    assert_eq!(&config_snapshot(setup), before);
}

#[test]
fn a_credential_sent_between_turns_switches_the_label() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    install_labeled_provider(&setup, &server);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    // After the session started: the command writes no configuration.
    let configs = config_snapshot(&setup);

    credential(&client, "c_cred", "home");
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    let accepted = stream
        .iter()
        .find(|line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_cred"
        })
        .unwrap();
    assert_eq!(accepted["payload"]["command_id"], "c_cred");
    let changed = stream.last().unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/m");
    assert_eq!(changed["payload"]["after"]["model"], "fake/m");
    assert_eq!(changed["payload"]["before"]["credential"], "work");
    assert_eq!(changed["payload"]["after"]["credential"], "home");
    assert_eq!(changed["payload"]["source"], "driver");

    prompt(&client, "c_again", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "switch");
    assert_eq!(built["payload"]["credential"], "home");

    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    // The next request carries the new label's key.
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        requests[1].header("authorization"),
        Some(fakes::fingerprint("Bearer home-key").as_str())
    );
    // The command writes no configuration.
    assert_config_unchanged(&setup, &configs);
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
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
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "model_changed",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
}

#[test]
fn a_credential_naming_no_label_is_rejected_and_changes_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    install_labeled_provider(&setup, &server);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id, &[]);

    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    // After the session started: the command writes no configuration.
    let configs = config_snapshot(&setup);

    credential(&client, "c_cred", "nope");
    stream.extend(until(&client, "command_rejected", |line| {
        line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_cred"
    }));
    let rejected = stream.last().unwrap();
    assert_eq!(rejected["payload"]["code"], "credential_missing");
    let message = rejected["payload"]["message"].as_str().unwrap();
    assert!(message.contains("home"), "{message}");
    assert!(message.contains("work"), "{message}");

    prompt(&client, "c_again", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));

    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    // The next request still uses the old label's key.
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        requests[1].header("authorization"),
        Some(fakes::fingerprint("Bearer work-key").as_str())
    );
    assert_config_unchanged(&setup, &configs);
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_rejected",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
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
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
}

#[test]
fn a_credential_switch_changes_no_other_running_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello(), hello()]).unwrap();
    install_labeled_provider(&setup, &server);
    let first = doors::mint("s_");
    let second = doors::mint("s_");
    let running = start_session(&setup, &first, &[]);
    let other = start_session(&setup, &second, &[]);

    let client = running.connect(&setup.session_socket(&first));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    let peer = other.connect(&setup.session_socket(&second));
    other.wait_for("extensions_loaded");
    let mut peer_stream = vec![subscribe(&peer)];

    prompt(&client, "a_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    prompt(&peer, "b_prompt", "hi");
    peer_stream.extend(until(&peer, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));

    credential(&client, "a_cred", "home");
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));

    prompt(&client, "a_again", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    prompt(&peer, "b_again", "again");
    peer_stream.extend(until(&peer, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));

    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    close(&peer);
    peer_stream.extend(until_close(&peer));
    drop(peer);
    let (status, stderr) = other.wait();
    assert!(status.success(), "stderr: {stderr}");

    // The other session still runs on the old label's key, with no
    // `model_changed` in its log.
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert_eq!(
        requests[3].header("authorization"),
        Some(fakes::fingerprint("Bearer work-key").as_str())
    );
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    assert_eq!(
        kinds(&peer_stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ]
    );
    assert_eq!(
        log_kinds(&setup, &second),
        [
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
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
}
