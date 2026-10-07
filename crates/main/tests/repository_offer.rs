//! The offer of a repository's code, end to end (`docs/extensions.md`,
//! "Code a repository ships"): a session with a `full` client raises it and
//! takes the client's `decisions`; `fiber ask` has nobody to ask, so it skips
//! each item with a notice or fails on a required one; a session that exits
//! on its first offer is kept and raises the offer again on resume.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::Client;
use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{DEADLINE, HubProc, Setup, hello, run_to_exit, write_json};

/// Declares the MCP server `db` in the workspace's repository file.
fn declare_db(setup: &Setup, extra: &Value) {
    let mut entry = json!({"command": "/bin/echo"});
    for (key, value) in extra.as_object().unwrap() {
        entry[key] = value.clone();
    }
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": entry}}}),
    );
}

/// `fiber <args>` run in the workspace.
fn in_workspace(setup: &Setup, args: &[&str]) -> Command {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    command
}

/// `fiber ask hi` in the workspace: its exit code, stdout lines and stderr.
fn ask(setup: &Setup) -> (Option<i32>, Vec<Value>, String) {
    let output = run_to_exit("fiber ask", in_workspace(setup, &["ask", "hi"]));
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
        },
        lines,
    )
}

/// Waits for the session's `extensions_loaded` on stdout: every startup
/// line is written and the socket is listening.
fn started(lines: &mpsc::Receiver<Value>) {
    loop {
        let line = lines
            .recv_timeout(DEADLINE)
            .expect("the session's extensions_loaded within the deadline");
        if line["kind"] == "extensions_loaded" {
            return;
        }
    }
}

/// A `full` client on the session `id`, once the session counts it.
fn attach(setup: &Setup, id: &str, sub: &str) -> Client {
    let client = Client::connect(&setup.session_socket(id)).unwrap();
    client
        .send(&format!(
            r#"{{"id":"{sub}","command":"subscribe","args":{{"level":"full"}}}}"#
        ))
        .unwrap();
    client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "clients" && line["payload"]["count"] == 1
        })
        .expect("the session counts the client within the deadline");
    client
}

fn recv_kind(client: &Client, kind: &str) -> Value {
    client
        .recv_until(DEADLINE, |line| line["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} within {DEADLINE:?}"))
}

