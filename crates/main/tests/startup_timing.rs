//! Startup timers at the binary level (`docs/state.md`, "Diagnostic
//! logs"): the built `fiber` runs `ask` against the fake provider server.
//! At `debug` the pre-attach writer emits `startup.config`,
//! `startup.extensions` and `startup.routing` with only `op` and `ms` in
//! the pid file; at the default level nothing debug is written.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{HELLO_KINDS, Run, Setup, hello, is_status, run_to_exit, write_json};

/// Runs `fiber ask hi` in the workspace to exit and parses its stdout.
#[track_caller]
fn ask(setup: &Setup) -> Run {
    let mut command = setup.fiber(&["ask", "hi"]);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    for entry in fs::read_dir(&logs).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

/// Every `op_timing` line, in file order.
fn op_lines(setup: &Setup) -> Vec<Value> {
    log_lines(setup)
        .into_iter()
        .filter(|line| line["code"] == "op_timing")
        .collect()
}

fn ops(setup: &Setup, op: &str) -> Vec<Value> {
    op_lines(setup)
        .into_iter()
        .filter(|line| line["data"]["op"] == op)
        .collect()
}

fn debug_config(setup: &Setup) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}),
    );
}

/// The log file holding `line`: every startup line is written before the
/// session attaches, so all three sit in the pid file, never the session
/// file.
#[track_caller]
fn file_holding(setup: &Setup, line: &Value) -> String {
    let logs = setup.home().join("logs");
    let mut found = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&logs).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let text = fs::read_to_string(entry.path()).unwrap();
        for raw in text.lines() {
            let value: Value = serde_json::from_str(raw).unwrap();
            if value == *line {
                found.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    assert_eq!(found.len(), 1, "one file holds {line}");
    found.into_iter().next().unwrap()
}

#[test]
fn a_debug_ask_writes_one_startup_line_per_operation_before_attach() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing test really asked"
    );
    assert_eq!(run.kinds(), HELLO_KINDS);
    let session_id = run.session_id().to_owned();
    let startup = ["startup.config", "startup.extensions", "startup.routing"];
    for op in startup {
        assert_eq!(ops(&setup, op).len(), 1, "one {op} line");
    }
    let lines: Vec<Value> = startup
        .into_iter()
        .map(|op| ops(&setup, op).into_iter().next().unwrap())
        .collect();
    for line in &lines {
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        assert_eq!(line["process"], "ask", "{line}");
        assert!(
            line.get("session_id").is_none(),
            "written before attach: {line}"
        );
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let data = line["data"].as_object().expect("data is an object");
        assert_eq!(data.keys().collect::<Vec<_>>(), vec!["ms", "op"], "{line}");
        assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
        let raw = line.to_string();
        for sentinel in ["hi", "Hello.", "sk-test", "null"] {
            assert!(
                !raw.contains(sentinel),
                "no prompt, output, credential or null in {raw}"
            );
        }
    }
    let mut files = lines.iter().map(|line| file_holding(&setup, line));
    let first = files.next().unwrap();
    assert!(
        files.all(|file| file == first),
        "all three lines sit in one file"
    );
    let pid = first
        .strip_prefix("ask-")
        .and_then(|rest| rest.strip_suffix(".log"))
        .unwrap_or("");
    assert!(
        !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()),
        "the pid file holds them: {first}"
    );
    assert_ne!(first, format!("ask-{session_id}.log"));
}

#[test]
fn at_default_no_timing_or_debug_line_is_written() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing test really asked"
    );
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(
        log_lines(&setup)
            .iter()
            .all(|line| line["code"] != "op_timing" && line["level"] != "debug"),
        "the default stays silent"
    );
}

// `routing.switch` through a session socket: one entered preparation
// writes one line, rejections included, while the default level stays
// silent and the durable log matches.

use std::io::{BufRead, BufReader};
use std::process::{Child, ExitStatus};
use std::sync::mpsc;
use std::thread;

use fakes::Watchdog;
use support::{
    Deadline, KillGroup, Socket, complete_lines, expired, kinds, session_dir, spawn_watched, until,
    until_close,
};

/// Installs a provider `fake` with `m` and `n` on `openai-responses` at
/// the fake server, and makes `fake/m` the configured model.
fn install_switch_provider(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    let models = json!([
        {"id": "m", "protocol": "openai-responses",
         "base_url": format!("{}/v1", server.url()), "context_window": 100000},
        {"id": "n", "protocol": "openai-responses",
         "base_url": format!("{}/v1", server.url()), "context_window": 100000},
    ]);
    let mut providers = serde_json::Map::new();
    providers.insert("fake".to_owned(), json!([format!("{}/v1", server.url())]));
    let mut manifest = json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = Value::Object(providers);
    write_json(&source.join("extension.json"), &manifest);
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
        &json!({"model": "fake/m"}),
    );
}

