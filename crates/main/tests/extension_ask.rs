//! Binary-level tests of `host.ask` through a live session
//! (`docs/extensions.md`, "Commands and screens"): the built `fiber` runs
//! in its own process group with its own `FIBER_HOME`, holding an ordinary
//! provider and the extensions under test. Every wait for a socket line
//! uses one named deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;

use std::fs;
use std::process::ExitStatus;

use extension_harness::*;
use fakes::Client;
use fakes::ProviderServer;
use fakes::Watchdog;
use serde_json::{Value, json};

/// How long one socket line may take.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// An `openai-responses` function call for `shell`.
fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
}

fn subscribe(client: &Client) {
    client
        .send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#)
        .unwrap();
}

fn commanded(client: &Client, id: &str, name: &str) {
    client
        .send(&format!(
            r#"{{"id":"{id}","command":"command","args":{{"name":"{name}"}}}}"#
        ))
        .unwrap();
    client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == id
        })
        .expect("the command was admitted");
}

fn requested(client: &Client) -> Value {
    client
        .recv_until(DEADLINE, |line| line["kind"] == "interaction_requested")
        .expect("the question arrives")
}

fn ask_command() -> String {
    "fiber.command(\"askform\", { timeout = 8000, run = function(text)\nlocal answer = host.ask(\"form\", { fields = {\n{ header = \"Model\", question = \"Which?\", options = {{ label = \"a\" }, { label = \"b\" }} },\n{ header = \"Other\", question = \"What?\", options = {{ label = \"x\" }} } } })\nhost.status(json.encode(answer))\nend })\n"
    .to_owned()
}

#[test]
fn a_command_raising_a_form_resolves_through_reply() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua("worker", &ask_command());
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect_client(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(&client);
    commanded(&client, "c_1", "askform");
    let asked = requested(&client);
    assert_eq!(asked["payload"]["extension"], "fiber.test/worker");
    assert_eq!(asked["payload"]["fields"].as_array().unwrap().len(), 2);
    let request_id = asked["payload"]["request_id"].as_str().unwrap().to_owned();
    client
        .send(&format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request_id}","answers":[{{"labels":["a"]}},{{"skipped":true}}],"note":"hi"}}}}"#
        ))
        .unwrap();
    // `command_accepted` comes only after the client has read the
    // resolution carrying those answers.
    let resolved = client
        .recv_until(DEADLINE, |line| line["kind"] == "interaction_resolved")
        .expect("the resolution arrives");
    assert_eq!(resolved["payload"]["request_id"], request_id);
    assert_eq!(resolved["payload"]["by"], "person");
    assert_eq!(resolved["payload"]["answers"][0]["labels"], json!(["a"]));
    assert_eq!(resolved["payload"]["note"], "hi");
    client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_reply"
        })
        .expect("the reply was accepted");
    let ui = client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
        })
        .expect("the answer reaches host.status");
    let status: Value = serde_json::from_str(ui["payload"]["status"].as_str().unwrap()).unwrap();
    assert_eq!(status["answers"][0]["labels"], json!(["a"]));
    assert_eq!(status["note"], "hi");
    client
        .send(r#"{"id":"c_close","command":"close"}"#)
        .unwrap();
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert!(
        out.iter()
            .any(|line| line["kind"] == "interaction_resolved"),
        "the resolution is in the log"
    );
}

