//! The offer of a repository's code, end to end (`docs/extensions.md`,
//! "Code a repository ships"): a session with a `full` client raises it and
//! takes the client's `decisions`; `fiber ask` has nobody to ask, so it skips
//! each item with a notice or fails on a required one; a session that exits
//! on its first offer is kept and raises the offer again on resume. A
//! `start` with `content` through the hub waits for the requester's `full`
//! subscription, so a client that subscribes is offered the code, and a
//! connection that never subscribes runs unattended.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod extension_harness;
mod support;

use std::cell::Cell;
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::Client;
use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, HubProc, SessionGuard, Setup, Socket, close_session, connect_hub, function_call,
    hello, outputs_sent, run_to_exit, session_dir, start_session, stream, subscribe, until,
    write_json,
};

/// Declares the MCP server `db` in the workspace's repository file: a
/// server that really answers, so an approval starts it. `/bin/bash`
/// runs the repository's copy of the MCP fixture on a directory beside
/// the workspace; the copy is what the approval pins.
fn declare_db(setup: &Setup, extra: &Value) {
    let script = std::fs::read(fakes::mcp_fixture()).unwrap();
    let srv = setup.workspace().join("srv");
    std::fs::create_dir_all(&srv).unwrap();
    std::fs::write(srv.join("server.sh"), script).unwrap();
    let dir = setup.root.path().join("fx");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tools.json"),
        json!([{"name": "echo", "annotations": {"readOnlyHint": true, "openWorldHint": false}}])
            .to_string(),
    )
    .unwrap();
    let mut entry = json!({
        "command": "/bin/bash",
        "args": ["srv/server.sh", dir.display().to_string()],
    });
    for (key, value) in extra.as_object().unwrap() {
        entry[key] = value.clone();
    }
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": entry}}}),
    );
}

/// A person-layer MCP server `early`, started with the session and never
/// offered: its fixture lives under the root, and it joins the home
/// configuration keeping the fake model.
fn declare_early(setup: &Setup) {
    let script = std::fs::read(fakes::mcp_fixture()).unwrap();
    let srv = setup.root.path().join("early-srv");
    std::fs::create_dir_all(&srv).unwrap();
    std::fs::write(srv.join("server.sh"), script).unwrap();
    let dir = setup.root.path().join("early-fx");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tools.json"),
        json!([{"name": "echo"}]).to_string(),
    )
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {"early": {
            "command": "/bin/bash",
            "args": [srv.join("server.sh").display().to_string(), dir.display().to_string()],
        }}}}),
    );
}

/// `fiber <args>` run in the workspace.
#[track_caller]
fn in_workspace(setup: &Setup, args: &[&str]) -> Command {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    command
}

/// `fiber ask hi` in the workspace: its exit code, stdout lines and stderr.
#[track_caller]
fn ask(setup: &Setup) -> (Option<i32>, Vec<Value>, String) {
    let output = run_to_exit(
        setup.deadline,
        "fiber ask",
        in_workspace(setup, &["ask", "hi"]),
    );
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (
        output.status.code(),
        lines,
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn of<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|line| line["kind"] == kind).collect()
}

/// The internal session command for `id`, with `extra` appended, in its
/// own process group, and its stdout lines as they come.
#[track_caller]
fn spawn_session(setup: &Setup, id: &str, extra: &[&str]) -> (HubProc, mpsc::Receiver<Value>) {
    let workspace = setup.workspace();
    let mut args = vec!["session", "--id", id, "--workspace"];
    args.push(workspace.to_str().unwrap());
    args.extend(extra);
    let mut command = setup.fiber(&args);
    command.stderr(Stdio::null());
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let parsed = serde_json::from_str(&line).unwrap_or(Value::String(line));
            if tx.send(parsed).is_err() {
                break;
            }
        }
    });
    (
        HubProc {
            child,
            watchdog,
            group,
            deadline: setup.deadline,
        },
        lines,
    )
}

/// How long the reader thread polls for the next line while a wait's single
/// deadline runs on the test thread.
const SLICE: Duration = Duration::from_secs(1);

