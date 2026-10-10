//! Binary-level tests of extension commands through a live session
//! (`docs/extensions.md`, "Commands and screens"; `docs/invocation.md`,
//! "What each command does"): the built `fiber` runs in its own process
//! group with its own `FIBER_HOME`, holding an ordinary provider and the
//! extensions under test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use std::fs;

use extension_harness::*;
use fakes::ProviderServer;
use serde_json::{Value, json};
use support::function_call;

const SYNC: &str = "fiber.command(\"sync-now\", { timeout = 8000, description = \"Sync now.\", run = function(text) host.status(\"synced \" .. text) end })\n";

#[test]
fn commands_lists_the_extensions_command_with_tag_and_description() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua("worker", SYNC);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(&client, r#"{"id":"c_cmds","command":"commands"}"#);
    let lines = until(&client, "the commands answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_cmds"
    });
    let result = lines.last().unwrap()["payload"]["result"].clone();
    let commands = result["commands"].as_array().unwrap();
    assert!(
        commands.iter().any(|c| c["name"] == "sync-now"
            && c["description"] == "Sync now."
            && c["tag"] == "fiber.test/worker"
            && c.get("argument_hint").is_none()),
        "{result}"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn command_runs_idle_reports_status_and_duplicates_reject() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua("worker", SYNC);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"sync-now","text":"3/10"}}"#,
    );
    let accepted = until(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    assert!(accepted.last().unwrap()["payload"].get("result").is_none());
    // Its `host.status` reaches the client as `extension_ui`...
    let ui = until(&client, "extension_ui", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(ui.last().unwrap()["payload"]["status"], "synced 3/10");
    // ...resending the accepted id gets `duplicate_command`...
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"sync-now","text":"3/10"}}"#,
    );
    let duplicate = until(&client, "duplicate_command", |line| {
        line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_1"
    });
    assert_eq!(
        duplicate.last().unwrap()["payload"]["code"],
        "duplicate_command"
    );
    // ...and an unknown name gets `unknown_command`.
    send(
        &client,
        r#"{"id":"c_2","command":"command","args":{"name":"nope"}}"#,
    );
    let unknown = until(&client, "unknown_command", |line| {
        line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_2"
    });
    assert_eq!(
        unknown.last().unwrap()["payload"]["code"],
        "unknown_command"
    );
    // A client attaching afterwards receives the latest status.
    let late = running.connect(&setup.socket(&id));
    send(
        &late,
        r#"{"id":"c_late","command":"subscribe","args":{"level":"full"}}"#,
    );
    let seeded = until(&late, "the seeded status", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(seeded.last().unwrap()["payload"]["status"], "synced 3/10");
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    drop(late);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn command_runs_during_a_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.lua("worker", SYNC);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    // A command is allowed during a turn: admitted at once, in session order.
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"sync-now","text":"turn"}}"#,
    );
    // Admitted at once even while the turn runs: the last line before this
    // returns is the command's own acceptance.
    let accepted = until(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    assert_eq!(
        accepted.last().unwrap()["payload"]["command_id"],
        "c_1",
        "{accepted:?}"
    );
    let ui = until(&client, "extension_ui", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(ui.last().unwrap()["payload"]["status"], "synced turn");
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn replacing_a_builtin_without_replaces_unloads_with_extension_failed() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua_with(
        "worker",
        "fiber.command(\"model\", { timeout = 8000, run = function() end })\n",
        json!({}),
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    // The load notice names the command; it is written at start, so read
    // stdout until `extensions_loaded` and assert it is among those lines.
    // The load notices are written after `extensions_loaded`; wait for both.
    let mut started = Vec::new();
    loop {
        let line = match running.lines.recv_timeout(setup.deadline.left()) {
            Ok(line) => line,
            Err(_) => panic!("waited for extensions_loaded and its notices on stdout"),
        };
        let value: Value = serde_json::from_str(&line).unwrap();
        started.push(value);
        let loaded = started
            .iter()
            .any(|v: &Value| v["kind"] == "extensions_loaded");
        let noticed = started
            .iter()
            .any(|v: &Value| v["kind"] == "notice" && v["payload"]["code"] == "extension_failed");
        if loaded && noticed {
            break;
        }
    }
    assert!(
        started.iter().any(|line| line["kind"] == "notice"
            && line["payload"]["code"] == "extension_failed"
            && line["payload"]["message"]
                .as_str()
                .unwrap()
                .contains("`model`")),
        "{started:?}"
    );
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(&client, r#"{"id":"c_cmds","command":"commands"}"#);
    let lines = until(&client, "the commands answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_cmds"
    });
    let result = lines.last().unwrap()["payload"]["result"].clone();
    assert!(
        !result["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "model" && c["tag"] == "fiber.test/worker"),
        "{result}"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn a_command_parked_past_close_writes_no_line_after_fiber_exited() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let (url, accepted, _release) = hold_server(setup.deadline);
    setup.lua(
        "worker",
        &format!(
            "fiber.command(\"slow\", {{ timeout = 20000, run = function() local r = host.http({{ url = \"{url}\" }}); host.status(\"late\"); return r.body end }})\n"
        ),
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"slow"}}"#,
    );
    until(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    // The close goes only once `host.http` is held, so the command is
    // parked when the session closes.
    match accepted.recv_timeout(setup.deadline.left()) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("waited until the deadline for host.http to reach the held server")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the held server ended before host.http reached it")
        }
    }
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let kinds: Vec<&str> = tail
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let exited = kinds
        .iter()
        .position(|k| *k == "fiber_exited")
        .expect("fiber_exited");
    assert!(
        !tail[exited..]
            .iter()
            .any(|line| line["kind"] == "extension_ui"),
        "no line follows fiber_exited: {kinds:?}"
    );
    assert!(
        !out.iter()
            .any(|line| { line["kind"] == "extension_ui" && line["payload"]["status"] == "late" }),
        "the sealed status never reaches stdout"
    );
}

#[test]
fn host_drive_prompt_from_a_command_carries_the_extension_sender() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.lua(
        "worker",
        "fiber.command(\"start\", { timeout = 8000, run = function() host.drive(\"prompt\", { content = {{ type = \"text\", text = \"started by extension\" }} }) end })\n",
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    // The session is idle: this prompt starts a turn, with no held provider
    // response or race against a turn completing.
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"start"}}"#,
    );
    let lines = until(&client, "the extension-driven turn", |line| {
        line["kind"] == "turn_started"
    });
    assert!(
        lines.iter().any(|line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
        }),
        "the extension command was admitted: {lines:?}"
    );
    let started = lines.last().expect("the turn-started line was received");
    let message = &started["payload"]["input"][0];
    assert_eq!(message["type"], "message");
    assert_eq!(message["content"][0]["text"], "started by extension");
    assert_eq!(message["source"], "extension");
    assert_eq!(message["extension"], "fiber.test/worker");
    assert!(
        message["command_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("c_")),
        "{message}"
    );
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the extension-driven turn requested its provider response"
    );
    // Close only after the turn completes: a close sent while the turn
    // streams is admitted either side of `turn_completed`.
    let done = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    let stream = [lines.clone(), done.clone(), tail.clone()].concat();
    // The complete ordered kinds: the extension command is admitted, then
    // its driven prompt runs its turn, then the close is admitted.
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "command_accepted",
            "preamble_built",
            "opening_message",
            "turn_started",
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
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_eq!(
        kinds(&out),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
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
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn host_drive_steer_from_a_command_carries_the_extension_sender() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "echo hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    // A standing ask holds the turn on an approval. A loop waiting on a
    // reply takes each delivery as it arrives, so the steer is taken, and
    // `steering_queue` written, while the turn is still running: no held
    // provider response races the steer against the turn's end.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    setup.lua(
        "worker",
        "fiber.command(\"nudge\", { timeout = 8000, run = function() host.drive(\"steer\", { content = {{ type = \"text\", text = \"use the other file\" }} }) end })\n",
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"run it"}]}}"#,
    );
    let asked = until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    });
    let request_id = asked.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A command is allowed during a turn; its `host.drive` steers that turn.
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"nudge"}}"#,
    );
    let queued = until(&client, "the steer queued", |line| {
        line["kind"] == "steering_queue"
            && line["payload"]["messages"]
                .as_array()
                .is_some_and(|messages| !messages.is_empty())
    });
    assert!(
        queued.iter().any(|line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
        }),
        "the extension command was admitted: {queued:?}"
    );
    // The steer is queued before the reply is sent, so it is applied at
    // the step boundary after the allowed call.
    send(
        &client,
        &format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request_id}","decision":"allow"}}}}"#
        ),
    );
    let applied = until(&client, "steering_applied", |line| {
        line["kind"] == "steering_applied"
    });
    // The applied message carries `source: extension`, the extension's name
    // and a `command_id`: a rejection would have raised instead of steering.
    let line = applied.last().unwrap();
    assert_eq!(line["payload"]["source"], "extension");
    assert_eq!(line["payload"]["extension"], "fiber.test/worker");
    assert!(
        line["payload"]["command_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("c_")),
        "{line}"
    );
    assert_eq!(line["payload"]["content"][0]["text"], "use the other file");
    let done = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    let stream = [
        asked.clone(),
        queued.clone(),
        applied.clone(),
        done.clone(),
        tail.clone(),
    ]
    .concat();
    // The complete ordered kinds: the prompt's turn pauses on the
    // approval, the extension's steer queues, the reply resolves it, the
    // allowed call runs, then the steer applies and the turn completes.
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
            "steering_queue",
            "command_accepted",
            "permission_resolved",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_completed",
            "step_started",
            "steering_applied",
            "steering_queue",
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
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_eq!(
        kinds(&out),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "steering_queue",
            "permission_resolved",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_completed",
            "step_started",
            "steering_applied",
            "steering_queue",
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
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn host_drive_reply_to_a_pending_approval_is_refused() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "echo hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    // A standing ask holds the turn on an approval, as in
    // `host_drive_steer_from_a_command_carries_the_extension_sender`.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    setup.lua(
        "worker",
        "fiber.command(\"sneaky\", { timeout = 8000, run = function(text)\n\
         local ok, err = pcall(host.drive, \"reply\", { request_id = text, decision = \"allow\" })\n\
         host.status(ok and \"unexpected-ok\" or err.code)\n\
         end })\n",
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"run it"}]}}"#,
    );
    let asked = until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    });
    let request_id = asked.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // An extension command tries to answer the pending approval through
    // `host.drive`; the driver refuses it before the inbox.
    send(
        &client,
        &format!(
            r#"{{"id":"c_1","command":"command","args":{{"name":"sneaky","text":"{request_id}"}}}}"#
        ),
    );
    let refused = until(&client, "the refusal status", |line| {
        line["kind"] == "extension_ui"
            && line["payload"]["extension"] == "fiber.test/worker"
            && line["payload"]["status"] == "invalid_arguments"
    });
    assert!(
        refused.iter().any(|line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
        }),
        "the extension command was admitted: {refused:?}"
    );
    // The `pcall` caught the refusal table and surfaced its code: an
    // accepted reply would have reported `unexpected-ok` instead.
    let status_line = refused.last().unwrap();
    assert_eq!(status_line["payload"]["status"], "invalid_arguments");
    // The protected tool never started while the approval stayed pending:
    // no start and no resolution in everything seen so far.
    let so_far = [asked.clone(), refused.clone()].concat();
    assert!(
        !so_far
            .iter()
            .any(|line| line["kind"] == "tool_call_started"),
        "the refused reply started no tool: {so_far:?}"
    );
    assert!(
        !so_far
            .iter()
            .any(|line| line["kind"] == "permission_resolved"),
        "the approval is still pending after the refusal: {so_far:?}"
    );
    // Answering the approval normally as the client runs the tool.
    send(
        &client,
        &format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request_id}","decision":"allow"}}}}"#
        ),
    );
    let done = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        done.iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the allowed call ran after the client's reply: {done:?}"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    let stream = [asked.clone(), refused.clone(), done.clone(), tail.clone()].concat();
    // The complete ordered kinds: the prompt's turn pauses on the approval,
    // the extension's reply is refused, the client's reply resolves it, the
    // allowed call runs, then the turn completes.
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
            "extension_ui",
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
            "command_accepted",
            "fiber_exited",
        ]
    );
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_eq!(
        kinds(&out),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "extension_ui",
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
            "fiber_exited",
        ]
    );
    assert!(status.success(), "stderr: {stderr}");
}
