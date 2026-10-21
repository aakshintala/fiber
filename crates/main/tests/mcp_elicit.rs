//! Binary-level tests of MCP elicitation through a live session
//! (`docs/mcp.md`, "Elicitation, sampling and roots"): the built `fiber`
//! runs in its own process group with its own `FIBER_HOME`, holding an
//! ordinary provider and one configured fixture server. Every wait for a
//! socket line uses the test's one deadline.

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
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;

use extension_harness::Running;
use fakes::clock::FakeClock;
use fakes::{Client, ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, Setup, function_call, hello, stream};

/// Kills every process whose command line names `root`, and their groups,
/// while held: every fixture server runs with its directory under the
/// root as its argument.
#[allow(dead_code, reason = "held only for its drop")]
struct Guard(Watchdog);

fn guard(setup: &Setup) -> Guard {
    Guard(Watchdog::matching(&setup.root.path().display().to_string()))
}

fn subscribe(deadline: Deadline, client: &Client) {
    client
        .send_by(
            r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
            &|| deadline.left(),
        )
        .unwrap();
}

fn prompt(deadline: Deadline, client: &Client) {
    client
        .send_by(
            r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"go"}]}}"#,
            &|| deadline.left(),
        )
        .unwrap();
}

/// Waits for the subscribe's `command_accepted`, so the prompt that
/// follows cannot overtake it.
fn subscribed(deadline: Deadline, client: &Client) {
    client
        .recv_until(deadline, |line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_sub"
        })
        .expect("the subscribe is accepted");
}

fn requested(deadline: Deadline, client: &Client) -> Value {
    client
        .recv_until(deadline, |line| line["kind"] == "interaction_requested")
        .expect("the question arrives")
}

fn reply(deadline: Deadline, client: &Client, id: &str, args: &Value) {
    let command = json!({"id": id, "command": "reply", "args": args}).to_string();
    client.send_by(&command, &|| deadline.left()).unwrap();
}

fn resolved(deadline: Deadline, client: &Client) -> Value {
    client
        .recv_until(deadline, |line| line["kind"] == "interaction_resolved")
        .expect("the resolution arrives")
}

fn rejected(deadline: Deadline, client: &Client, id: &str) -> Value {
    client
        .recv_until(deadline, |line| {
            line["kind"] == "command_rejected" && line["payload"]["command_id"] == id
        })
        .expect("the reply is rejected")
}

/// Writes the fixture server's directory: `tools.json` and one file per
/// `(name, body)` pair. Returns the directory.
fn fixture(setup: &Setup, tools: &Value, files: &[(&str, &str)]) -> PathBuf {
    let dir = setup.root.path().join("fx");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tools.json"), tools.to_string()).unwrap();
    for (name, body) in files {
        fs::write(dir.join(name), body).unwrap();
    }
    dir
}

/// Configures one fixture server `fx` in `dir`, with `model` and
/// `mcp.servers` in one file, the shape `tests/mcp.rs` writes.
fn configure_fx(setup: &Setup, dir: &Path, extra: Value) {
    let mut server = json!({
        "command": "/bin/bash",
        "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
        "startup_timeout_ms": 600000,
    });
    if let (Some(object), Value::Object(fields)) = (server.as_object_mut(), extra) {
        for (key, value) in fields {
            object.insert(key, value);
        }
    }
    fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "mcp": {"servers": {"fx": server}}}).to_string(),
    )
    .unwrap();
}