/// The answer to the command `id`.
fn answer(client: &Client, id: &str) -> Value {
    client
        .recv_until(DEADLINE, |line| {
            (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
                && line["payload"]["command_id"] == id
        })
        .unwrap_or_else(|| panic!("no answer to {id} within {DEADLINE:?}"))
}

fn prompt(client: &Client, id: &str) {
    client
        .send(&format!(
            r#"{{"id":"{id}","command":"prompt","args":{{"content":[{{"type":"text","text":"hi"}}]}}}}"#
        ))
        .unwrap();
}

fn reply(client: &Client, id: &str, request: &str, decisions: &[&str]) {
    client
        .send(&format!(
            r#"{{"id":"{id}","command":"reply","args":{{"request_id":"{request}","decisions":{}}}}}"#,
            json!(decisions)
        ))
        .unwrap();
}

fn close(client: &Client, id: &str) {
    client
        .send(&format!(r#"{{"id":"{id}","command":"close"}}"#))
        .unwrap();
}

#[test]
fn a_full_client_answers_the_offer_and_the_turn_runs() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({}));
    let id = doors::mint("s_");
    let (session, stdout) = spawn_session(&setup, &id, &[]);
    started(&stdout);
    let client = attach(&setup, &id, "c_sub");
    prompt(&client, "c_prompt");

    let offered = recv_kind(&client, "repository_code_offered");
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
        .recv_until(DEADLINE, |line| {
            line["kind"] == "session_status" && line["payload"]["state"] == "waiting"
        })
        .expect("a waiting session_status within the deadline");
    assert_eq!(status["payload"]["waiting"]["kind"], "offer");
    assert_eq!(status["payload"]["waiting"]["request_id"], request.as_str());

    reply(&client, "c_r0", &request, &["approve", "approve"]);
    let rejected = answer(&client, "c_r0");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");

    reply(&client, "c_r1", &request, &["approve"]);
    let resolved = recv_kind(&client, "repository_code_resolved");
    assert_eq!(resolved["payload"]["decisions"], json!(["approve"]));
    // The reply is accepted after its resolved line.
    assert_eq!(answer(&client, "c_r1")["kind"], "command_accepted");
    recv_kind(&client, "turn_completed");
    let approval = fs::read_to_string(setup.home().join("approvals").join(&hash)).unwrap();
    assert!(approval.contains(r#""decision":"approve""#), "{approval}");

    reply(&client, "c_r2", &request, &["approve"]);
    let stale = answer(&client, "c_r2");
    assert_eq!(stale["kind"], "command_rejected");
    assert_eq!(stale["payload"]["code"], "stale_request");

    close(&client, "c_close");
    recv_kind(&client, "fiber_exited");
    drop(client);
    assert!(session.wait().success());
    assert_eq!(server.requests().len(), 1);
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
}

#[test]
fn fiber_ask_after_fiber_approve_loads_without_a_notice() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({"required": true}));
    let approved = run_to_exit("fiber approve", in_workspace(&setup, &["approve", "--yes"]));
    assert!(approved.status.success());
    let (code, lines, stderr) = ask(&setup);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(of(&lines, "notice").is_empty());
    assert_eq!(server.requests().len(), 1);
}

/// The `seq` of a line, 0 when it has none.
fn seq(line: &Value) -> u64 {
    line["seq"].as_u64().unwrap_or(0)
}

fn session_dir(setup: &Setup, id: &str) -> PathBuf {
    log::sessions_dir(&setup.home(), &doors::project(&setup.workspace())).join(id)
}

#[test]
fn a_session_that_exits_on_its_first_offer_raises_it_again_on_resume() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare_db(&setup, &json!({}));
    let id = doors::mint("s_");
    let (session, stdout) = spawn_session(&setup, &id, &[]);
    started(&stdout);
    let client = attach(&setup, &id, "c_sub");
    prompt(&client, "c_prompt");
    let offered = recv_kind(&client, "repository_code_offered");
    let request = offered["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();

    session.kill("TERM");
    let exited = recv_kind(&client, "fiber_exited");
    assert_eq!(exited["payload"]["suspended_on"], request.as_str());
    drop(client);
    session.wait();
    assert!(
        session_dir(&setup, &id).exists(),
        "a session with an offer keeps its directory"
    );

    let (resumed, stdout) = spawn_session(&setup, &id, &["--resume"]);
    started(&stdout);
    let client = Client::connect(&setup.session_socket(&id)).unwrap();
    client
        .send(r#"{"id":"c_sub2","command":"subscribe","args":{"level":"full"}}"#)
        .unwrap();
    // The replay holds the old offer; this process's `fiber_started` is the
    // latest one before the client is counted.
    let mut since = 0;
    loop {
        let line = client
            .recv(DEADLINE)
            .expect("the replay and the clients line within the deadline");
        if line["kind"] == "fiber_started" {
            since = seq(&line);
        }
        if line["kind"] == "clients" && line["payload"]["count"] == 1 {
            break;
        }
    }
    prompt(&client, "c_prompt2");
    let again = client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "repository_code_offered" && seq(line) > since
        })
        .expect("the offer raised again within the deadline");
    assert_eq!(again["payload"]["request_id"], request.as_str());
    reply(&client, "c_r1", &request, &["approve"]);
    recv_kind(&client, "repository_code_resolved");
    recv_kind(&client, "turn_completed");
    close(&client, "c_close2");
    recv_kind(&client, "fiber_exited");
    drop(client);
    assert!(resumed.wait().success());
    assert_eq!(server.requests().len(), 1);
}