/// Waits for the session's `extensions_loaded` on stdout: every startup
/// line is written and the socket is listening. One deadline bounds the
/// whole wait: a scoped thread reads the lines and the test takes the
/// arrival with one `recv_timeout`.
#[track_caller]
fn started(deadline: Deadline, lines: mpsc::Receiver<Value>) {
    let stop = AtomicBool::new(false);
    let got = thread::scope(|scope| {
        let (tx, found) = mpsc::channel();
        let stop = &stop;
        scope.spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match Deadline::after(SLICE).recv(&lines) {
                    Ok(line) => {
                        if line["kind"] == "extensions_loaded" {
                            if let Ok(()) = tx.send(true) {}
                            return;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if let Ok(()) = tx.send(false) {}
                        return;
                    }
                }
            }
        });
        let got = deadline.recv(&found);
        stop.store(true, Ordering::SeqCst);
        got
    });
    assert!(
        got.expect("the session's extensions_loaded within the deadline"),
        "the session's stdout closed before extensions_loaded"
    );
}

/// A `full` client on the session `id`, once the session counts it.
#[track_caller]
fn attach(setup: &Setup, id: &str, sub: &str) -> Client {
    let client = Client::connect(&setup.session_socket(id)).unwrap();
    client
        .send(&format!(
            r#"{{"id":"{sub}","command":"subscribe","args":{{"level":"full"}}}}"#
        ))
        .unwrap();
    client
        .recv_until(setup.deadline, |line| {
            line["kind"] == "clients" && line["payload"]["count"] == 1
        })
        .expect("the session counts the client within the deadline");
    client
}

fn recv_kind(deadline: Deadline, client: &Client, kind: &str) -> Value {
    client
        .recv_until(deadline, |line| line["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} within the deadline"))
}

