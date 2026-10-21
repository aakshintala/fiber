//! Binary-level tests of the `name_session` tool (`docs/tools.md`,
//! "Naming the session"; `docs/testing.md`, "Levels"): the built `fiber`
//! runs `fiber ask` in its own process group with its own `FIBER_HOME`,
//! with the fake provider.

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
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, Running, Setup, Socket, answered, expired, function_call, group_alive, hello,
    is_status, read_kinds, run_to_exit, stream, until,
};

/// A finished `function_call` for `name_session` with `arguments`.
fn name_call(call_id: &str, arguments: &Value) -> Value {
    function_call(call_id, "name_session", arguments)
}

/// Runs `fiber ask` with a model that answers `responses`, then returns
/// stdout's lines.
fn ask_with(setup: &Setup, prompt: &str) -> Vec<Value> {
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", prompt]));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect()
}

/// The session's directory: whichever project holds `id` (`fiber ask`
/// runs with the setup root as its working directory, so the session's
/// project is the root, not the workspace, and no workspace-keyed path
/// finds it).
fn session_dir(setup: &Setup, id: &str) -> std::path::PathBuf {
    let projects = setup.home().join("projects");
    let mut stack = vec![projects];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().unwrap() == id {
                    return path;
                }
                stack.push(path);
            }
        }
    }
    panic!("no session dir for {id}");
}

/// Reads `events.jsonl` in `dir` as one JSON value per line.
fn read_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The session id stdout's first line names.
fn session_id(lines: &[Value]) -> &str {
    lines[0]["session_id"].as_str().unwrap()
}