/// One read-only offline tool, so its calls take the permission fast path.
fn ask_tool() -> Value {
    json!({
        "name": "ask",
        "description": "Asks.",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

fn hold_tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": "Holds.",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

/// The three-field elicitation: `name` (a required string) then
/// `approve` (a boolean) then `count` (an integer of at least 1). `name`
/// before `approve` is the reverse of sorted order, so the order proves
/// the server's order end to end.
fn three_fields() -> &'static str {
    r#"{"message":"Deploy settings","requestedSchema":{"type":"object","properties":{"name":{"type":"string","title":"Name"},"approve":{"type":"boolean"},"count":{"type":"integer","minimum":1}},"required":["name","approve","count"]}}"#
}

fn mkfifo(dir: &Path, name: &str) {
    fakes::fifo(dir.join(name));
}

/// Reads one FIFO's line, with the test's deadline naming the wait. After
/// `Timeout` the reader thread stays blocked in `open` on the fifo; the
/// fixture's writer, or process exit, ends it. Nothing joins it, so
/// nothing hangs.
fn read_fifo(deadline: Deadline, path: PathBuf) -> Result<String, RecvTimeoutError> {
    let (done, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let content = fs::read_to_string(&path).expect("the fifo reads");
        done.send(content).expect("collected");
    });
    deadline.recv(&read)
}

#[test]
fn read_fifo_past_a_spent_deadline_times_out() {
    let setup = Setup::new();
    let dir = setup.root.path().join("fifo");
    fs::create_dir_all(&dir).unwrap();
    mkfifo(&dir, "never-written");
    let clock = Box::leak(Box::new(FakeClock::new()));
    let spent = Deadline::on(&**clock);
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    clock.advance(support::WAITS);
    assert_eq!(
        read_fifo(spent, dir.join("never-written")),
        Err(RecvTimeoutError::Timeout),
    );
}

/// Names whether the fixture server received `tools/call` for `tool`,
/// then prints the whole `requests.log` and the whole session stderr, so
/// a `Timeout` sighting says which side of the server the stall is on. A
/// line counts only when it names both the method and the tool, with the
/// tool's closing quote, so a similarly named tool never matches.
fn hold_report(tool: &str, requests_log: &str, stderr: &str) -> String {
    let reached = requests_log.lines().any(|line| {
        line.contains("\"method\":\"tools/call\"") && line.contains(&format!("\"name\":\"{tool}\""))
    });
    let verdict = if reached {
        format!("`tools/call` for {tool} reached the server, so the stall is after it")
    } else {
        format!("no `tools/call` for {tool} in requests.log: the server never received it")
    };
    format!("{verdict}\n\nrequests.log:\n{requests_log}\n\nsession stderr:\n{stderr}")
}

#[test]
fn hold_report_names_a_call_that_reached_the_server() {
    let log = concat!(
        "{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{}}\n",
        "{\"id\":2,\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"arguments\":{},\"name\":\"hold\"}}\n",
        "{\"id\":3,\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"arguments\":{},\"name\":\"ask\"}}\n",
    );
    let report = hold_report("hold", log, "some stderr");
    assert!(report.contains("reached the server"), "{report}");
    assert!(report.contains(log), "{report}");
    assert!(report.contains("some stderr"), "{report}");
}

#[test]
fn hold_report_names_a_call_that_never_arrived() {
    let log = concat!(
        "{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{}}\n",
        "{\"id\":2,\"jsonrpc\":\"2.0\",\"method\":\"tools/list\",\"params\":{}}\n",
        "{\"id\":3,\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"arguments\":{},\"name\":\"ask\"}}\n",
        "{\"id\":4,\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"arguments\":{},\"name\":\"holder\"}}\n",
    );
    let report = hold_report("hold", log, "some stderr");
    assert!(report.contains("no `tools/call` for hold"), "{report}");
    assert!(report.contains("never received"), "{report}");
    assert!(report.contains(log), "{report}");
    assert!(report.contains("some stderr"), "{report}");
}

/// Reads the fixture's `held-<tool>` fifo, and on expiry kills the
/// session, then fails with the log and stderr saying whether `tools/call`
/// reached the server. `wait` bounds only the fifo read; `cleanup` bounds
/// the kill and the stderr receive, so a spent `wait` still leaves room
/// for the report. A panicked reader thread leads with that fact, not
/// with what the server never received.
fn read_held(
    wait: Deadline,
    cleanup: Deadline,
    running: &Running,
    dir: &Path,
    tool: &str,
) -> Result<String, String> {
    match read_fifo(wait, dir.join(format!("held-{tool}"))) {
        Ok(line) => Ok(line),
        Err(error) => {
            let requests_log = fs::read_to_string(dir.join("requests.log"))
                .unwrap_or_else(|error| format!("requests.log is unreadable: {error}"));
            match support::kill_group(cleanup, running.group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let stderr = match cleanup.cleanup_phase().recv(&running.stderr) {
                Ok(stderr) => stderr,
                Err(error) => format!("the session's stderr did not arrive: {error}"),
            };
            let report = hold_report(tool, &requests_log, &stderr);
            Err(match error {
                RecvTimeoutError::Timeout => report,
                RecvTimeoutError::Disconnected => {
                    format!("the fifo's reader thread panicked before it was read\n{report}")
                }
            })
        }
    }
}

/// Writes one line to a FIFO, with the test's deadline naming the wait.
fn write_fifo(deadline: Deadline, path: PathBuf) {
    let (done, wrote) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        fs::write(&path, "go\n").expect("the fifo writes");
        done.send(()).expect("collected");
    });
    deadline.recv(&wrote).expect("the fifo writes in time");
}