/// The answer to the command `id`.
fn answer(deadline: Deadline, client: &Client, id: &str) -> Value {
    client
        .recv_until(deadline, |line| {
            (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
                && line["payload"]["command_id"] == id
        })
        .unwrap_or_else(|| panic!("no answer to {id} within the deadline"))
}

#[track_caller]
fn prompt(client: &Client, id: &str) {
    client
        .send(&format!(
            r#"{{"id":"{id}","command":"prompt","args":{{"content":[{{"type":"text","text":"hi"}}]}}}}"#
        ))
        .unwrap();
}

#[track_caller]
fn reply(client: &Client, id: &str, request: &str, decisions: &[&str]) {
    client
        .send(&format!(
            r#"{{"id":"{id}","command":"reply","args":{{"request_id":"{request}","decisions":{}}}}}"#,
            json!(decisions)
        ))
        .unwrap();
}

#[track_caller]
fn close(client: &Client, id: &str) {
    client
        .send(&format!(r#"{{"id":"{id}","command":"close"}}"#))
        .unwrap();
}

#[test]
fn a_full_client_reads_a_listed_file_and_nothing_else() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let script = setup.workspace().join("tools/db.sh");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    let source = "# OFFEREDSOURCE1237\n";
    fs::write(&script, source).unwrap();
    let permissions = fs::metadata(&script).unwrap().permissions();
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": {"command": "/bin/echo", "args": ["tools/db.sh"]}}}}),
    );
    let id = doors::mint("s_");
    let (session, stdout) = spawn_session(&setup, &id, &[]);
    started(setup.deadline, stdout);
    let client = attach(&setup, &id, "c_sub");
    prompt(&client, "c_prompt");
    let offered = recv_kind(setup.deadline, &client, "repository_code_offered");
    let request = offered["payload"]["request_id"].as_str().unwrap();
    assert_eq!(
        offered["payload"]["items"][0]["files"],
        json!(["tools/db.sh"])
    );
    let ask_file = |command: &str, path: &str| {
        client.send(&json!({"id": command, "command": "offered_file", "args": {"request_id": request, "path": path}}).to_string()).unwrap();
    };
    ask_file("c_file", "tools/db.sh");
    let accepted = answer(setup.deadline, &client, "c_file");
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(accepted["payload"]["result"], json!({"text": source}));
    for (command, path) in [("c_parent", "../secret"), ("c_absolute", "/etc/hosts")] {
        ask_file(command, path);
        let rejected = answer(setup.deadline, &client, command);
        assert_eq!(rejected["kind"], "command_rejected");
        assert_eq!(rejected["payload"]["code"], "invalid_arguments");
        assert!(rejected["payload"].get("result").is_none());
    }
    let outside = setup.root.path().join("secret");
    let secret = "OUTSIDESECRET1237";
    fs::write(&outside, secret).unwrap();
    fs::remove_file(&script).unwrap();
    std::os::unix::fs::symlink(&outside, &script).unwrap();
    ask_file("c_link", "tools/db.sh");
    let rejected = client
        .recv_until(setup.deadline, |line| {
            assert!(
                !line.to_string().contains(secret),
                "protected bytes reached the client: {line}"
            );
            line["payload"]["command_id"] == "c_link"
        })
        .expect("the replaced link is refused within the deadline");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    fs::remove_file(&script).unwrap();
    fs::write(&script, source).unwrap();
    fs::set_permissions(&script, permissions).unwrap();
    reply(&client, "c_reply", request, &["approve"]);
    recv_kind(setup.deadline, &client, "repository_code_resolved");
    assert_eq!(
        answer(setup.deadline, &client, "c_reply")["kind"],
        "command_accepted"
    );
    recv_kind(setup.deadline, &client, "turn_completed");
    ask_file("c_stale", "tools/db.sh");
    let stale = answer(setup.deadline, &client, "c_stale");
    assert_eq!(stale["kind"], "command_rejected");
    assert_eq!(stale["payload"]["code"], "stale_request");
    close(&client, "c_close");
    recv_kind(setup.deadline, &client, "fiber_exited");
    drop(client);
    assert!(session.wait().success());
    assert_eq!(server.requests().len(), 1);
    let events = fs::read_to_string(session_dir(&setup, &id).join("events.jsonl")).unwrap();
    assert!(!events.contains(secret));
}

#[test]
fn a_full_client_answers_the_offer_and_the_turn_runs() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({}));
    let id = doors::mint("s_");
    let (session, stdout) = spawn_session(&setup, &id, &[]);
    started(setup.deadline, stdout);
    let client = attach(&setup, &id, "c_sub");
    prompt(&client, "c_prompt");

    let offered = recv_kind(setup.deadline, &client, "repository_code_offered");
    let request = offered["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let items = offered["payload"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "mcp_server");
    assert_eq!(items[0]["name"], "db");
    let hash = items[0]["hash"].as_str().unwrap().to_owned();
    let status = client
        .recv_until(setup.deadline, |line| {
            line["kind"] == "session_status" && line["payload"]["state"] == "waiting"
        })
        .expect("a waiting session_status within the deadline");
    assert_eq!(status["payload"]["waiting"]["kind"], "offer");
    assert_eq!(status["payload"]["waiting"]["request_id"], request.as_str());

    reply(&client, "c_r0", &request, &["approve", "approve"]);
    let rejected = answer(setup.deadline, &client, "c_r0");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");

    reply(&client, "c_r1", &request, &["approve"]);
    let resolved = recv_kind(setup.deadline, &client, "repository_code_resolved");
    assert_eq!(resolved["payload"]["decisions"], json!(["approve"]));
    // The reply is accepted after its resolved line.
    assert_eq!(
        answer(setup.deadline, &client, "c_r1")["kind"],
        "command_accepted"
    );
    recv_kind(setup.deadline, &client, "turn_completed");
    let approval = fs::read_to_string(setup.home().join("approvals").join(&hash)).unwrap();
    assert!(approval.contains(r#""decision":"approve""#), "{approval}");

    reply(&client, "c_r2", &request, &["approve"]);
    let stale = answer(setup.deadline, &client, "c_r2");
    assert_eq!(stale["kind"], "command_rejected");
    assert_eq!(stale["payload"]["code"], "stale_request");

    close(&client, "c_close");
    recv_kind(setup.deadline, &client, "fiber_exited");
    drop(client);
    assert!(session.wait().success());
    assert_eq!(server.requests().len(), 1);
    // The server approved in the session's own offer starts before the
    // first model request: the preamble declares its tools.
    let text = fs::read_to_string(session_dir(&setup, &id).join("events.jsonl")).unwrap();
    let built = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    let tools: Vec<&str> = built["payload"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(tools.contains(&"mcp__db__echo"), "{tools:?}");
    // The complete, ordered durable kinds: the offer is raised and resolved
    // before the preamble, then the turn runs.
    assert_eq!(
        durable_kinds(&setup, &id)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "repository_code_offered",
            "repository_code_resolved",
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
    );
}

#[test]
fn an_extensions_replacement_of_an_mcp_tool_survives_the_offer() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call("call_echo", "mcp__db__echo", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    declare_early(&setup);
    setup.lua(
        "shadow",
        "fiber.tool(\"mcp__db__echo\", { description = \"The shadow tool.\", input_schema = { type = \"object\" }, timeout = 2000, effects = { effects = { \"reads\" }, reversible = true }, run = function() return \"from the extension\" end })\n",
    );
    declare_db(&setup, &json!({}));
    let id = doors::mint("s_");
    let (session, stdout) = spawn_session(&setup, &id, &[]);
    started(setup.deadline, stdout);
    let client = attach(&setup, &id, "c_sub");
    prompt(&client, "c_prompt");

    let offered = recv_kind(setup.deadline, &client, "repository_code_offered");
    let request = offered["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let items = offered["payload"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "mcp_server");
    assert_eq!(items[0]["name"], "db");
    reply(&client, "c_r1", &request, &["approve"]);
    let resolved = recv_kind(setup.deadline, &client, "repository_code_resolved");
    assert_eq!(resolved["payload"]["decisions"], json!(["approve"]));
    recv_kind(setup.deadline, &client, "turn_completed");
    // The `tools` answer names the extension as the source of the
    // tool it replaced, and keeps the other rows' sources.
    client
        .send(r#"{"id":"c_tools","command":"tools"}"#)
        .unwrap();
    let answered = answer(setup.deadline, &client, "c_tools");
    assert_eq!(answered["kind"], "command_accepted", "{answered}");
    let rows = answered["payload"]["result"]["tools"].as_array().unwrap();
    let shadow = rows
        .iter()
        .find(|tool| tool["name"] == "mcp__db__echo")
        .unwrap();
    assert_eq!(shadow["source"], "extension", "{shadow}");
    assert_eq!(shadow["extension"], "fiber.test/shadow", "{shadow}");
    let early = rows
        .iter()
        .find(|tool| tool["name"] == "mcp__early__echo")
        .unwrap();
    assert_eq!(early["source"], "mcp", "{early}");
    assert_eq!(early["server"], "early", "{early}");
    close(&client, "c_close");
    recv_kind(setup.deadline, &client, "fiber_exited");
    drop(client);
    assert!(session.wait().success());
    // The extension's replacement answers the model's call: the
    // post-offer load left the registered name alone.
    assert_eq!(outputs_sent(&server), ["from the extension"]);
    let text = fs::read_to_string(session_dir(&setup, &id).join("events.jsonl")).unwrap();
    let built = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert!(built["payload"].get("replaced").is_none());
    let tools = built["payload"]["tools"].as_array().unwrap();
    let shadow = tools
        .iter()
        .find(|tool| tool["name"] == "mcp__db__echo")
        .unwrap();
    assert_eq!(shadow["registered_by"], "fiber.test/shadow");
    let early = tools
        .iter()
        .find(|tool| tool["name"] == "mcp__early__echo")
        .unwrap();
    assert_eq!(early["registered_by"], "early");
    // The first request declares both: the replacement and the tool
    // the server loaded before the offer.
    let first: Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    let names: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"mcp__db__echo"), "{names:?}");
    assert!(names.contains(&"mcp__early__echo"), "{names:?}");
    // The complete, ordered durable kinds: the offer is raised and
    // resolved before the preamble, then the tool step runs the
    // replacement and the text step answers.
    assert_eq!(
        durable_kinds(&setup, &id)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "repository_code_offered",
            "repository_code_resolved",
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
}

#[test]
fn fiber_ask_skips_an_unapproved_server_with_a_notice() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({}));
    let (code, lines, stderr) = ask(&setup);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(of(&lines, "repository_code_offered").is_empty());
    let notices = of(&lines, "notice");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["payload"]["code"], "repository_code_skipped");
    let message = notices[0]["payload"]["message"].as_str().unwrap();
    assert!(message.contains("`db`"), "{message}");
    assert!(message.contains("fiber approve"), "{message}");
    assert!(
        !of(&lines, "session_status").is_empty(),
        "a session_status on stdout"
    );
    // The complete, ordered stdout kinds: one skip notice, then the turn.
    assert_eq!(
        stdout_kinds(&lines),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "notice",
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
}

#[test]
fn fiber_ask_fails_on_a_required_server_nobody_approved() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({"required": true}));
    let (code, lines, stderr) = ask(&setup);
    assert_eq!(code, Some(1), "stderr: {stderr}");
    let exited = lines.last().unwrap();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["error"]["code"], "mcp_server_unapproved");
    assert!(stderr.contains("`db`"), "{stderr}");
    assert!(server.requests().is_empty());
    assert!(
        !of(&lines, "session_status").is_empty(),
        "a session_status on stdout"
    );
    // The complete, ordered stdout kinds: no turn, the run fails.
    assert_eq!(
        stdout_kinds(&lines),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ]
    );
}