#[test]
fn naming_writes_session_named_before_the_completion() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[name_call("c1", &json!({"name": "Fix the parser"}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let lines = ask_with(&setup, "go");

    let mut expected = read_kinds();
    let at = expected
        .iter()
        .position(|k| *k == "tool_call_completed")
        .unwrap();
    expected.insert(at, "session_named");
    assert_eq!(support::kinds(&lines), expected);

    let log = read_lines(&session_dir(&setup, session_id(&lines)).join("events.jsonl"));
    let named = log.iter().find(|l| l["kind"] == "session_named").unwrap();
    let completed = log
        .iter()
        .find(|l| l["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(
        named["payload"],
        json!({"name": "Fix the parser", "by": "model"})
    );
    assert_eq!(named["action_id"], completed["action_id"]);
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"], json!([]));
    let started = log
        .iter()
        .find(|l| l["kind"] == "tool_call_started")
        .unwrap();
    assert_eq!(started["payload"]["effects"], json!([]));

    let body = &server.requests()[0].body;
    let tools: Value = serde_json::from_slice(body).unwrap();
    let tool = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "name_session")
        .unwrap();
    assert_eq!(tool["strict"], false);
    assert_eq!(tool["parameters"]["properties"]["name"]["minLength"], 1);
    assert_eq!(tool["parameters"]["properties"]["name"]["maxLength"], 60);
    assert!(
        tool["description"]
            .as_str()
            .unwrap()
            .contains("after the first prompt"),
        "{tool}"
    );
    assert!(
        tool["description"]
            .as_str()
            .unwrap()
            .contains("when the work's topic changes, a handoff included"),
        "{tool}"
    );
}

#[test]
fn length_limits_fail_invalid_arguments_and_write_no_name() {
    let setup = Setup::new();
    let long = "a".repeat(61);
    let sixty = "é".repeat(60);
    let server = ProviderServer::start([
        stream(&[
            name_call("c1", &json!({"name": ""})),
            name_call("c2", &json!({"name": long})),
            name_call("c3", &json!({"name": sixty})),
        ]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    // One step makes all three calls: the fake provider answers all three
    // in one reply.
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", "go"]));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect();
    assert_eq!(
        support::kinds(&lines),
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
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            // The two invalid calls fail before starting; only the valid
            // call writes `tool_call_started`.
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "session_named",
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
    let id = session_id(&lines).to_owned();
    let log = read_lines(&session_dir(&setup, &id).join("events.jsonl"));
    let completed: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), 3);
    assert_eq!(completed[0]["payload"]["status"], "failed");
    assert_eq!(
        completed[0]["payload"]["error"]["code"],
        "invalid_arguments"
    );
    assert_eq!(completed[1]["payload"]["status"], "failed");
    assert_eq!(
        completed[1]["payload"]["error"]["code"],
        "invalid_arguments"
    );
    let named: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .collect();
    assert_eq!(named.len(), 1, "{log:?}");
    assert_eq!(named[0]["payload"]["name"], json!(sixty));
    assert_eq!(named[0]["action_id"], completed[2]["action_id"]);
}

#[test]
fn the_latest_name_is_the_session_name_after_a_resume() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[name_call("c1", &json!({"name": "first"}))]),
        stream(&[name_call("c2", &json!({"name": "second"}))]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let lines = ask_with(&setup, "go");
    // Two naming steps, then the closing text step.
    let step = [
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_started",
        "session_named",
        "tool_call_completed",
    ];
    let mut expected = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    expected.extend(step);
    expected.extend(step);
    expected.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    assert_eq!(support::kinds(&lines), expected);
    let id = session_id(&lines).to_owned();
    let log = read_lines(&session_dir(&setup, &id).join("events.jsonl"));
    let named: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .collect();
    assert_eq!(named.len(), 2);
    assert_eq!(named[0]["payload"]["name"], "first");
    assert_eq!(named[1]["payload"]["name"], "second");

    let output = run_to_exit(
        setup.deadline,
        "fiber ask --resume",
        setup.fiber(&["ask", "--resume", &id, "hi"]),
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let resumed: Vec<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let resumed_lines: Vec<Value> = resumed
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect();
    assert_eq!(
        support::kinds(&resumed_lines),
        [
            // A resume writes no `session_started`, and the log already
            // holds an opening message, so none is written either.
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
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
    let status = resumed
        .iter()
        .filter(|line| is_status(line))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .next_back()
        .expect("a session_status line");
    assert_eq!(status["payload"]["name"], "second");
}

#[test]
fn a_person_name_pins_the_session_after_a_resume() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[name_call("c1", &json!({"name": "first"}))]),
        hello(),
        stream(&[name_call("c2", &json!({"name": "Theirs"}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    server.hold();
    let running = start(setup.fiber(&["ask", "go"]), setup.deadline);
    let first_raw: String = setup
        .deadline
        .recv(&running.stdout)
        .expect("waited until the deadline for fiber_started");
    let started: Value = serde_json::from_str(&first_raw).unwrap();
    let id = started["session_id"].as_str().unwrap().to_owned();
    assert!(
        server.await_requests(1, setup.deadline),
        "the held response was requested"
    );
    let client = Socket::connect(setup.deadline, &setup.session_socket(&id));
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let subscribed = until(&client, "the subscribe acknowledgement", |line| {
        line["payload"]["command_id"] == "c_sub"
    });
    assert_eq!(
        answered(&subscribed, "c_sub")["kind"],
        "command_accepted",
        "{subscribed:?}"
    );
    // The `tools` probe's answer proves the `name` is queued before the
    // release, so the name is taken at step 2's drain, after `c1` completed.
    client.send(r#"{"id":"c_pin","command":"name","args":{"text":"Mine"}}"#);
    client.send(r#"{"id":"c_q1","command":"tools"}"#);
    let queued = until(&client, "the answer to c_q1", |line| {
        line["payload"]["command_id"] == "c_q1"
    });
    assert_eq!(
        answered(&queued, "c_q1")["kind"],
        "command_accepted",
        "{queued:?}"
    );
    server.release_one();
    let pinned = until(&client, "the answer to c_pin", |line| {
        line["payload"]["command_id"] == "c_pin"
    });
    assert_eq!(
        answered(&pinned, "c_pin")["kind"],
        "command_accepted",
        "{pinned:?}"
    );
    server.release();
    let mut raw = vec![first_raw];
    loop {
        let line: String = setup
            .deadline
            .recv(&running.stdout)
            .expect("waited until the deadline for fiber_exited");
        let parsed: Value = serde_json::from_str(&line).unwrap();
        let done = parsed["kind"] == "fiber_exited";
        raw.push(line);
        if done {
            break;
        }
    }
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    finish(running);
    // The first run names once by model (`c1`) and once by person (taken at
    // step 2's drain): `read_kinds` with a `session_named` before the first
    // `tool_call_completed` and another after the second `step_started`,
    // with `session_status` and `clients` set aside.
    let mut expected = read_kinds();
    let at = expected
        .iter()
        .position(|k| *k == "tool_call_completed")
        .unwrap();
    expected.insert(at, "session_named");
    let mut seen = 0;
    let at_step = expected
        .iter()
        .position(|k| {
            if *k == "step_started" {
                seen += 1;
            }
            seen == 2
        })
        .unwrap();
    expected.insert(at_step + 1, "session_named");
    // Only the subscribe's attach writes `clients`: one line, with count 1.
    let clients: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "clients")
        .collect();
    assert_eq!(clients.len(), 1, "{lines:?}");
    assert_eq!(clients[0]["payload"]["count"], 1);
    let kinds: Vec<&str> = lines
        .iter()
        .filter(|line| line["kind"] != "session_status" && line["kind"] != "clients")
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, expected);
    let before = read_lines(&session_dir(&setup, &id).join("events.jsonl"))
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .count();

    let output = run_to_exit(
        setup.deadline,
        "fiber ask --resume",
        setup.fiber(&["ask", "--resume", &id, "hi"]),
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let resumed_stdout = String::from_utf8(output.stdout).unwrap();
    let resumed_lines: Vec<Value> = resumed_stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect();
    assert_eq!(
        support::kinds(&resumed_lines),
        [
            // A resume writes no `session_started`, and the log already
            // holds an opening message, so none is written either. The
            // pinned call starts, then fails with `name_pinned`.
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
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
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let log = read_lines(&session_dir(&setup, &id).join("events.jsonl"));
    let named: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .collect();
    assert_eq!(named.len(), before, "{log:?}");
    assert_eq!(named.last().unwrap()["payload"]["by"], "person");
    let completed: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "tool_call_completed")
        .collect();
    let last = completed.last().unwrap();
    assert_eq!(last["payload"]["status"], "failed");
    assert_eq!(last["payload"]["error"]["code"], "name_pinned");
    assert_eq!(
        last["payload"]["error"]["message"],
        "the person named this session"
    );
    let status = resumed_stdout
        .lines()
        .filter(|line| is_status(line))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .next_back()
        .expect("a session_status line");
    assert_eq!(status["payload"]["name"], "Mine");
}

#[track_caller]
fn start(mut command: Command, deadline: Deadline) -> Running {
    // The caller builds the command with `setup.fiber(..)`, which runs with
    // the setup root as its working directory (like `ask_with`), so
    // `fiber ask --resume` later finds it there.
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let stderr_copy = Arc::clone(&stderr_text);
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        match std::io::Read::read_to_string(&mut reader, &mut buf) {
            Ok(_) | Err(_) => {}
        }
        *stderr_copy.lock().unwrap() = buf;
    });
    let (tx, stdout_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match tx.send(line.unwrap()) {
                Ok(()) => {}
                Err(mpsc::SendError(_)) => break,
            }
        }
    });
    Running {
        child,
        watchdog,
        group,
        stdout: stdout_rx,
        stderr: stderr_text,
        deadline,
    }
}

#[track_caller]
fn finish(running: Running) {
    let Running {
        mut child,
        watchdog,
        group,
        stdout,
        stderr,
        deadline,
    } = running;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match deadline.recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            expired(deadline, group, &finished, "fiber to exit")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("fiber's wait thread ended before fiber exited")
        }
    };
    assert!(status.success(), "stderr: {}", stderr.lock().unwrap());
    assert!(
        !group_alive(deadline, group),
        "fiber left a process in its group"
    );
    drop(stdout);
    watchdog.stand_down(deadline.cleanup_phase());
}

#[test]
fn the_person_name_pins_refuses_the_model_and_clears() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[name_call("c1", &json!({"name": "first"}))]),
        stream(&[name_call("c2", &json!({"name": "Theirs"}))]),
        stream(&[name_call("c3", &json!({"name": "Again"}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    server.hold();
    let running = start(setup.fiber(&["ask", "go"]), setup.deadline);
    // The first stdout line names the session. It is read as raw text: the
    // byte-for-byte check below needs every durable line's exact bytes, and
    // parsing the first line away would lose them.
    let first_raw: String = setup
        .deadline
        .recv(&running.stdout)
        .expect("waited until the deadline for fiber_started");
    let started: Value = serde_json::from_str(&first_raw).unwrap();
    let id = started["session_id"].as_str().unwrap().to_owned();
    assert!(
        server.await_requests(1, setup.deadline),
        "the held response was requested"
    );

    let client = Socket::connect(setup.deadline, &setup.session_socket(&id));
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let subscribed = until(&client, "the subscribe acknowledgement", |line| {
        line["payload"]["command_id"] == "c_sub"
    });
    assert_eq!(
        answered(&subscribed, "c_sub")["kind"],
        "command_accepted",
        "{subscribed:?}"
    );

    // The reader hands a line to the inbox before it reads the next, so the
    // `tools` probe's answer proves the `name` is queued; a send alone
    // proves only that the bytes were written. The name is taken at step 2's
    // drain, after `c1` completed.
    client.send(r#"{"id":"c_pin","command":"name","args":{"text":"Mine"}}"#);
    client.send(r#"{"id":"c_q1","command":"tools"}"#);
    let queued = until(&client, "the answer to c_q1", |line| {
        line["payload"]["command_id"] == "c_q1"
    });
    assert_eq!(
        answered(&queued, "c_q1")["kind"],
        "command_accepted",
        "{queued:?}"
    );
    server.release_one();
    let pinned = until(&client, "the answer to c_pin", |line| {
        line["payload"]["command_id"] == "c_pin"
    });
    assert_eq!(
        answered(&pinned, "c_pin")["kind"],
        "command_accepted",
        "{pinned:?}"
    );
    assert!(
        server.await_requests(2, setup.deadline),
        "the pinned call sent a second request"
    );

    client.send(r#"{"id":"c_clear","command":"name","args":{"text":""}}"#);
    client.send(r#"{"id":"c_q2","command":"tools"}"#);
    let queued = until(&client, "the answer to c_q2", |line| {
        line["payload"]["command_id"] == "c_q2"
    });
    assert_eq!(
        answered(&queued, "c_q2")["kind"],
        "command_accepted",
        "{queued:?}"
    );
    server.release_one();
    let cleared = until(&client, "the answer to c_clear", |line| {
        line["payload"]["command_id"] == "c_clear"
    });
    assert_eq!(
        answered(&cleared, "c_clear")["kind"],
        "command_accepted",
        "{cleared:?}"
    );
    assert!(
        server.await_requests(3, setup.deadline),
        "the unpinned call sent a third request"
    );

    server.release();
    until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });

    // Stdout's lines, in order: the first line read above, then every line
    // through `fiber_exited`, each under the test's deadline.
    let mut raw = vec![first_raw];
    loop {
        let line: String = setup
            .deadline
            .recv(&running.stdout)
            .expect("waited until the deadline for fiber_exited");
        let parsed: Value = serde_json::from_str(&line).unwrap();
        let done = parsed["kind"] == "fiber_exited";
        raw.push(line);
        if done {
            break;
        }
    }
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    finish(running);

    // The complete, ordered kinds, with `session_status` and `clients` set
    // aside: only the subscribe's attach writes `clients`.
    let kinds: Vec<&str> = lines
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
            "session_named",
            "tool_call_completed",
            "step_started",
            "session_named",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "session_named",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "session_named",
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
    let clients: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "clients")
        .collect();
    assert_eq!(clients.len(), 1, "{lines:?}");
    assert_eq!(clients[0]["payload"]["count"], 1);

    // The four `session_named` payloads, in order; the person's lines are
    // about the session, not a turn, so they carry no `turn_id`.
    let log = read_lines(&session_dir(&setup, &id).join("events.jsonl"));
    let named: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .collect();
    assert_eq!(named.len(), 4, "{log:?}");
    assert_eq!(named[0]["payload"], json!({"name": "first", "by": "model"}));
    assert_eq!(named[1]["payload"], json!({"name": "Mine", "by": "person"}));
    assert_eq!(named[2]["payload"], json!({"name": null, "by": "person"}));
    assert_eq!(named[3]["payload"], json!({"name": "Again", "by": "model"}));
    assert!(named[1].get("turn_id").is_none(), "{}", named[1]);
    assert!(named[2].get("turn_id").is_none(), "{}", named[2]);
    assert!(named[1].get("action_id").is_none(), "{}", named[1]);
    assert!(named[2].get("action_id").is_none(), "{}", named[2]);

    // The pinned call fails `name_pinned`; the call after the clear completes.
    let completed: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), 3, "{log:?}");
    assert_eq!(completed[1]["payload"]["status"], "failed");
    assert_eq!(completed[1]["payload"]["error"]["code"], "name_pinned");
    assert_eq!(
        completed[1]["payload"]["error"]["message"],
        "the person named this session"
    );
    assert_eq!(completed[2]["payload"]["status"], "completed");

    // Stdout's durable lines are the log, byte for byte.
    let stdout_durable: String = raw
        .iter()
        .zip(lines.iter())
        .filter(|(_, line)| line.get("seq").is_some())
        .map(|(line, _)| format!("{line}\n"))
        .collect();
    let log_text = fs::read_to_string(session_dir(&setup, &id).join("events.jsonl")).unwrap();
    assert_eq!(stdout_durable, log_text);
}