#[test]
fn an_unfit_reply_is_rejected_then_a_fitting_one_resolves_and_a_second_is_stale() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua("worker", &ask_command());
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect_client(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(&client);
    commanded(&client, "c_1", "askform");
    let asked = requested(&client);
    let request_id = asked["payload"]["request_id"].as_str().unwrap().to_owned();
    // Another kind's answer keys do not fit a `form`.
    client
        .send(&format!(
            r#"{{"id":"c_2","command":"reply","args":{{"request_id":"{request_id}","confirmed":true}}}}"#
        ))
        .unwrap();
    let rejected = client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_2"
        })
        .expect("the unfit reply is rejected");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    // The request stays pending: nothing resolves it yet.
    assert!(
        client
            .recv_until(std::time::Duration::from_secs(2), |line| {
                line["kind"] == "interaction_resolved"
            })
            .is_none(),
        "the request stays pending after an unfit reply"
    );
    client
        .send(&format!(
            r#"{{"id":"c_3","command":"reply","args":{{"request_id":"{request_id}","answers":[{{"labels":["b"]}},{{"labels":["x"]}}]}}}}"#
        ))
        .unwrap();
    let resolved = client
        .recv_until(DEADLINE, |line| line["kind"] == "interaction_resolved")
        .expect("the fitting reply resolves");
    assert_eq!(resolved["payload"]["request_id"], request_id);
    client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_3"
        })
        .expect("the fitting reply was accepted");
    // The request is resolved: one more reply is stale.
    client
        .send(&format!(
            r#"{{"id":"c_4","command":"reply","args":{{"request_id":"{request_id}","answers":[{{"labels":["b"]}},{{"labels":["x"]}}]}}}}"#
        ))
        .unwrap();
    let stale = client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_4"
        })
        .expect("the second reply is rejected");
    assert_eq!(stale["payload"]["code"], "stale_request");
    client
        .send(r#"{"id":"c_close","command":"close"}"#)
        .unwrap();
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

/// Runs `fiber ask` with `args` to completion and returns its exit status,
/// stdout lines and stderr. `fiber` runs in its own process group under a
/// watchdog, and the group is reaped and checked empty under [`DEADLINE`],
/// including after a timeout (`docs/testing.md`, "Running tests").
fn run_ask(setup: &Setup, args: &[&str]) -> (ExitStatus, Vec<Value>, String) {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let (child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(DEADLINE) {
        Ok(output) => output.unwrap(),
        Err(_) => {
            fakes::kill_group(group, "KILL").unwrap();
            // Reaps the killed child, so the check below sees the group
            // as the kill left it.
            let reaped = finished.recv_timeout(DEADLINE).is_ok();
            assert!(
                !group_alive(group),
                "`fiber` left a process in its group behind"
            );
            panic!("waited {DEADLINE:?} for fiber ask to exit (reaped after the kill: {reaped})");
        }
    };
    assert!(
        !group_alive(group),
        "`fiber` left a process in its group behind"
    );
    // The group is empty. Skip the drop, which would kill it again,
    // and tell the watchdog to exit without signalling.
    std::mem::forget(guard);
    watchdog.stand_down(DEADLINE);
    let status = output.status;
    let stderr = String::from_utf8(output.stderr).unwrap();
    let out = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (status, out, stderr)
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group (see `Setup::fiber` in the shared harness, which already sets
/// `process_group(0)`; as the sibling tests do).
fn spawn_watched(command: &mut std::process::Command) -> (std::process::Child, Watchdog) {
    use std::os::unix::process::CommandExt;
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    // A failed watchdog spawn still kills the child on unwind.
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

#[test]
fn fiber_ask_declines_a_hook_ask_and_writes_no_question() {
    // `turn_start` has no call site yet: only `after_tool` hooks run, so
    // the hook below answers a tool call instead, naming the answer in
    // the content the provider sees next.
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
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    setup.lua(
        "worker",
        "fiber.hook(\"after_tool\", { timeout = 8000, on_failure = \"blocking\", run = function(call)\nlocal answer = host.ask(\"confirm\", { prompt = \"go?\" })\nreturn { content = \"declined=\" .. tostring(answer.declined) }\nend })\n",
    );
    let (status, out, stderr) = run_ask(&setup, &["ask", "hi"]);
    assert!(status.success(), "stderr: {stderr}");
    assert!(
        !out.iter()
            .any(|line| line["kind"] == "interaction_requested"),
        "no question is written under fiber ask"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        String::from_utf8_lossy(&requests[1].body).contains("declined=true"),
        "the hook saw declined at once"
    );
}

#[test]
fn close_while_an_ask_is_pending_declines_it_by_fiber() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua(
        "worker",
        "fiber.command(\"slowask\", { timeout = 8000, run = function(text)\nreturn host.ask(\"confirm\", { prompt = \"go?\" })\nend })\n",
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect_client(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(&client);
    commanded(&client, "c_1", "slowask");
    let asked = requested(&client);
    let request_id = asked["payload"]["request_id"].as_str().unwrap().to_owned();
    client
        .send(r#"{"id":"c_close","command":"close"}"#)
        .unwrap();
    let exited = client
        .recv_until(DEADLINE, |line| line["kind"] == "fiber_exited")
        .expect("the session exits");
    assert!(exited.get("suspended_on").is_none());
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let kept: Vec<&Value> = out
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .collect();
    let tail: Vec<&str> = kept
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let at = tail
        .iter()
        .position(|kind| *kind == "interaction_requested")
        .expect("the question is in the log");
    assert_eq!(
        tail[at..],
        [
            "interaction_requested",
            "interaction_resolved",
            "fiber_exited"
        ]
    );
    let resolved = kept[at + 1];
    assert_eq!(resolved["payload"]["request_id"], request_id);
    assert_eq!(resolved["payload"]["by"], "fiber");
    assert_eq!(resolved["payload"]["declined"], true);
}

#[test]
fn a_resumed_interactive_session_answers_a_host_ask() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.lua(
        "worker",
        "fiber.command(\"slowask\", { timeout = 8000, run = function(text)\nlocal answer = host.ask(\"confirm\", { prompt = \"go?\" })\nhost.status(tostring(answer.confirmed))\nend })\n",
    );
    // An exited session to resume.
    let (status, out, stderr) = run_ask(&setup, &["ask", "hi"]);
    assert!(status.success(), "stderr: {stderr}");
    let id = out[0]["session_id"].as_str().unwrap().to_owned();
    // A resumed session serving clients is answerable: the ask reaches a
    // connected client instead of declining at once.
    let mut running = setup.start_session(&id, &["--resume"]);
    let client = running.connect_client(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(&client);
    commanded(&client, "c_1", "slowask");
    let asked = requested(&client);
    let request_id = asked["payload"]["request_id"].as_str().unwrap().to_owned();
    client
        .send(&format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request_id}","confirmed":true}}}}"#
        ))
        .unwrap();
    let resolved = client
        .recv_until(DEADLINE, |line| line["kind"] == "interaction_resolved")
        .expect("the answer resolves");
    assert_eq!(resolved["payload"]["request_id"], request_id);
    assert_eq!(resolved["payload"]["by"], "person");
    assert!(
        resolved["payload"].get("declined").is_none(),
        "the resumed ask was answered, not declined: {resolved}"
    );
    client
        .recv_until(DEADLINE, |line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_reply"
        })
        .expect("the reply was accepted");
    let ui = client
        .recv_until(DEADLINE, |line| line["kind"] == "extension_ui")
        .expect("the answer reaches the command");
    assert_eq!(ui["payload"]["status"], "true");
    client
        .send(r#"{"id":"c_close","command":"close"}"#)
        .unwrap();
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn a_resumed_one_turn_session_declines_a_host_ask_at_once() {
    // As `fiber_ask_declines_a_hook_ask_and_writes_no_question`, but on a
    // resumed session: `fiber ask --resume` runs one turn with no client,
    // so the hook's ask declines at once and writes no question.
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "echo hi"}),
        )]),
        hello(),
        stream(&[function_call(
            "call_2",
            "shell",
            &json!({"command": "echo hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    setup.lua(
        "worker",
        "fiber.hook(\"after_tool\", { timeout = 8000, on_failure = \"blocking\", run = function(call)\nlocal answer = host.ask(\"confirm\", { prompt = \"go?\" })\nreturn { content = \"declined=\" .. tostring(answer.declined) }\nend })\n",
    );
    let (status, out, stderr) = run_ask(&setup, &["ask", "hi"]);
    assert!(status.success(), "stderr: {stderr}");
    let id = out[0]["session_id"].as_str().unwrap().to_owned();
    let (status, out, stderr) = run_ask(&setup, &["ask", "--resume", &id, "hi"]);
    assert!(status.success(), "stderr: {stderr}");
    assert!(
        out.iter()
            .any(|line| line["kind"] == "fiber_started" && line["payload"]["resumed"] == true),
        "the second run resumed the session"
    );
    assert!(
        !out.iter()
            .any(|line| line["kind"] == "interaction_requested"),
        "no question is written on the resumed one-turn run"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        String::from_utf8_lossy(&requests[3].body).contains("declined=true"),
        "the resumed hook saw declined at once"
    );
}