#[test]
fn fiber_ask_fails_on_a_required_repository_extension_nobody_approved() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let workspace = setup.workspace();
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(&workspace)
        .status()
        .unwrap();
    assert!(status.success());
    write_json(
        &workspace.join("pkg/extension.json"),
        &json!({"name": "fiber.test/pkg", "version": "v1.0.0", "fiber": "0.1.0", "api": 1}),
    );
    fs::write(workspace.join("pkg/init.lua"), "-- entry\n").unwrap();
    write_json(
        &workspace.join(".fiber/config.json"),
        &json!({"repository_extensions": [{"path": "pkg", "required": true}]}),
    );
    let (code, lines, stderr) = ask(&setup);
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert_eq!(
        lines.last().unwrap()["payload"]["error"]["code"],
        "extension_unapproved"
    );
    assert!(server.requests().is_empty());
    assert!(
        !of(&lines, "session_status").is_empty(),
        "a session_status on stdout"
    );
    // The complete, ordered stdout kinds: no turn, the run fails.
    assert_eq!(
        stdout_kinds(&lines),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ]
    );
}

#[test]
fn fiber_ask_after_fiber_approve_loads_without_a_notice() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({"required": true}));
    let approved = run_to_exit(
        setup.deadline,
        "fiber approve",
        in_workspace(&setup, &["approve", "--yes"]),
    );
    assert!(approved.status.success());
    let (code, lines, stderr) = ask(&setup);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(of(&lines, "notice").is_empty());
    assert_eq!(server.requests().len(), 1);
    assert!(
        !of(&lines, "session_status").is_empty(),
        "a session_status on stdout"
    );
    // The complete, ordered stdout kinds: approved, so no notice.
    assert_eq!(
        stdout_kinds(&lines),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
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
}

