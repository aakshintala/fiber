//! The `model` driver command through the session socket
//! (`docs/testing.md`, "Levels"): switching model or thinking level at the
//! next turn boundary, writing `model_changed` then `preamble_built`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

/// How long one `fiber` run, or one socket line, may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Installs a provider `fake` with `m` and `n` on `openai-responses` at
    /// the fake server, and makes `fake/m` the configured model. `n` takes
    /// `low` and `high`, defaulting to `low`.
    fn provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [
                    {"id": "m", "protocol": "openai-responses",
                     "base_url": format!("{}/v1", server.url()), "context_window": 100000},
                    {"id": "n", "protocol": "openai-responses",
                     "base_url": format!("{}/v1", server.url()), "context_window": 100000,
                     "thinking_levels": ["low", "high"], "thinking_default": "low"},
                ]
            }),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// The session's directory, from its id.
    fn session_dir(&self, id: &str) -> PathBuf {
        log::sessions_dir(&self.home(), &doors::project(&self.workspace())).join(id)
    }

    fn socket(&self, id: &str) -> PathBuf {
        self.home().join("run").join(id)
    }

    /// One `fiber` invocation with `args`: the environment every test
    /// runs under. Stdio is piped; the caller decides how to wait.
    fn fiber(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }

    /// Starts `fiber session --id <id> --workspace <workspace>` with
    /// `extra` appended, in its own process group, its stdout drained on a
    /// thread and its stderr kept for a failure.
    fn start_session(&self, id: &str, extra: &[&str]) -> Running {
        let workspace = self.workspace();
        let mut args = vec!["session", "--id", id, "--workspace"];
        args.push(workspace.to_str().unwrap());
        args.extend(extra);
        let mut command = self.fiber(&args);
        let mut child = command.spawn().unwrap();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
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
            first: Vec::new(),
        }
    }
}

fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
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

/// A running `fiber session`: its stdout drained line by line, its stderr
/// kept for a failure.
struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    guard: KillGroup,
    lines: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
    /// Stdout lines taken as the readiness signal, as written, prepended
    /// to what [`Running::wait`] returns.
    first: Vec<String>,
}

