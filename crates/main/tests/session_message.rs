//! `session_message` between real sessions: a `fiber ask` sender reaches a
//! hub-started session, and a hub-started session messages itself.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures and assertions"
)]
mod extension_harness;
mod support;

use fakes::{ProviderServer, Request, Response};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use support::{Setup, Socket, function_call, hello};

/// Installs provider `pa/m` on `openai-responses` at the server, and makes
/// it the session model and the reviewer, with idle exit far out so an idle
/// hub session stays up for the whole test.
fn install(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src-pa");
    let url = server.url();
    let mut manifest = json!({"name": "pa", "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = Value::Object(
        [(String::from("pa"), json!([format!("{url}/v1")]))]
            .into_iter()
            .collect(),
    );
    support::write_json(&source.join("extension.json"), &manifest);
    support::write_json(
        &source.join("providers/pa.json"),
        &json!({
            "name": "pa",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{url}/v1"), "context_window": 100000}]
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
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000}}),
    );
}

fn is_session_request(body: &[u8]) -> bool {
    String::from_utf8_lossy(body).contains(r#""type":"function_call""#)
        || serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|body| {
                body["tools"].as_array().map(|tools| {
                    tools.iter().any(|tool| {
                        tool["name"]
                            .as_str()
                            .is_some_and(|name| name == "session_message")
                    })
                })
            })
            .unwrap_or(false)
}

fn allow() -> Response {
    support::text_reply("allow")
}

fn message_call(id: &str, to: &str, text: &str) -> Value {
    function_call(id, "session_message", &json!({"id": to, "text": text}))
}

/// The `tool_call_started`, `permission_resolved` and `tool_call_completed`
/// lines for the session_message call whose request names `target`, in the
/// sender's lines.
fn call_lines(lines: &[Value], target: &str) -> (Value, Value, Value) {
    let requested = lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_requested"
                && line["payload"]["name"] == "session_message"
                && line["payload"]["arguments"]["id"] == target
        })
        .unwrap_or_else(|| panic!("no session_message call to {target}"))
        .clone();
    let action = requested["action_id"].as_str().unwrap();
    let started = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started" && line["action_id"] == action)
        .unwrap_or_else(|| panic!("no start for the call to {target}"))
        .clone();
    let resolved = lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved" && line["action_id"] == action)
        .unwrap_or_else(|| panic!("no review for the call to {target}"))
        .clone();
    let completed = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == action)
        .unwrap_or_else(|| panic!("no completion for the call to {target}"))
        .clone();
    (started, resolved, completed)
}

fn completed_text(completed: &Value) -> &str {
    assert_eq!(completed["payload"]["status"], "completed", "{completed}");
    completed["payload"]["content"][0]["text"].as_str().unwrap()
}

fn failed_code(completed: &Value) -> (&str, &str) {
    assert_eq!(completed["payload"]["status"], "failed", "{completed}");
    (
        completed["payload"]["error"]["code"].as_str().unwrap(),
        completed["payload"]["error"]["message"].as_str().unwrap(),
    )
}

fn read_log(setup: &Setup, id: &str) -> Vec<Value> {
    let text = fs::read_to_string(support::session_dir(setup, id).join("events.jsonl")).unwrap();
    support::complete_lines(&text)
}

