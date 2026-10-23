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
use support::{Setup, function_call, session_dir};

const SYNC: &str = "fiber.command(\"sync-now\", { timeout = 8000, description = \"Sync now.\", run = function(text) host.status(\"synced \" .. text) end })\n";

/// `SYNC` held behind `host.http`: `run` blocks on the held server before it
/// reports status, so the test decides when `extension_ui` lands. `{url}`
/// is the held server's url, replaced at the use site.
const SYNC_HELD: &str = "fiber.command(\"sync-now\", { timeout = 8000, description = \"Sync now.\", run = function(text) host.http({ url = \"{url}\" }) host.status(\"synced \" .. text) end })\n";

/// Every line of the command's action, not just the expected endpoints.
fn command_action<'a>(
    lines: &'a [Value],
    command: &str,
    extension: &str,
    expected: &[&str],
) -> Vec<&'a Value> {
    let requested = lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_requested"
                && line["payload"]["ran_by"]["command_id"] == command
        })
        .expect("command's inner request");
    assert_eq!(
        requested["payload"]["ran_by"],
        json!({"extension":extension,"command_id":command})
    );
    let action = requested["action_id"].as_str().expect("inner action id");
    let lines: Vec<_> = lines
        .iter()
        .filter(|line| line["action_id"] == action)
        .collect();
    assert_eq!(
        lines
            .iter()
            .map(|line| line["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    for line in &lines {
        assert!(line.get("turn_id").is_none(), "{line}");
    }
    for line in lines.iter().skip(1) {
        assert!(line["payload"].get("ran_by").is_none(), "{line}");
    }
    lines
}

#[test]
fn an_idle_commands_host_tool_is_turnless_and_never_contacts_the_provider() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "the file\n").unwrap();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua("peek", "fiber.command('peek', {timeout=5000,run=function() local o=host.tool('read',{path='note.txt'}) host.status(o.status .. ':' .. #o.content) end})");
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
        r#"{"id":"c_peek","command":"command","args":{"name":"peek"}}"#,
    );
    let seen = until_session(&client, "command tool status", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/peek"
    });
    assert_eq!(seen.last().unwrap()["payload"]["status"], "completed:1");
    let action = command_action(
        &seen,
        "c_peek",
        "fiber.test/peek",
        &[
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    assert_eq!(action[0]["payload"]["name"], "read");
    assert_eq!(action[2]["payload"]["status"], "completed");
    assert!(server.requests().is_empty());
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_session_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let all = [seen, tail].concat();
    command_action(
        &all,
        "c_peek",
        "fiber.test/peek",
        &[
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    command_action(
        &out,
        "c_peek",
        "fiber.test/peek",
        &[
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    assert!(server.requests().is_empty());
}

#[test]
fn a_commands_own_before_hook_fails_but_other_hooks_and_later_model_calls_run() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "the file\n").unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path":"note.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    setup.lua("audit", "fiber.hook('before_tool', {timeout=5000,on_failure='blocking',run=function(call) host.fs.write('audit',call.tool) end})");
    setup.lua("guard", "fiber.hook('before_tool', {timeout=5000,on_failure='non-blocking',run=function() host.fs.write('guard-ran','x') end})\nfiber.command('peek', {timeout=5000,run=function() local o=host.tool('read',{path='note.txt'}) host.status(o.status .. ':' .. o.error.code) end})");
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
        r#"{"id":"c_peek","command":"command","args":{"name":"peek"}}"#,
    );
    let seen = until_session(&client, "before hook failure status", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/guard"
    });
    assert_eq!(
        seen.last().unwrap()["payload"]["status"],
        "failed:hook_failed"
    );
    let action = command_action(
        &seen,
        "c_peek",
        "fiber.test/guard",
        &["tool_call_requested", "tool_call_completed"],
    );
    assert_eq!(action[1]["payload"]["status"], "failed");
    assert_eq!(action[1]["payload"]["error"]["code"], "hook_failed");
    assert!(
        action[1]["payload"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("fiber.test/guard")
    );
    assert_eq!(
        fs::read_to_string(setup.workspace().join("audit")).unwrap(),
        "read"
    );
    assert!(!setup.workspace().join("guard-ran").exists());
    assert!(server.requests().is_empty());
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"read it"}]}}"#,
    );
    let done = until_session(&client, "later model turn", |line| {
        line["kind"] == "turn_completed"
    });
    assert_eq!(
        fs::read_to_string(setup.workspace().join("guard-ran")).unwrap(),
        "x"
    );
    let model_results: Vec<_> = done
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(model_results.len(), 1);
    assert_eq!(model_results[0]["payload"]["status"], "completed");
    assert!(model_results[0].get("turn_id").is_some());
    assert_eq!(server.requests().len(), 2);
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_session_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    command_action(
        &[seen, done, tail].concat(),
        "c_peek",
        "fiber.test/guard",
        &["tool_call_requested", "tool_call_completed"],
    );
    command_action(
        &out,
        "c_peek",
        "fiber.test/guard",
        &["tool_call_requested", "tool_call_completed"],
    );
}