impl Running {
    /// Waits under [`DEADLINE`] for the child's first stdout line, then
    /// connects once. The socket is bound in `Session::open` before the
    /// loop writes that line, so the line is the signal the socket
    /// accepts (`docs/testing.md`, "Waits and timeouts").
    fn connect(&mut self, socket: &Path) -> Socket {
        let line = match self.lines.recv_timeout(DEADLINE) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited {DEADLINE:?} for the session's first stdout line")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session exited before its first stdout line")
            }
        };
        self.first.push(line);
        Socket::connect(socket).expect("the session's socket accepted before the deadline")
    }

    /// Reads stdout lines until one of `kind` arrives, waiting [`DEADLINE`]
    /// for each, and keeps every line for [`Running::wait`]. Only kinds the
    /// loop writes before waiting for a prompt qualify: `preamble_built`
    /// and later need a turn, which needs the test's prompt.
    fn wait_for(&mut self, kind: &str) {
        loop {
            let line = match self.lines.recv_timeout(DEADLINE) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited {DEADLINE:?} for {kind} on the session's stdout")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the session's stdout closed before {kind}")
                }
            };
            let value: Value = serde_json::from_str(&line).unwrap();
            let done = value["kind"] == kind;
            self.first.push(line);
            if done {
                return;
            }
        }
    }

    /// Waits under [`DEADLINE`] for the process to exit, drains its
    /// stdout, and asserts that nothing it started is left in its group.
    fn wait(mut self) -> (ExitStatus, Vec<Value>, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match finished.recv_timeout(DEADLINE) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited {DEADLINE:?} for the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session's wait thread ended before the session exited")
            }
        };
        assert!(
            !group_alive(self.group),
            "the session left a process in its group behind"
        );
        std::mem::forget(self.guard);
        // The process is gone, so its stdout is closed: the drain ends
        // and every line arrives, each waited under DEADLINE. Lines taken
        // as the readiness signal come first.
        let mut raw = std::mem::take(&mut self.first);
        loop {
            match self.lines.recv_timeout(DEADLINE) {
                Ok(line) => raw.push(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited {DEADLINE:?} for the session's stdout to close")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let stderr = self.stderr.recv_timeout(DEADLINE).unwrap_or_default();
        self.watchdog.stand_down(DEADLINE);
        let _ = raw;
        (status, Vec::new(), stderr)
    }
}

/// A client on the session's socket that tells a read deadline from the
/// session closing the socket.
struct Socket {
    write: Mutex<UnixStream>,
    read: Mutex<BufReader<UnixStream>>,
}

impl Socket {
    fn connect(path: &Path) -> std::io::Result<Self> {
        let write = UnixStream::connect(path)?;
        let read = write.try_clone()?;
        read.set_read_timeout(Some(DEADLINE))?;
        Ok(Self {
            write: Mutex::new(write),
            read: Mutex::new(BufReader::new(read)),
        })
    }

    fn send(&self, line: &str) {
        let mut write = self.write.lock().unwrap();
        write.write_all(line.as_bytes()).unwrap();
        if !line.ends_with('\n') {
            write.write_all(b"\n").unwrap();
        }
        write.flush().unwrap();
    }

    /// One socket line: a line, or `None` when the session closed the
    /// socket. A [`DEADLINE`] with neither panics naming `what`, with
    /// the lines before it.
    fn next(&self, what: &str, got: &[Value]) -> Option<Value> {
        let mut buf = String::new();
        match self.read.lock().unwrap().read_line(&mut buf) {
            Ok(0) => None,
            Ok(_) => {
                let line = buf.trim_end_matches(&['\r', '\n'][..]).to_owned();
                Some(serde_json::from_str(&line).unwrap_or(Value::String(line)))
            }
            Err(error)
                if error.kind() == ErrorKind::TimedOut || error.kind() == ErrorKind::WouldBlock =>
            {
                panic!("waited {DEADLINE:?} for {what}; got {got:?}")
            }
            Err(error) => {
                panic!("reading the session socket while waiting for {what}: {error}")
            }
        }
    }
}

fn send(client: &Socket, line: &str) {
    client.send(line);
}

/// Collects lines until `done`, waiting [`DEADLINE`] for each: expiry panics
/// naming `what`, and the socket closing first panics too.
fn until(client: &Socket, what: &str, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
    let mut lines = Vec::new();
    loop {
        let line = match client.next(what, &lines) {
            Some(line) => line,
            None => {
                panic!("the session closed the socket while waiting for {what}; got {lines:?}")
            }
        };
        let stop = done(&line);
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

/// Collects socket lines until the session closes the socket, waiting
/// [`DEADLINE`] for each.
fn until_close(client: &Socket) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.next("the session to close the socket", &lines) {
        lines.push(line);
    }
    lines
}

/// The event kinds of `lines`, in order, without `session_status`: an
/// observer thread writes it, so where it falls among the loop's own
/// lines is not what these tests pin.
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// An `openai-responses` stream thinking `thought`, then answering
/// `Hello.`.
fn reasoning_hello(thought: &str) -> Response {
    stream(&[
        json!({"type": "response.reasoning_summary_text.delta", "delta": thought}),
        json!({"type": "response.output_item.done", "item": {
            "type": "reasoning", "id": "rs_1",
            "summary": [{"type": "summary_text", "text": thought}]
        }}),
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// An `openai-responses` stream calling the `shell` tool.
fn shell_call() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_call_1",
        "call_id": "call_1",
        "name": "shell",
        "arguments": json!({"command": "echo hi"}).to_string()
    }})])
}

fn stream(events: &[Value]) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }});
    body.push_str(&format!(
        "event: {}\ndata: {done}\n\n",
        done["type"].as_str().unwrap()
    ));
    Response::stream(body)
}

fn subscribe(client: &Socket) -> Value {
    send(
        client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = until(client, "the subscribe acknowledgement", |line| {
        line["kind"] == "command_accepted"
    });
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0]["payload"]["command_id"], "c_sub");
    sub.into_iter().next().unwrap()
}