#[test]
fn a_fiber_ask_session_messages_a_hub_started_session_and_a_fiber_ask_target_is_closing() {
    let setup = Setup::new();
    let deadline = setup.deadline;
    let _guard = support::SessionGuard::arm(deadline, &setup.root.path().to_string_lossy());
    let t_prompt = "sit idle and wait for a message";
    let b_prompt = "hold the line open for a message";
    let e_prompt = "say goodbye and exit";
    let s_prompt = "send four session messages to the other sessions";
    let b_cell = Arc::new(Mutex::new(None::<String>));
    let e_id = Arc::new(Mutex::new(None::<String>));
    let random_id = doors::mint("s_");
    let responder_b_id = Arc::clone(&b_cell);
    let responder_e_id = Arc::clone(&e_id);
    let responder_random_id = random_id.clone();
    let responder_t_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let responder_t_probe = Arc::clone(&responder_t_id);
    let server = ProviderServer::start_responding(move |request: &Request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        // Reviewer requests carry no tools: every send is reviewed and allowed.
        if body["tools"].as_array().is_none_or(|tools| tools.is_empty()) {
            return allow();
        }
        let input = body["input"].as_array().unwrap();
        let text = serde_json::to_string(input).unwrap();
        if text.contains(t_prompt) {
            return hello();
        }
        if text.contains(b_prompt) {
            return Response::stall(200, "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"...\"}\n\n", 100_100)
                .header("content-type", "text/event-stream");
        }
        if text.contains(e_prompt) {
            return hello();
        }
        assert!(text.contains(s_prompt), "the sender's request: {text}");
        if text.contains(&responder_random_id) {
            return hello();
        }
        if let Some(e) = responder_e_id.lock().unwrap().clone()
            && text.contains(&e)
        {
            return support::stream(&[message_call("call_random", &responder_random_id, "ping")]);
        }
        if text.contains("closing") {
            let e = responder_e_id.lock().unwrap().clone().expect("E ran first");
            return support::stream(&[message_call("call_e", &e, "ping")]);
        }
        if text.contains("delivered") {
            let b = responder_b_id.lock().unwrap().clone().expect("B started");
            return support::stream(&[message_call("call_b", &b, "ping")]);
        }
        let t = responder_t_probe.lock().unwrap().clone().expect("T started");
        support::stream(&[message_call("call_t", &t, "ping from S")])
    })
    .unwrap();
    install(&setup, &server);
    let hub = Arc::new(Mutex::new(None));
    let (main, _) = support::connect_hub(&setup, &hub);
    // The hub starts T, which answers its prompt and goes idle.
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let t_id = support::start_session(&main, &workspace, t_prompt);
    *responder_t_id.lock().unwrap() = Some(t_id.clone());
    // A `full` subscription on T's socket, held for the whole test.
    let t_socket = Socket::connect(deadline, &setup.session_socket(&t_id));
    let (t_tx, t_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut lines = Vec::new();
        t_socket.send(
            &json!({"id":"c_full","command":"subscribe","args":{"level":"full"}}).to_string(),
        );
        while let Some(line) = t_socket.next("T's next line", &lines) {
            t_tx.send(line.clone()).unwrap_or(());
            lines.push(line);
        }
    });
    // T's first turn ends: it is idle.
    let mut t_lines = Vec::new();
    loop {
        let line: Value = deadline.recv_or_fail(&t_rx, "T's first turn ends");
        let done = line["kind"] == "turn_completed";
        t_lines.push(line);
        if done {
            break;
        }
    }
    // E runs to completion first, so its id names an exited session.
    let e_run = setup.fiber(&["ask", e_prompt]);
    let e_out = support::run_to_exit(deadline, "E exits", e_run);
    assert!(e_out.status.success());
    let e_lines: Vec<Value> = String::from_utf8(e_out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let e_session = e_lines[0]["session_id"].as_str().unwrap().to_owned();
    *e_id.lock().unwrap() = Some(e_session.clone());
    // B starts and stays mid-turn on its stalled response.
    let mut b_command = setup.fiber(&["ask", b_prompt]);
    let (mut b, b_watchdog) = support::spawn_watched(&mut b_command);
    let b_group = support::KillGroup(b.id());
    let b_stdout = b.stdout.take().unwrap();
    let (b_tx, b_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(b_stdout).lines() {
            if b_tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let b_first: Value =
        serde_json::from_str(&deadline.recv_or_fail(&b_rx, "B session started")).unwrap();
    let b_id = b_first["session_id"].as_str().unwrap().to_owned();
    *b_cell.lock().unwrap() = Some(b_id.clone());
    assert!(
        server.await_requests(3, deadline),
        "T, E and B each called the model"
    );
    // The sender calls session_message once per step: T, B, E, then random.
    let s_run = setup.fiber(&["ask", s_prompt]);
    let s_out = support::run_to_exit(deadline, "S sends four messages", s_run);
    assert!(
        s_out.status.success(),
        "{}",
        String::from_utf8_lossy(&s_out.stderr)
    );
    let s_lines: Vec<Value> = String::from_utf8(s_out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let s_id = s_lines[0]["session_id"].as_str().unwrap();
    for target in [&t_id, &b_id, &e_session, &random_id] {
        let (started, resolved, _) = call_lines(&s_lines, target);
        assert_eq!(started["payload"]["effects"], json!(["writes"]), "{target}");
        assert_eq!(started["payload"]["reversible"], false, "{target}");
        assert!(started["payload"].get("paths").is_none(), "{started}");
        assert_eq!(resolved["payload"]["decided_by"], "reviewer", "{target}");
    }
    let (_, _, t_completed) = call_lines(&s_lines, &t_id);
    assert_eq!(completed_text(&t_completed), "delivered");
    let (_, _, b_completed) = call_lines(&s_lines, &b_id);
    assert_eq!(failed_code(&b_completed).0, "closing");
    for target in [&e_session, &random_id] {
        let (_, _, completed) = call_lines(&s_lines, target);
        let (code, message) = failed_code(&completed);
        assert_eq!(code, "unreachable", "{target}");
        assert_eq!(message, format!("No running session has the id {target}."));
    }
    // The message arrives on T as a turn whose input names S.
    let t_log = read_log(&setup, &t_id);
    assert!(
        t_log.iter().any(|line| {
            line["kind"] == "turn_started"
                && line["payload"]["input"].as_array().is_some_and(|input| {
                    input.iter().any(|item| {
                        item["type"] == "message"
                            && item["source"] == "session"
                            && item["from_session_id"] == s_id
                    })
                })
        }),
        "T takes the message as a turn"
    );
    // Between the subscribe and the message's turn no `clients` line
    // arrives, and the status still says one client.
    let mut fed = std::mem::take(&mut t_lines);
    loop {
        let line: Value = deadline.recv_or_fail(&t_rx, "T takes the message turn");
        let done = line["kind"] == "turn_started"
            && line["payload"]["input"]
                .as_array()
                .is_some_and(|input| input.iter().any(|item| item["type"] == "message"));
        fed.push(line);
        if done {
            break;
        }
    }
    // The one `clients` line is the held subscription's own attach; the
    // send, on a `summary` connection, adds none, so the count stays 1.
    let clients: Vec<_> = fed
        .iter()
        .filter(|line| line["kind"] == "clients")
        .collect();
    assert_eq!(clients.len(), 1, "one clients line: {clients:?}");
    assert_eq!(clients[0]["payload"]["count"], 1, "{clients:?}");
    let status = fed
        .iter()
        .rev()
        .find(|line| line["kind"] == "session_status")
        .expect("the fold carries a status");
    assert_eq!(status["payload"]["clients"], 1, "{status}");
    // Nothing is left behind: B's group, the hub with T, and the guard.
    fakes::kill_group(b_group.0, "TERM").unwrap();
    b.wait().unwrap();
    b_watchdog.stand_down(deadline.cleanup_phase());
    let t_closer = Socket::connect(deadline, &setup.session_socket(&t_id));
    support::close_session(&t_closer);
    let hub = hub.lock().unwrap().take().expect("the hub started");
    hub.kill("TERM");
    hub.wait();
}

#[test]
fn a_hub_started_session_messages_itself_and_takes_it_as_steering_at_its_next_step() {
    let setup = Setup::new();
    let deadline = setup.deadline;
    let _guard = support::SessionGuard::arm(deadline, &setup.root.path().to_string_lossy());
    let prompt = "message yourself with the word pineapple";
    // The sent text shares no word with the prompt, so only the framed
    // session-message item can carry it into the next model request.
    const SENT: &str = "the lighthouse keeps its lamp lit";
    let own = Arc::new(Mutex::new(None::<String>));
    let responder_own = Arc::clone(&own);
    let server = ProviderServer::start_responding(move |request: &Request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        if body["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty())
        {
            return allow();
        }
        let input = body["input"].as_array().unwrap();
        let text = serde_json::to_string(input).unwrap();
        assert!(text.contains(prompt), "T's request: {text}");
        if text.contains("function_call_output") {
            return hello();
        }
        let id = responder_own
            .lock()
            .unwrap()
            .clone()
            .expect("start answered");
        support::stream(&[message_call("call_self", &id, SENT)])
    })
    .unwrap();
    install(&setup, &server);
    // The server holds every response until `start` answers with T's id.
    server.hold();
    let hub = Arc::new(Mutex::new(None));
    let (main, _) = support::connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let t_id = support::start_session(&main, &workspace, prompt);
    *own.lock().unwrap() = Some(t_id.clone());
    server.release();
    // T's first turn ends after the self-message round trip.
    let t_socket = Socket::connect(deadline, &setup.session_socket(&t_id));
    let (t_tx, t_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut lines = Vec::new();
        t_socket.send(
            &json!({"id":"c_full","command":"subscribe","args":{"level":"full"}}).to_string(),
        );
        while let Some(line) = t_socket.next("T's next line", &lines) {
            t_tx.send(line.clone()).unwrap_or(());
            lines.push(line);
        }
    });
    let mut fed = Vec::new();
    loop {
        let line: Value = deadline.recv_or_fail(&t_rx, "T's first turn ends");
        let done = line["kind"] == "turn_completed";
        fed.push(line);
        if done {
            break;
        }
    }
    // The self-message arrives as steering in the same turn.
    let t_log = read_log(&setup, &t_id);
    let requested = t_log
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "session_message"
        })
        .expect("T calls session_message");
    let action = requested["action_id"].as_str().unwrap();
    let completed = t_log
        .iter()
        .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == action)
        .expect("the self-message completes");
    assert_eq!(completed["payload"]["status"], "completed", "{completed}");
    assert_eq!(
        completed["payload"]["content"][0]["text"], "delivered",
        "{completed}"
    );
    assert!(
        t_log.iter().any(|line| {
            line["kind"] == "steering_applied"
                && line["payload"]["source"] == "session"
                && line["payload"]["from_session_id"] == t_id
        }),
        "the self-message steers the turn"
    );
    // The next model request of that turn carries the sent text framed.
    let bodies: Vec<Vec<u8>> = server
        .requests()
        .into_iter()
        .filter(|request| is_session_request(&request.body))
        .map(|request| request.body)
        .collect();
    assert!(bodies.len() >= 2, "the turn calls the model again");
    let next: Value = serde_json::from_slice(bodies.last().unwrap()).unwrap();
    let input = next["input"].as_array().unwrap();
    assert!(
        input.iter().any(|item| {
            let flat = serde_json::to_string(item).unwrap();
            // The provider flattens the framed text into a user item, so
            // the marker and the session id pin the item as the framed
            // session message, and only steering put the text there.
            flat.contains("Fiber: a message from session")
                && flat.contains(&t_id)
                && flat.contains(SENT)
        }),
        "the framed session-message item carries the sent text"
    );
    let t_closer = Socket::connect(deadline, &setup.session_socket(&t_id));
    support::close_session(&t_closer);
    let hub = hub.lock().unwrap().take().expect("the hub started");
    hub.kill("TERM");
    hub.wait();
}