#[test]
fn an_elicitation_raises_one_interaction_per_field_and_the_server_gets_the_answers() {
    let setup = Setup::new();
    let _guard = guard(&setup);
    let dir = fixture(
        &setup,
        &json!([ask_tool()]),
        &[
            (
                "call-ask.json",
                r#"{"content":[{"type":"text","text":"finished"}]}"#,
            ),
            ("elicit-ask.json", three_fields()),
        ],
    );
    let server = ProviderServer::start([
        stream(&[function_call("c1", "mcp__fx__ask", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect_client(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(setup.deadline, &client);
    client
        .recv_until(
            fakes::Deadline::after(std::time::Duration::from_secs(10)),
            |line| line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_sub",
        )
        .expect("the subscribe is accepted");
    prompt(setup.deadline, &client);
    // The call's id rides on the envelope, not the payload.
    let call = client
        .recv_until(setup.deadline, |line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "mcp__fx__ask"
        })
        .expect("the call is requested");
    let action = call["action_id"].as_str().unwrap().to_owned();
    // One interaction per field, in the server's order, each naming the call.
    let first = requested(setup.deadline, &client);
    assert_eq!(first["payload"]["kind"], "text_input");
    assert!(
        first["payload"]["prompt"]
            .as_str()
            .unwrap()
            .ends_with("Name"),
        "the first prompt: {}",
        first["payload"]["prompt"],
    );
    assert_eq!(first["payload"]["action_ids"], json!([action]));
    let first_id = first["payload"]["request_id"].as_str().unwrap().to_owned();
    reply(
        setup.deadline,
        &client,
        "c_r1",
        &json!({"request_id": first_id, "text": "prod"}),
    );
    let named = resolved(setup.deadline, &client);
    assert_eq!(named["payload"]["request_id"], first_id);
    assert_eq!(named["payload"]["by"], "person");
    let second = requested(setup.deadline, &client);
    assert_eq!(second["payload"]["kind"], "confirm");
    assert_eq!(second["payload"]["action_ids"], json!([action]));
    let second_id = second["payload"]["request_id"].as_str().unwrap().to_owned();
    reply(
        setup.deadline,
        &client,
        "c_r2",
        &json!({"request_id": second_id, "confirmed": true}),
    );
    let confirmed = resolved(setup.deadline, &client);
    assert_eq!(confirmed["payload"]["request_id"], second_id);
    // The integer field keeps one request: unfit answers are rejected and
    // resolve nothing.
    let third = requested(setup.deadline, &client);
    assert_eq!(third["payload"]["kind"], "text_input");
    assert!(
        third["payload"]["prompt"]
            .as_str()
            .unwrap()
            .ends_with("count"),
        "the third prompt: {}",
        third["payload"]["prompt"],
    );
    let third_id = third["payload"]["request_id"].as_str().unwrap().to_owned();
    reply(
        setup.deadline,
        &client,
        "c_r3",
        &json!({"request_id": third_id, "text": "abc"}),
    );
    let unfit = rejected(setup.deadline, &client, "c_r3");
    assert_eq!(unfit["payload"]["code"], "invalid_arguments");
    reply(
        setup.deadline,
        &client,
        "c_r4",
        &json!({"request_id": third_id, "text": "0"}),
    );
    let unfit = rejected(setup.deadline, &client, "c_r4");
    assert_eq!(unfit["payload"]["code"], "invalid_arguments");
    reply(
        setup.deadline,
        &client,
        "c_r5",
        &json!({"request_id": third_id, "text": "3"}),
    );
    let counted = resolved(setup.deadline, &client);
    assert_eq!(counted["payload"]["request_id"], third_id);
    assert_eq!(counted["payload"]["text"], "3");
    let completed = client
        .recv_until(setup.deadline, |line| line["kind"] == "tool_call_completed")
        .expect("the call completes");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], "finished");
    client
        .send_by(r#"{"id":"c_close","command":"close"}"#, &|| {
            setup.deadline.left()
        })
        .unwrap();
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    // Without the position-free lines (`session_status`, and `clients`,
    // written when a connection attaches): exactly three
    // requested/resolved pairs, so the rejected replies wrote nothing
    // and the integer field stayed one request.
    let kinds: Vec<&str> = out
        .iter()
        .filter(|line| line["kind"] != "session_status" && line["kind"] != "clients")
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
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
            "interaction_requested",
            "interaction_resolved",
            "interaction_requested",
            "interaction_resolved",
            "interaction_requested",
            "interaction_resolved",
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
        ],
    );
    // The integer reaches the wire as a JSON integer.
    let log = fs::read_to_string(dir.join("requests.log")).unwrap();
    let accept = log
        .lines()
        .find(|line| line.contains("\"id\":\"el-") && line.contains("\"action\":\"accept\""))
        .unwrap_or_else(|| panic!("the accept reaches the wire: {log}"));
    assert!(
        accept.contains(r#""approve":true"#)
            && accept.contains(r#""count":3"#)
            && accept.contains(r#""name":"prod""#),
        "the server gets the answers: {accept}",
    );
}

#[test]
fn an_elicitation_with_two_calls_in_flight_names_both() {
    let setup = Setup::new();
    let _guard = guard(&setup);
    let dir = fixture(
        &setup,
        &json!([hold_tool("hold"), ask_tool()]),
        &[
            (
                "call-hold.json",
                r#"{"content":[{"type":"text","text":"held"}]}"#,
            ),
            (
                "call-ask.json",
                r#"{"content":[{"type":"text","text":"asked"}]}"#,
            ),
            ("elicit-ask.json", three_fields()),
        ],
    );
    for name in ["release-hold", "held-hold", "release-ask"] {
        mkfifo(&dir, name);
    }
    // One model reply calls both tools in one step, then a text reply.
    let server = ProviderServer::start([
        stream(&[
            function_call("c_hold", "mcp__fx__hold", &json!({})),
            function_call("c_ask", "mcp__fx__ask", &json!({})),
        ]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect_client(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(setup.deadline, &client);
    subscribed(setup.deadline, &client);
    prompt(setup.deadline, &client);
    // `hold` is held first: its acknowledgement proves it is registered,
    // so the elicitation lists it alongside `ask`.
    assert_eq!(
        read_held(setup.deadline, setup.deadline, &running, &dir, "hold")
            .unwrap_or_else(|message| panic!("{message}")),
        "held\n",
    );
    write_fifo(setup.deadline, dir.join("release-ask"));
    let mut actions = Vec::new();
    for tool in ["mcp__fx__hold", "mcp__fx__ask"] {
        let call = client
            .recv_until(setup.deadline, |line| {
                line["kind"] == "tool_call_requested" && line["payload"]["name"] == tool
            })
            .expect("both calls are requested");
        actions.push(call["action_id"].as_str().unwrap().to_owned());
    }
    actions.sort();
    // All three fields name both calls, and all three resolve.
    for (index, (command, args)) in [
        ("c_r1", json!({"text": "prod"})),
        ("c_r2", json!({"confirmed": true})),
        ("c_r3", json!({"text": "3"})),
    ]
    .into_iter()
    .enumerate()
    {
        let asked = requested(setup.deadline, &client);
        let mut listed: Vec<String> = asked["payload"]["action_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect();
        listed.sort();
        assert_eq!(listed, actions, "ask {index} names both calls");
        let request_id = asked["payload"]["request_id"].as_str().unwrap().to_owned();
        let mut full = json!({"request_id": request_id});
        for (key, value) in args.as_object().unwrap() {
            full[key] = value.clone();
        }
        reply(setup.deadline, &client, command, &full);
        let resolved = resolved(setup.deadline, &client);
        assert_eq!(resolved["payload"]["request_id"], request_id);
    }
    write_fifo(setup.deadline, dir.join("release-hold"));
    for _ in 0..2 {
        let completed = client
            .recv_until(setup.deadline, |line| line["kind"] == "tool_call_completed")
            .expect("both calls complete");
        assert_eq!(completed["payload"]["status"], "completed");
    }
    client
        .send_by(r#"{"id":"c_close","command":"close"}"#, &|| {
            setup.deadline.left()
        })
        .unwrap();
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    // Without the position-free lines: both calls requested and started
    // in one step, three pairs naming both, both completed.
    let kinds: Vec<&str> = out
        .iter()
        .filter(|line| line["kind"] != "session_status" && line["kind"] != "clients")
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
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
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_started",
            "interaction_requested",
            "interaction_resolved",
            "interaction_requested",
            "interaction_resolved",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
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
        ],
    );
}

#[test]
fn a_hold_that_never_reaches_the_server_is_named() {
    let setup = Setup::new();
    let _guard = guard(&setup);
    let dir = fixture(
        &setup,
        &json!([hold_tool("hold"), ask_tool()]),
        &[
            (
                "call-hold.json",
                r#"{"content":[{"type":"text","text":"held"}]}"#,
            ),
            (
                "call-ask.json",
                r#"{"content":[{"type":"text","text":"asked"}]}"#,
            ),
            ("elicit-ask.json", three_fields()),
        ],
    );
    for name in ["release-hold", "held-hold", "release-ask"] {
        mkfifo(&dir, name);
    }
    // Only `ask` is ever called, so `held-hold` is never written and
    // `tools/call hold` never reaches the server, whatever the order of
    // events.
    let server = ProviderServer::start([
        stream(&[function_call("c_ask", "mcp__fx__ask", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect_client(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(setup.deadline, &client);
    subscribed(setup.deadline, &client);
    prompt(setup.deadline, &client);
    let clock = Box::leak(Box::new(FakeClock::new()));
    let spent = Deadline::on(&**clock);
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    clock.advance(support::WAITS);
    let message = read_held(spent, setup.deadline, &running, &dir, "hold").unwrap_err();
    assert!(message.contains("no `tools/call` for hold"), "{message}",);
    assert!(message.contains("never received"), "{message}");
}