fn prompt(client: &Socket, id: &str, text: &str) {
    send(
        client,
        &format!(
            r#"{{"id":"{id}","command":"prompt","args":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        ),
    );
}

fn model(client: &Socket, id: &str, reference: &str, thinking: Option<&str>) {
    let thinking = thinking.map_or(String::new(), |level| format!(r#","thinking":"{level}""#));
    send(
        client,
        &format!(r#"{{"id":"{id}","command":"model","args":{{"model":"{reference}"{thinking}}}}}"#),
    );
}

fn close(client: &Socket) {
    send(client, r#"{"id":"c_close","command":"close"}"#);
}

/// The request bodies the fake server saw, in arrival order.
fn bodies(server: &ProviderServer) -> Vec<Value> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/responses")
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

/// The durable kinds of the session log: every line with a `seq`.
fn log_kinds(setup: &Setup, id: &str) -> Vec<String> {
    let log = fs::read_to_string(setup.session_dir(id).join("events.jsonl")).unwrap();
    log.lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line.get("seq").is_some())
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_model_sent_between_turns_applies_at_the_next_turn_boundary() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    assert!(
        stream
            .iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the fake model's text arrived: {stream:?}"
    );

    model(&client, "c_model", "fake/n", None);
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    let accepted = stream
        .iter()
        .find(|line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_model"
        })
        .unwrap();
    assert_eq!(accepted["payload"]["command_id"], "c_model");
    let changed = stream.last().unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/m");
    assert_eq!(changed["payload"]["after"]["model"], "fake/n");
    assert_eq!(changed["payload"]["after"]["thinking"], "low");
    assert_eq!(changed["payload"]["source"], "driver");

    prompt(&client, "c_again", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "switch");
    assert_eq!(built["payload"]["model"], "fake/n");
    assert_eq!(built["payload"]["thinking"], "low");

    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
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
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "model_changed",
            "preamble_built",
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
    let seen = bodies(&server);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0]["model"], "m");
    assert_eq!(seen[1]["model"], "n");
}

#[test]
fn a_model_then_a_prompt_in_one_batch_runs_the_prompt_on_the_new_model() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    // A switch admitted while idle applies at once, so the prompt that
    // follows in the same drain starts its turn on the new model.
    model(&client, "c_model", "fake/n", None);
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
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
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
    let changed = stream
        .iter()
        .find(|line| line["kind"] == "model_changed")
        .unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/m");
    assert_eq!(changed["payload"]["after"]["model"], "fake/n");
    let seen = bodies(&server);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["model"], "n");
}

#[test]
fn a_model_sent_during_a_turn_applies_after_turn_completed() {
    let setup = Setup::new();
    let server = ProviderServer::start([shell_call(), hello(), hello()]).unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "run it");
    stream.extend(until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    }));
    let request = stream.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // During the approval wait the switch is accepted but waits for the
    // next turn boundary.
    model(&client, "c_model", "fake/n", None);
    stream.extend(until(&client, "the model acknowledgement", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_model"
    }));
    send(
        &client,
        &format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request}","decision":"allow"}}}}"#
        ),
    );
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    let tail = &stream[stream
        .iter()
        .position(|line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_model"
        })
        .unwrap()..];
    // The switch is accepted before the turn completes, and applies after.
    assert_eq!(
        kinds(tail),
        [
            "command_accepted",
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
            "model_changed",
        ]
    );
    let changed = stream.last().unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/m");
    assert_eq!(changed["payload"]["after"]["model"], "fake/n");
    // The turn that took the switch finishes on the old model; the next
    // turn runs on the new one.
    let during = bodies(&server);
    assert_eq!(during.len(), 2, "{during:?}");
    assert_eq!(during[0]["model"], "m");
    assert_eq!(during[1]["model"], "m");
    prompt(&client, "c_after", "after the switch");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "switch");
    assert_eq!(built["payload"]["model"], "fake/n");
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let seen = bodies(&server);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(seen[0]["model"], "m");
    assert_eq!(seen[1]["model"], "m");
    assert_eq!(seen[2]["model"], "n");
}