/// The fixture gatekeeper extension: driver messages pass, a session
/// message whose parent is not `target` is refused, and a child's is
/// rewritten.
const GATEKEEPER: &str = "fiber.test/gatekeeper";
const REFUSAL: &str = "only the target's children may write";
const REWRITE: &str = "rewritten by the target's hook";

fn gatekeeper_lua(target: &str) -> String {
    format!(
        "fiber.hook(\"before_message\", {{ timeout = 30000, on_failure = \"blocking\",\n\
           run = function(msg)\n\
             if msg.from_session_id == nil then return nil end\n\
             if msg.parent_session_id ~= \"{target}\" then return {{ refuse = \"{REFUSAL}\" }} end\n\
             return {{ content = {{{{ type = \"text\", text = \"{REWRITE}\" }}}} }}\n\
           end }})\n"
    )
}

/// A `fiber ask` sender's complete ordered kinds through one
/// session-message call: the reviewer allows the call, the call
/// completes, and the turn ends after the model sees it. The second
/// `usage_recorded` is the reviewer's own request.
const ASK_SEND_KINDS: [&str; 24] = [
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
    "permission_resolved",
    "tool_call_started",
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
];

/// No attach writes no `clients` line.
fn assert_no_clients(lines: &[Value]) {
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["kind"] == "clients")
            .count(),
        0,
        "no attach writes no clients line: {lines:?}"
    );
}