/// Starts `fiber session --id <id> --workspace <workspace>` in its own
/// process group under `spawn_watched`, its stdout drained on a thread
/// and its stderr kept for a failure.
#[track_caller]
fn start_session(setup: &Setup, id: &str) -> Running {
    let workspace = setup.workspace();
    let mut command = setup.fiber(&[
        "session",
        "--id",
        id,
        "--workspace",
        workspace.to_str().unwrap(),
    ]);
    let (mut child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
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
    #[track_caller]
    fn connect(&self, socket: &std::path::Path) -> Socket {
        match self.deadline.recv(&self.lines) {
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
    /// remains of the test's deadline.
    #[track_caller]
    fn wait_for(&self, kind: &str) {
        loop {
            let line = match self.deadline.recv(&self.lines) {
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
    /// The child is reaped once, then its group is checked empty.
    #[track_caller]
    fn wait(mut self) -> (ExitStatus, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match self.deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                expired(self.deadline, self.group, &finished, "the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session's wait thread ended before the session exited")
            }
        };
        assert!(
            !fakes::group_alive(self.group),
            "the session left a process in its group behind"
        );
        std::mem::forget(self.guard);
        loop {
            match self.deadline.recv(&self.lines) {
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for the session's stdout to close")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let stderr = match self.deadline.recv(&self.stderr) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's stderr")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stderr reader ended without its text")
            }
        };
        self.watchdog.stand_down(self.deadline.cleanup_phase());
        (status, stderr)
    }
}

#[track_caller]
fn subscribe(client: &Socket) -> Value {
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let sub = until(client, "the subscribe acknowledgement", |line| {
        line["kind"] == "command_accepted"
    });
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0]["payload"]["command_id"], "c_sub");
    sub.into_iter().next().unwrap()
}

#[track_caller]
fn prompt(client: &Socket, id: &str, text: &str) {
    client.send(&format!(
        r#"{{"id":"{id}","command":"prompt","args":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
    ));
}

#[track_caller]
fn model(client: &Socket, id: &str, reference: &str) {
    client.send(&format!(
        r#"{{"id":"{id}","command":"model","args":{{"model":"{reference}"}}}}"#
    ));
}

#[track_caller]
fn close(client: &Socket) {
    client.send(r#"{"id":"c_close","command":"close"}"#);
}

/// The durable kinds of the session log: every line with a `seq`.
fn log_kinds(setup: &Setup, id: &str) -> Vec<String> {
    let log = std::fs::read_to_string(session_dir(setup, id).join("events.jsonl")).unwrap();
    complete_lines(&log)
        .into_iter()
        .filter(|line| line.get("seq").is_some())
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// One `model fake/n` and one prompt in a single batch, through close:
/// the socket stream to `fiber_exited`.
#[track_caller]
fn switch_journey(setup: &Setup, id: &str) -> Vec<Value> {
    let running = start_session(setup, id);
    let client = running.connect(&setup.session_socket(id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    // A switch admitted while idle applies at once, so the prompt that
    // follows in the same drain starts its turn on the new model.
    model(&client, "c_model", "fake/n");
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
    assert!(
        stream.iter().any(|line| line["kind"] == "model_changed"),
        "the switch applied: {stream:?}"
    );
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(stream.last().unwrap()["kind"], "fiber_exited");
    // The complete ordered kinds, so a duplicated, missing or reordered
    // event fails (docs/testing.md, "Event streams"): the subscribe
    // acknowledgement, the switch admitted while idle, the turn on the
    // new model, then the close. `clients` is written when the one
    // connection attaches, on its own trigger, so it is set aside from
    // the ordered list and its count is asserted instead.
    assert_eq!(
        stream
            .iter()
            .filter(|line| line["kind"] == "clients")
            .count(),
        1,
        "{stream:?}"
    );
    assert_eq!(
        kinds(&stream)
            .into_iter()
            .filter(|kind| *kind != "clients")
            .collect::<Vec<_>>(),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
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
    stream
}

#[test]
fn a_debug_switch_writes_one_routing_line_with_the_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&setup, &server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}),
    );
    let id = doors::mint("s_");
    let _stream = switch_journey(&setup, &id);
    assert!(
        server.await_requests(1, setup.deadline),
        "the new model's request arrived: a passing test really switched"
    );
    let lines: Vec<Value> = op_lines(&setup)
        .into_iter()
        .filter(|line| line["data"]["op"] == "routing.switch")
        .collect();
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert_eq!(line["code"], "op_timing", "{line}");
    assert_eq!(line["level"], "debug", "{line}");
    assert_eq!(line["process"], "session", "{line}");
    assert_eq!(line["session_id"].as_str().unwrap(), id, "{line}");
    assert_eq!(line["message"], "An operation ended.", "{line}");
    let data = line["data"].as_object().expect("data is an object");
    assert_eq!(data.keys().collect::<Vec<_>>(), vec!["ms", "op"], "{line}");
    assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
    let raw = line.to_string();
    for sentinel in ["sk-test", "hi"] {
        assert!(
            !raw.contains(sentinel),
            "no credential or prompt text in {raw}"
        );
    }
}

#[test]
fn at_default_a_switch_writes_no_timing_line_and_leaves_the_same_log() {
    let debug = Setup::new();
    let debug_server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&debug, &debug_server);
    write_json(
        &debug.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}),
    );
    let debug_id = doors::mint("s_");
    switch_journey(&debug, &debug_id);

    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_switch_provider(&setup, &server);
    let id = doors::mint("s_");
    switch_journey(&setup, &id);
    assert!(
        server.await_requests(1, setup.deadline),
        "the new model's request arrived: a passing test really switched"
    );
    assert!(
        log_lines(&setup)
            .iter()
            .all(|line| line["code"] != "op_timing"),
        "the default writes no timing line"
    );
    assert!(!log_kinds(&setup, &id).is_empty());
    // The complete ordered kinds of the durable log: the switch, the
    // turn on the new model, then the exit. Each run meets the same
    // list, so an omitted, duplicated or reordered event fails even when
    // both runs agree (docs/testing.md, "Event streams").
    let expected = [
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "model_changed",
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
    .map(str::to_owned);
    assert_eq!(
        log_kinds(&setup, &id),
        expected,
        "the durable log holds the switch and the turn"
    );
    assert_eq!(
        log_kinds(&debug, &debug_id),
        expected,
        "the debug run leaves the same durable log"
    );
}