#[test]
fn reasoning_stays_with_the_model_that_produced_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        reasoning_hello("thought-alpha-xyz"),
        reasoning_hello("thought-beta-xyz"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_first", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    assert!(
        stream
            .iter()
            .any(|line| line["kind"] == "reasoning_completed"),
        "the first turn reasoned: {stream:?}"
    );
    // Switching model drops the earlier model's reasoning from the next
    // request.
    model(&client, "c_model", "fake/n", None);
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    prompt(&client, "c_second", "again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let seen = bodies(&server);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[1]["model"], "n");
    assert!(
        !seen[1].to_string().contains("thought-alpha-xyz"),
        "no reasoning item from the earlier model is sent: {}",
        seen[1]
    );
    // A thinking-only switch keeps the same model's reasoning.
    model(&client, "c_level", "fake/n", Some("high"));
    stream.extend(until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    }));
    let changed = stream.last().unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "fake/n");
    assert_eq!(changed["payload"]["after"]["model"], "fake/n");
    assert_eq!(changed["payload"]["before"]["thinking"], "low");
    assert_eq!(changed["payload"]["after"]["thinking"], "high");
    prompt(&client, "c_third", "once more");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    let seen = bodies(&server);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(
        seen[2].to_string().contains("thought-beta-xyz"),
        "the same model's reasoning is still sent: {}",
        seen[2]
    );
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn an_unknown_model_is_rejected_and_changes_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    model(&client, "c_model", "nope/x", None);
    stream.extend(until(&client, "command_rejected", |line| {
        line["kind"] == "command_rejected"
    }));
    let rejected = stream.last().unwrap();
    assert_eq!(rejected["payload"]["command_id"], "c_model");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
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
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_rejected",
            "command_accepted",
            "fiber_exited",
        ]
    );
    assert!(
        !log_kinds(&setup, &id).contains(&"model_changed".to_owned()),
        "a rejected model writes no line"
    );
}

#[test]
fn an_unsupported_thinking_level_is_rejected() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    let mut stream = vec![subscribe(&client)];
    prompt(&client, "c_prompt", "hi");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    // `m` takes no thinking level.
    model(&client, "c_model", "fake/m", Some("high"));
    stream.extend(until(&client, "command_rejected", |line| {
        line["kind"] == "command_rejected"
    }));
    let rejected = stream.last().unwrap();
    assert_eq!(rejected["payload"]["command_id"], "c_model");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    close(&client);
    stream.extend(until_close(&client));
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    assert!(
        !log_kinds(&setup, &id).contains(&"model_changed".to_owned()),
        "a rejected level writes no line"
    );
}

#[test]
fn a_resume_restores_the_switched_model_and_level() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(&client);
    prompt(&client, "c_first", "hi");
    until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    model(&client, "c_model", "fake/n", Some("high"));
    until(&client, "model_changed", |line| {
        line["kind"] == "model_changed"
    });
    prompt(&client, "c_second", "again");
    let second = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let built = second
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "switch");
    assert_eq!(built["payload"]["model"], "fake/n");
    assert_eq!(built["payload"]["thinking"], "high");
    close(&client);
    until_close(&client);
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");

    let mut resumed = setup.start_session(&id, &["--resume"]);
    let again = resumed.connect(&setup.socket(&id));
    subscribe(&again);
    until(&again, "resumed fiber_started", |line| {
        line["kind"] == "fiber_started" && line["payload"]["resumed"] == true
    });
    prompt(&again, "c_third", "a third turn");
    let third = until(&again, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert_eq!(
        kinds(&third),
        [
            "extensions_loaded",
            "clients",
            "preamble_built",
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
        ]
    );
    let built = third
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["model"], "fake/n");
    assert_eq!(built["payload"]["thinking"], "high");
    close(&again);
    until_close(&again);
    drop(again);
    let (status, _, stderr) = resumed.wait();
    assert!(status.success(), "stderr: {stderr}");
    let seen = bodies(&server);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(seen[2]["model"], "n");
    assert_eq!(seen[2]["reasoning"]["effort"], "high");
}