/// The event kinds of `lines` in order, setting aside `clients` and
/// `session_status`, which the contract writes on their own trigger
/// (`docs/testing.md`, "Event streams").
fn ordered_kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| !matches!(line["kind"].as_str(), Some("clients" | "session_status")))
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// A message turn's input item from `from` in `log`, if the session took one.
fn message_input<'a>(log: &'a [Value], from: &str) -> Option<&'a Value> {
    log.iter()
        .filter(|line| line["kind"] == "turn_started")
        .filter_map(|line| line["payload"]["input"].as_array())
        .flatten()
        .find(|item| {
            item["type"] == "message"
                && item["source"] == "session"
                && item["from_session_id"] == from
        })
}

#[test]
fn a_target_hook_refuses_sibling_senders_and_rewrites_its_children() {
    let setup = Setup::new();
    let deadline = setup.deadline;
    let _guard = support::SessionGuard::arm(deadline, &setup.root.path().to_string_lossy());
    let t_id = doors::mint("s_");
    let t_prompt = "sit idle and wait for a gated message";
    let s_prompt = "send one session message to the gated target";
    let c_prompt = "send one session message to the parent target";
    let responder_t = t_id.clone();
    let server = ProviderServer::start_responding(move |request: &Request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        // Reviewer requests carry no tools: every send is reviewed and allowed.
        if body["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty())
        {
            return allow();
        }
        let input = body["input"].as_array().unwrap();
        let text = serde_json::to_string(input).unwrap();
        if text.contains(t_prompt) {
            return hello();
        }
        if text.contains("function_call_output") {
            return hello();
        }
        if text.contains(s_prompt) {
            return support::stream(&[message_call("call_s", &responder_t, "sibling says hi")]);
        }
        assert!(text.contains(c_prompt), "the child's request: {text}");
        support::stream(&[message_call("call_c", &responder_t, "child says hi")])
    })
    .unwrap();
    install(&setup, &server);
    setup.lua("gatekeeper", &gatekeeper_lua(&t_id));
    // The target serves its session under a fixed id, so the fixture names it.
    let mut target = setup.start_session(&t_id, &["--prompt", t_prompt]);
    target.wait_for("turn_completed");
    // A top-level sibling has no parent: the hook refuses it, and no turn
    // takes anything from the sibling.
    let s_run = setup.fiber(&["ask", s_prompt]);
    let s_out = support::run_to_exit(deadline, "the sibling sends", s_run);
    assert!(s_out.status.success());
    let s_lines: Vec<Value> = String::from_utf8(s_out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let s_id = s_lines[0]["session_id"].as_str().unwrap();
    let (_, _, s_completed) = call_lines(&s_lines, &t_id);
    let (code, message) = failed_code(&s_completed);
    assert_eq!(code, "message_refused");
    assert_eq!(
        message,
        format!("Extension {GATEKEEPER} refused the message: {REFUSAL}")
    );
    // The sibling's event stream, in full: the reviewer allows the call,
    // the refusal fails it, and the turn ends after the model sees it.
    assert_eq!(
        ordered_kinds(&s_lines),
        ASK_SEND_KINDS,
        "the sibling's complete ordered kinds: {s_lines:?}"
    );
    assert_no_clients(&s_lines);
    let t_log = read_log(&setup, &t_id);
    assert!(
        message_input(&t_log, s_id).is_none(),
        "no turn takes the refused sibling's message"
    );
    // A child names the target as its parent: the hook rewrites it, the
    // sender is answered delivered, and the target logs the framed
    // rewrite with changed_by. Only `fiber session` takes a parent, so
    // the child is a parented fresh session under the target.
    let c_id = doors::mint("s_");
    let job = doors::mint("j_");
    let (mut child, _lifeline) = setup.start_session_stdin(
        &c_id,
        &[
            "--prompt",
            c_prompt,
            "--parent",
            t_id.as_str(),
            "--delegate-id",
            job.as_str(),
            "--depth",
            "1",
        ],
    );
    child.wait_for("fiber_exited");
    let (status, c_lines, stderr) = child.wait();
    assert!(status.success(), "{stderr}");
    let (_, _, c_completed) = call_lines(&c_lines, &t_id);
    assert_eq!(completed_text(&c_completed), "delivered");
    // The child's event stream, in full: the reviewer allows the call,
    // the rewrite is delivered, and the turn ends after the model sees it.
    assert_eq!(
        ordered_kinds(&c_lines),
        ASK_SEND_KINDS,
        "the child's complete ordered kinds: {c_lines:?}"
    );
    assert_no_clients(&c_lines);
    let t_log = read_log(&setup, &t_id);
    let input = message_input(&t_log, &c_id).expect("the target takes the child's message");
    let content = input["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    let framed = content[0]["text"].as_str().unwrap();
    assert!(
        framed.contains(REWRITE),
        "the target logs the framed rewrite: {framed}"
    );
    assert!(
        !framed.contains("child says hi"),
        "the rewrite replaced the raw text: {framed}"
    );
    assert_eq!(input["changed_by"], json!([GATEKEEPER]));
    // The target's log, in full: its own first turn, then the child's
    // message starting the second. The refused sibling leaves nothing.
    // The child's delivery starts the target's second turn without waiting
    // for it, so the log holds the whole turn only once its completion
    // reaches the target's stdout; wait for that line before reading.
    target.wait_for("turn_completed");
    let t_log = read_log(&setup, &t_id);
    // The complete ordered kinds, so a duplicated, missing or reordered
    // event fails (`docs/testing.md`, "Event streams"). Positions follow
    // `docs/events.md`, "Process boundary" (`session_started` is the
    // log's first line, `fiber_started` follows it), "Extensions"
    // (`extensions_loaded` before the first model request), "Preamble"
    // (`preamble_built` follows `fiber_started` before the next model
    // request), "Opening message" (written at session start), "Session
    // and turn" (`step_started` opens the step before anything else in
    // it; `turn_completed` means settled), "Actions" (a text reply's
    // `text_completed` follows its `assistant_message_started` in the
    // model's order, `assistant_message_completed` closes it), "Usage
    // and notices" (one `usage_recorded` per model call, so one per
    // quiet turn here), and `docs/loop.md`, "Starting a turn", "One
    // step" and "Ending a turn" (one step per turn: the model replies
    // with no tool call; the refused sibling's message is neither
    // logged nor sent per `docs/extensions.md`, "The hook points", so
    // no turn takes it, while the child's message starts the second
    // turn, giving exactly two turns).
    assert_eq!(
        ordered_kinds(&t_log),
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
        ],
        "the target's complete ordered kinds"
    );
    // Close the target: its socket, then its process.
    let t_closer = Socket::connect(deadline, &setup.session_socket(&t_id));
    support::close_session(&t_closer);
    let (status, _, stderr) = target.wait();
    assert!(status.success(), "{stderr}");
}

#[test]
fn delivered_is_answered_only_after_the_hook_returns() {
    let setup = Setup::new();
    let deadline = setup.deadline;
    let _guard = support::SessionGuard::arm(deadline, &setup.root.path().to_string_lossy());
    let t_id = doors::mint("s_");
    let t_prompt = "sit idle and wait for a held message";
    let s_prompt = "send one session message to the held target";
    // The hole: the hook's HTTP parks here, recorded before it is
    // answered, until the test releases it below.
    let hole = ProviderServer::start_responding(|_: &Request| Response::status(200, "{}")).unwrap();
    hole.hold();
    let hole_url = hole.url();
    let responder_t = t_id.clone();
    let server = ProviderServer::start_responding(move |request: &Request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        if body["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty())
        {
            return allow();
        }
        let input = body["input"].as_array().unwrap();
        let text = serde_json::to_string(input).unwrap();
        if text.contains(t_prompt) {
            return hello();
        }
        if text.contains("function_call_output") {
            return hello();
        }
        assert!(text.contains(s_prompt), "the sender's request: {text}");
        support::stream(&[message_call("call_s", &responder_t, "held says hi")])
    })
    .unwrap();
    install(&setup, &server);
    setup.lua(
        "holder",
        &format!(
            "fiber.hook(\"before_message\", {{ timeout = 30000, on_failure = \"blocking\",\n\
               run = function(msg)\n\
                 if msg.from_session_id == nil then return nil end\n\
                 host.http({{ url = \"{hole_url}/hold\" }})\n\
                 return nil\n\
               end }})\n"
        ),
    );
    let mut target = setup.start_session(&t_id, &["--prompt", t_prompt]);
    target.wait_for("turn_completed");
    // The sender runs live, so its lines are watched while it waits.
    let mut s_command = setup.fiber(&["ask", s_prompt]);
    let (mut s, s_watchdog) = support::spawn_watched(&mut s_command);
    let s_group = support::KillGroup(s.id());
    let s_stdout = s.stdout.take().unwrap();
    let (s_tx, s_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(s_stdout).lines() {
            if s_tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    // The hook parked on the hole: the entered request names that signal.
    assert!(
        hole.await_requests(1, deadline),
        "the hook parked on its blocking read"
    );
    // While it is parked the sender's answer stays pending: no line up
    // to the call completes it. The drain ends on the call itself, under
    // the test's deadline. The first line names the sender's session.
    let mut action = None;
    let mut sender = None;
    while action.is_none() {
        let text: String = deadline.recv_or_fail(&s_rx, "the sender's call");
        let line: Value = serde_json::from_str(&text).unwrap();
        if sender.is_none() {
            sender = line
                .get("session_id")
                .and_then(|id| id.as_str())
                .map(str::to_owned);
        }
        if line["kind"] == "tool_call_requested" && line["payload"]["name"] == "session_message" {
            action = Some(line["action_id"].as_str().unwrap().to_owned());
        }
        assert!(
            line["kind"] != "tool_call_completed",
            "the sender waits for the hook: {line}"
        );
    }
    let sender = sender.expect("the sender names its session");
    // Still parked, so the take cannot have answered: a steer through the
    // sender's own socket proves it. The steer is answered at once and its
    // `steering_queue` line copies to the sender's stdout behind any line
    // already written there, so reaching the marker with no completion past
    // means no premature completion was in flight; the hook still holds the
    // ack past it. A round trip orders the check instead of a wall-clock
    // wait.
    let probe = Socket::connect(deadline, &setup.session_socket(&sender));
    probe.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    // The subscription replays the session from its start, so the ack is
    // read past the replay.
    let sub = loop {
        let line = support::recv(&probe, "the probe subscribes");
        if line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_sub" {
            break line;
        }
    };
    assert_eq!(sub["kind"], "command_accepted", "{sub}");
    probe.send(
        r#"{"id":"c_probe","command":"steer","args":{"content":[{"type":"text","text":"probe"}]}}"#,
    );
    // The ack may arrive ahead of the subscription's replay.
    let ack = loop {
        let line = support::recv(&probe, "the probe steer is accepted");
        if line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_probe" {
            break line;
        }
    };
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_probe", "{ack}");
    let action = action.expect("the sender called session_message");
    loop {
        let text: String = deadline.recv_or_fail(&s_rx, "the probe steer is queued");
        let line: Value = serde_json::from_str(&text).unwrap();
        if line["kind"] == "steering_queue"
            && line["payload"]["messages"]
                .as_array()
                .is_some_and(|messages| {
                    messages
                        .iter()
                        .any(|message| message["command_id"] == "c_probe")
                })
        {
            break;
        }
        assert!(
            line["kind"] != "tool_call_completed",
            "the sender waits for the hook: {line}"
        );
    }
    // While the hook stays held on the hole's `/hold` signal, no
    // completion for this call may arrive: drain a bounded 100 ms window
    // from one start instant (`docs/testing.md`, "Waits and timeouts")
    // and fail only on the sender's own `tool_call_completed`. Other
    // lines are benign traffic that macOS may order into the window.
    let held = fakes::Deadline::after(std::time::Duration::from_millis(100));
    loop {
        match held.recv(&s_rx) {
            Err(_) => break,
            Ok(text) => {
                let text: String = text;
                let line: Value = serde_json::from_str(&text).unwrap();
                assert!(
                    !(line["kind"] == "tool_call_completed" && line["action_id"] == action),
                    "no tool_call_completed while the hook stays held on the hole's /hold signal: {line}"
                );
            }
        }
    }
    hole.release();
    // Released, the hook answers and the sender is delivered.
    let completed = loop {
        let text: String = deadline.recv_or_fail(&s_rx, "the sender's completion");
        let line: Value = serde_json::from_str(&text).unwrap();
        if line["kind"] == "tool_call_completed" && line["action_id"] == action {
            break line;
        }
    };
    assert_eq!(completed_text(&completed), "delivered");
    // The sender exits on its own once delivered; then the target closes.
    loop {
        let text: String = deadline.recv_or_fail(&s_rx, "the sender to exit");
        let line: Value = serde_json::from_str(&text).unwrap();
        if line["kind"] == "fiber_exited" {
            break;
        }
    }
    // The sender already exited on its own, so the group may be gone:
    // the kill's result is ignored, as `expired` ignores its cleanup kill.
    match fakes::kill_group(s_group.0, "TERM") {
        Ok(_) | Err(_) => {}
    }
    s.wait().unwrap();
    s_watchdog.stand_down(deadline.cleanup_phase());
    let t_closer = Socket::connect(deadline, &setup.session_socket(&t_id));
    support::close_session(&t_closer);
    let (status, _, stderr) = target.wait();
    assert!(status.success(), "{stderr}");
}