/// The `seq` of a line, 0 when it has none.
fn seq(line: &Value) -> u64 {
    line["seq"].as_u64().unwrap_or(0)
}

/// The kinds of every durable line in the session `id`'s log, in order.
fn durable_kinds(setup: &Setup, id: &str) -> Vec<String> {
    let text = fs::read_to_string(session_dir(setup, id).join("events.jsonl")).unwrap();
    text.lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The kinds of `fiber ask`'s stdout `lines`, in order, without
/// `session_status`: an observer thread writes it, so where it falls among
/// the loop's own lines is not what these tests pin.
fn stdout_kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

#[test]
fn a_session_that_exits_on_its_first_offer_raises_it_again_on_resume() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({}));
    let id = doors::mint("s_");
    let (session, stdout) = spawn_session(&setup, &id, &[]);
    started(setup.deadline, stdout);
    let client = attach(&setup, &id, "c_sub");
    prompt(&client, "c_prompt");
    let offered = recv_kind(setup.deadline, &client, "repository_code_offered");
    let request = offered["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();

    session.kill("TERM");
    let exited = recv_kind(setup.deadline, &client, "fiber_exited");
    assert_eq!(exited["payload"]["suspended_on"], request.as_str());
    drop(client);
    session.wait();
    assert!(
        session_dir(&setup, &id).exists(),
        "a session with an offer keeps its directory"
    );

    let (resumed, stdout) = spawn_session(&setup, &id, &["--resume"]);
    started(setup.deadline, stdout);
    let client = Client::connect(&setup.session_socket(&id)).unwrap();
    client
        .send(r#"{"id":"c_sub2","command":"subscribe","args":{"level":"full"}}"#)
        .unwrap();
    // The replay holds the old offer; this process's `fiber_started` is the
    // latest one before the client is counted.
    let since = Cell::new(0);
    client
        .recv_until(setup.deadline, |line| {
            if line["kind"] == "fiber_started" {
                since.set(seq(line));
            }
            line["kind"] == "clients" && line["payload"]["count"] == 1
        })
        .expect("the replay and the clients line within the deadline");
    let since = since.get();
    prompt(&client, "c_prompt2");
    let again = client
        .recv_until(setup.deadline, |line| {
            line["kind"] == "repository_code_offered" && seq(line) > since
        })
        .expect("the offer raised again within the deadline");
    assert_eq!(again["payload"]["request_id"], request.as_str());
    reply(&client, "c_r1", &request, &["approve"]);
    recv_kind(setup.deadline, &client, "repository_code_resolved");
    recv_kind(setup.deadline, &client, "turn_completed");
    close(&client, "c_close2");
    recv_kind(setup.deadline, &client, "fiber_exited");
    drop(client);
    assert!(resumed.wait().success());
    assert_eq!(server.requests().len(), 1);
    // The complete, ordered durable kinds: the first process exits on its
    // offer, the resume raises it again, resolves it, then runs the turn.
    assert_eq!(
        durable_kinds(&setup, &id)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "repository_code_offered",
            "fiber_exited",
            "fiber_started",
            "extensions_loaded",
            "repository_code_offered",
            "repository_code_resolved",
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
    );
}

/// Ends a session started through the hub: kills the hub, closes the
/// session on its own socket and waits for its processes to exit.
#[track_caller]
fn end_through_hub(
    setup: &Setup,
    hub: &Arc<Mutex<Option<HubProc>>>,
    id: &str,
    guard: SessionGuard,
) {
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    assert!(!hub.kill_and_wait().success(), "SIGKILL ends the hub");
    let direct = Socket::connect(setup.deadline, &setup.session_socket(id));
    close_session(&direct);
    drop(direct);
    guard.wait_gone();
}

#[test]
fn a_client_that_starts_with_content_through_the_hub_is_offered_the_code() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({}));
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let id = start_session(&client, &workspace, "hi");
    subscribe(&client, &id);
    let lines = until(&client, "repository_code_offered", |line| {
        line["kind"] == "repository_code_offered"
    });
    let offered = lines.last().unwrap();
    let request = offered["payload"]["request_id"].as_str().unwrap();
    client.send(&format!(
        r#"{{"id":"c_reply","session_id":"{id}","command":"reply","args":{{"request_id":"{request}","decisions":["approve"]}}}}"#
    ));
    until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    drop(client);
    let kinds = durable_kinds(&setup, &id);
    let at = |kind: &str| {
        kinds
            .iter()
            .position(|seen| seen == kind)
            .unwrap_or_else(|| panic!("no {kind} in {kinds:?}"))
    };
    assert!(at("repository_code_offered") < at("repository_code_resolved"));
    assert!(at("repository_code_resolved") < at("preamble_built"));
    end_through_hub(&setup, &hub, &id, guard);
}

#[test]
fn a_start_with_content_that_never_subscribes_runs_unattended() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({}));
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let id = start_session(&client, &workspace, "hi");
    // The offer is decided once, before the first model request: by then
    // the session has decided with nobody to answer.
    assert!(
        server.await_requests(1, setup.deadline),
        "the turn's model request within the deadline"
    );
    subscribe(&client, &id);
    let lines = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let kinds: Vec<&str> = lines
        .iter()
        .filter_map(|line| line["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"preamble_built"), "{kinds:?}");
    for kind in ["repository_code_offered", "repository_code_resolved"] {
        assert!(!kinds.contains(&kind), "{kind} in {kinds:?}");
    }
    drop(client);
    let durable = durable_kinds(&setup, &id);
    assert!(
        durable.iter().any(|kind| kind == "turn_completed"),
        "{durable:?}"
    );
    for kind in ["repository_code_offered", "repository_code_resolved"] {
        assert!(
            !durable.iter().any(|seen| seen == kind),
            "{kind} in {durable:?}"
        );
    }
    end_through_hub(&setup, &hub, &id, guard);
}