#[test]
fn a_commands_own_sanitize_after_hook_fails_without_leaking_the_output() {
    let setup = Setup::new_with_fm_root();
    let secret = "do-not-leak-556";
    fs::write(setup.workspace().join("note.txt"), secret).unwrap();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua("guard", "fiber.hook('after_tool', {phase='sanitize',timeout=5000,on_failure='blocking',run=function() host.fs.write('guard-ran','x') end})\nfiber.command('peek', {timeout=5000,run=function() local o=host.tool('read',{path='note.txt'}) host.status(o.status .. ':' .. o.error.code .. ':' .. o.content[1].text) end})");
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
        r#"{"id":"c_peek","command":"command","args":{"name":"peek"}}"#,
    );
    let seen = until_session(&client, "after hook failure status", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/guard"
    });
    assert!(
        seen.last().unwrap()["payload"]["status"]
            .as_str()
            .unwrap()
            .starts_with("failed:hook_failed:")
    );
    let action = command_action(
        &seen,
        "c_peek",
        "fiber.test/guard",
        &[
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    let completed = &action[2]["payload"];
    assert_eq!(completed["status"], "failed");
    assert_eq!(completed["error"]["code"], "hook_failed");
    assert!(
        completed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("fiber.test/guard")
    );
    assert_eq!(completed["content"].as_array().unwrap().len(), 1);
    assert!(completed.get("artifact").is_none());
    assert!(!setup.workspace().join("guard-ran").exists());
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let sessions_dir = log::sessions_dir(&setup.home(), &doors::project(&workspace));
    // Check while live too: closing an idle session need not retain its directory.
    assert!(!support::on_disk(&sessions_dir).contains(secret));
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_session_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let all = [seen, tail].concat();
    command_action(
        &all,
        "c_peek",
        "fiber.test/guard",
        &[
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    assert!(!serde_json::to_string(&all).unwrap().contains(secret));
    assert!(!serde_json::to_string(&out).unwrap().contains(secret));
    assert!(!stderr.contains(secret));
    assert!(!support::on_disk(&sessions_dir).contains(secret));
    assert!(server.requests().is_empty());
}

#[test]
fn commands_lists_the_extensions_command_with_tag_and_description() {
    let setup = Setup::new_with_fm_root();
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
    let lines = until_session(&client, "the commands answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_cmds"
    });
    let result = lines.last().unwrap()["payload"]["result"].clone();
    let commands = result["commands"].as_array().unwrap();
    assert!(
        commands.iter().any(|c| c["name"] == "sync-now"
            && c["description"] == "Sync now."
            && c["tag"] == "fiber.test/worker"
            && c["extension"] == true
            && c.get("argument_hint").is_none()),
        "{result}"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn command_runs_idle_reports_status_and_duplicates_reject() {
    let setup = Setup::new_with_fm_root();
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
    let accepted = until_session(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    assert!(accepted.last().unwrap()["payload"].get("result").is_none());
    // Its `host.status` reaches the client as `extension_ui`...
    let ui = until_session(&client, "extension_ui", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(ui.last().unwrap()["payload"]["status"], "synced 3/10");
    // ...resending the accepted id gets `duplicate_command`...
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"sync-now","text":"3/10"}}"#,
    );
    let duplicate = until_session(&client, "duplicate_command", |line| {
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
    let unknown = until_session(&client, "unknown_command", |line| {
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
    let seeded = until_session(&late, "the seeded status", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(seeded.last().unwrap()["payload"]["status"], "synced 3/10");
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    drop(late);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn command_runs_during_a_turn() {
    let setup = Setup::new_with_fm_root();
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
    let accepted = until_session(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    assert_eq!(
        accepted.last().unwrap()["payload"]["command_id"],
        "c_1",
        "{accepted:?}"
    );
    let ui = until_session(&client, "extension_ui", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(ui.last().unwrap()["payload"]["status"], "synced turn");
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn replacing_a_builtin_without_replaces_unloads_with_extension_failed() {
    let setup = Setup::new_with_fm_root();
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
        let line = match setup.deadline.recv(&running.lines) {
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
    let lines = until_session(&client, "the commands answer", |line| {
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
    let _tail = until_session_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn extensions_loaded_in_the_log_carries_each_replaces() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua_with(
        "worker",
        "fiber.command(\"model\", { timeout = 8000, run = function() end })\n",
        json!({"replaces": ["model"]}),
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    until_session(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_sub"
    });
    // Read before close: a never-prompted session's directory is deleted
    // on close, so after the exit the file is gone
    // (`crates/doors/src/session.rs`, `prompted`).
    let text = fs::read_to_string(session_dir(&setup, &id).join("events.jsonl")).unwrap();
    let log = support::complete_lines(&text);
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let loaded = log
        .iter()
        .rfind(|line| line["kind"] == "extensions_loaded")
        .expect("the log holds extensions_loaded");
    let extensions = loaded["payload"]["extensions"]
        .as_array()
        .expect("extensions_loaded carries extensions");
    assert_eq!(
        extensions
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["fake", "fiber.test/worker"]
    );
    assert_eq!(extensions[0].get("replaces"), None);
    assert_eq!(extensions[1]["replaces"], json!(["model"]));
    // The complete ordered kinds of the log: `clients` and
    // `session_status` are ephemeral, so they never reach the file
    // (docs/testing.md, "Event streams"). No turn runs, so no preamble
    // is built and no opening message is written.
    assert_eq!(
        log.iter()
            .map(|line| line["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["session_started", "fiber_started", "extensions_loaded"]
    );
}

#[test]
fn a_command_parked_past_close_writes_no_line_after_fiber_exited() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let held = ProviderServer::start([]).unwrap();
    held.hold();
    let url = format!("{}/", held.url());
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
    until_session(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    // The close goes only once `host.http` is held, so the command is
    // parked when the session closes.
    assert!(
        held.await_requests(1, setup.deadline),
        "waited until the deadline for host.http to reach the held server"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_session_close(&client);
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
    let setup = Setup::new_with_fm_root();
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
    let lines = until_session(&client, "the extension-driven turn", |line| {
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
        server.await_requests(1, setup.deadline),
        "the extension-driven turn requested its provider response"
    );
    // Close only after the turn completes: a close sent while the turn
    // streams is admitted either side of `turn_completed`.
    let done = until_session(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_session_close(&client);
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
    let setup = Setup::new_with_fm_root();
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
    let asked = until_session(&client, "permission_requested", |line| {
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
    let queued = until_session(&client, "the steer queued", |line| {
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
    let applied = until_session(&client, "steering_applied", |line| {
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
    let done = until_session(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_session_close(&client);
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
            "steering_queue",
            "steering_applied",
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
            "steering_queue",
            "steering_applied",
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
    let setup = Setup::new_with_fm_root();
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
    let asked = until_session(&client, "permission_requested", |line| {
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
    let refused = until_session(&client, "the refusal status", |line| {
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
    let done = until_session(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        done.iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the allowed call ran after the client's reply: {done:?}"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_session_close(&client);
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

#[test]
fn a_skill_named_like_an_extension_command_keeps_the_commands_row() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let held = ProviderServer::start([]).unwrap();
    held.hold();
    let url = format!("{}/", held.url());
    setup.lua("worker", &SYNC_HELD.replace("{url}", &url));
    let dir = setup.workspace().join(".agents/skills/sync-now");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: sync-now\ndescription: Syncs the workspace.\n---\nBody.\n",
    )
    .unwrap();
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(&client, r#"{"id":"c_cmds","command":"commands"}"#);
    let lines = until_session(&client, "the commands answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_cmds"
    });
    let result = lines.last().unwrap()["payload"]["result"].clone();
    let commands = result["commands"].as_array().unwrap();
    let syncs: Vec<&Value> = commands
        .iter()
        .filter(|c| c["name"] == "sync-now")
        .collect();
    assert_eq!(syncs.len(), 1, "{result}");
    assert_eq!(syncs[0]["tag"], "skill", "{result}");
    assert!(syncs[0].get("extension").is_none(), "{result}");
    send(
        &client,
        r#"{"id":"c_run","command":"command","args":{"name":"sync-now","text":"x"}}"#,
    );
    let ran = until_session(&client, "the command answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_run"
    });
    assert!(!ran.is_empty());
    assert!(
        held.await_requests(1, setup.deadline),
        "waited until the deadline for the held command's host.http request"
    );
    // The held `run` is admitted but blocked on `host.http`: release it,
    // then wait for its status before closing, so `close` cannot win the
    // race against `extension_ui`.
    held.release();
    let status = until_session(
        &client,
        "the held command's status after host.http was released",
        |line| {
            line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
        },
    );
    assert_eq!(status.last().unwrap()["payload"]["status"], "synced x");
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    // `clients` is set aside: written when the connection attaches, with
    // no position against the session's other lines (`docs/testing.md`,
    // "Event streams"); one attach writes exactly one.
    // Only the durable lines stay pinned in order (`docs/events.md:116`):
    // `session_started` first, `fiber_started` after it, `fiber_exited`
    // last (`docs/events.md`, `session_started`, `fiber_started`,
    // `extensions_loaded` and `fiber_exited` sections). `notice` and
    // `extension_ui` are ephemeral with no promised count, order or
    // placement, so the test pins only their required facts below, not
    // their positions.
    let durable = kinds(&out)
        .into_iter()
        .filter(|kind| {
            *kind == "session_started"
                || *kind == "fiber_started"
                || *kind == "extensions_loaded"
                || *kind == "fiber_exited"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        durable,
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited"
        ],
    );
    assert_eq!(
        kinds(&out)
            .iter()
            .filter(|kind| **kind == "clients")
            .count(),
        1
    );
    let notices: Vec<&Value> = out
        .iter()
        .filter(|line| line["kind"] == "notice" && line["payload"]["code"] == "skill_shadowed")
        .collect();
    assert_eq!(notices.len(), 1, "{out:?}");
    let statuses: Vec<&Value> = out
        .iter()
        .filter(|line| {
            line["kind"] == "extension_ui"
                && line["payload"]["extension"] == "fiber.test/worker"
                && line["payload"]["status"] == "synced x"
        })
        .collect();
    assert_eq!(statuses.len(), 1, "{out:?}");
    assert!(
        notices[0]["payload"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("fiber.test/worker")
            && notices[0]["payload"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("sync-now"),
        "{:?}",
        notices[0]
    );
}
