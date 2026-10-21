//! `session_message` between real sessions: a `fiber ask` sender reaches a
//! hub-started session, and a hub-started session messages itself.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures and assertions"
)]
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
    support::kill_group(deadline, b_group.0, "TERM").unwrap();
    b.wait().unwrap();
    b_watchdog.stand_down(deadline.cleanup());
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
