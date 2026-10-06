//! Binary-level tests of the internal session command
//! (`docs/invocation.md`, "Processes"): the built `fiber` runs in its own
//! process group with its own `FIBER_HOME`, holding an ordinary provider
//! whose base URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{Client, ProviderServer, Response, Watchdog};
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

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
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
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url())}]
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

    /// An idle session exits at once: `session.idle_exit_ms` 0, keeping the
    /// configured model.
    fn no_idle(&self) {
        write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m", "session": {"idle_exit_ms": 0}}),
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
        self.start_session_in(id, &workspace, extra)
    }

    /// [`Setup::start_session`] in `workspace`: a second workspace is a
    /// second project, so the same id names another project's session.
    fn start_session_in(&self, id: &str, workspace: &Path, extra: &[&str]) -> Running {
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

    /// Runs `fiber` once with `args`, waiting under [`DEADLINE`]. The
    /// wait runs on a thread and is received under the deadline, so a
    /// hang reports what it waited for (`docs/testing.md`, "Waits and
    /// timeouts").
    fn run(&self, args: &[&str]) -> (Option<i32>, Vec<Value>, String) {
        let mut command = self.fiber(args);
        let child = command.spawn().unwrap();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                assert!(
                    !group_alive(group),
                    "`fiber` left a process in its group behind"
                );
                panic!(
                    "waited {DEADLINE:?} for `fiber {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
        };
        assert!(
            !group_alive(group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
        let lines = String::from_utf8(output.stdout).unwrap();
        let parsed = lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (
            output.status.code(),
            parsed,
            String::from_utf8(output.stderr).unwrap(),
        )
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
    /// Stdout lines taken as the readiness signal, prepended to what
    /// [`Running::wait`] returns.
    first: Vec<Value>,
}

impl Running {
    /// Waits under [`DEADLINE`] for the child's first stdout line, then
    /// connects once. The socket is bound in `Session::open` before the
    /// loop writes that line, so the line is the signal the socket
    /// accepts (`docs/testing.md`, "Waits and timeouts").
    fn connect(&mut self, socket: &Path) -> Client {
        let line = self
            .lines
            .recv_timeout(DEADLINE)
            .expect("the session's first stdout line before the deadline");
        self.first.push(serde_json::from_str(&line).unwrap());
        Client::connect(socket).expect("the session's socket accepted before the deadline")
    }

    /// Reads stdout lines until one of `kind` arrives, waiting [`DEADLINE`]
    /// for each, and keeps every line for [`Running::wait`]. Only kinds the
    /// loop writes before waiting for a prompt qualify: `preamble_built`
    /// and later need a turn, which needs the test's prompt.
    fn wait_for(&mut self, kind: &str) {
        loop {
            let line = self.lines.recv_timeout(DEADLINE).unwrap_or_else(|_| {
                panic!("waited {DEADLINE:?} for {kind} on the session's stdout")
            });
            let value: Value = serde_json::from_str(&line).unwrap();
            let done = value["kind"] == kind;
            self.first.push(value);
            if done {
                return;
            }
        }
    }

    /// Waits under [`DEADLINE`] for the process to exit, and asserts that
    /// nothing it started is left in its group.
    fn wait(mut self) -> (ExitStatus, Vec<Value>, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = finished
            .recv_timeout(DEADLINE)
            .expect("waited for the session to exit")
            .unwrap();
        assert!(
            !group_alive(self.group),
            "the session left a process in its group behind"
        );
        std::mem::forget(self.guard);
        // The process is gone, so its stdout is closed: the drain ends
        // and every line arrives, each waited under DEADLINE. Lines taken
        // as the readiness signal come first.
        let mut out = std::mem::take(&mut self.first);
        while let Ok(line) = self.lines.recv_timeout(DEADLINE) {
            out.push(serde_json::from_str(&line).unwrap());
        }
        let stderr = self.stderr.recv_timeout(DEADLINE).unwrap_or_default();
        self.watchdog.stand_down(DEADLINE);
        (status, out, stderr)
    }
}

/// An `openai-responses` stream of `events`, then a completed reply.
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

/// A finished `function_call` for `name` with `arguments`.
fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
fn hello() -> Response {
    let events = [
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect();
    Response::stream(body)
}

fn send(client: &Client, line: &str) {
    client.send(line).unwrap();
}

fn recv(client: &Client) -> Value {
    client.recv(DEADLINE).expect("a line arrived")
}

/// Collects socket lines until the session closes the socket, waiting
/// [`DEADLINE`] for each.
fn until_close(client: &Client) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.recv(DEADLINE) {
        lines.push(line);
    }
    lines
}

/// The event kinds of `lines`, in order, without `session_status`: an
/// observer thread writes it, so where it falls among the loop's own
/// lines is not what these tests pin (as `tests/ask.rs` filters it).
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}
/// Collects lines until `done`, waiting `DEADLINE` for each.
fn until(client: &Client, what: &str, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
    let mut lines = Vec::new();
    loop {
        let line = client
            .recv(DEADLINE)
            .unwrap_or_else(|| panic!("waited {DEADLINE:?} for {what}; got {lines:?}"));
        let stop = done(&line);
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

#[test]
fn a_prompt_over_the_socket_runs_a_turn_and_close_ends_the_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    // Behind the loop's startup: `extensions_loaded` is on stdout, so
    // every startup line is in the log and `clients` lands after them.
    // The loop parks waiting for a prompt, so nothing else moves.
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client);
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    let rest = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let turn_started = rest
        .iter()
        .find(|line| line["kind"] == "turn_started")
        .expect("the prompt started a turn");
    assert_eq!(
        turn_started["payload"]["input"][0]["content"][0]["text"],
        "hi"
    );
    assert!(
        rest.iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the fake model's text arrived: {rest:?}"
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    assert!(!setup.socket(&id).exists());
    // The session was prompted, so its directory remains.
    assert!(setup.session_dir(&id).join("events.jsonl").is_file());
    let mut stream = vec![sub];
    stream.extend(rest);
    stream.extend(tail);
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
            "fiber_exited",
        ]
    );
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
}

#[test]
fn a_prompt_naming_a_skill_sends_the_expanded_text_as_one_message() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    // The `review-pr` skill, as `tests/ask.rs` installs it: the prompt
    // driver command expands the same way (`see #532`).
    let dir = setup.root.path().join("w/.agents/skills/review-pr");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: review-pr\ndescription: Reviews a pull request.\n---\nReview the pull request named in the arguments.\n",
    )
    .unwrap();
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    // Behind the loop's startup: `extensions_loaded` is on stdout, so
    // every startup line is in the log and `clients` lands after them.
    // The loop parks waiting for a prompt, so nothing else moves.
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client);
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"/review-pr 42"}]}}"#,
    );
    let rest = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let expanded = "Review the pull request named in the arguments.\n\n42";
    let turn_started = rest
        .iter()
        .find(|line| line["kind"] == "turn_started")
        .expect("the prompt started a turn");
    assert_eq!(
        turn_started["payload"]["input"][0]["content"][0]["text"],
        expanded
    );
    // The fake provider's received user message carries the expanded text.
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        String::from_utf8_lossy(&requests[0].body)
            .contains(&serde_json::to_string(expanded).unwrap())
    );
    assert!(
        rest.iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the fake model's text arrived: {rest:?}"
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    assert!(!setup.socket(&id).exists());
    // The session was prompted, so its directory remains.
    assert!(setup.session_dir(&id).join("events.jsonl").is_file());
    let mut stream = vec![sub];
    stream.extend(rest);
    stream.extend(tail);
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
            "fiber_exited",
        ]
    );
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
}

#[test]
fn an_idle_session_exits_with_a_client_still_connected() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    setup.no_idle();
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &["--prompt", "hi"]);

    // The turn is held at the provider, so the client subscribes once the
    // request is in flight: every line before it is written, and the held
    // reply writes nothing until it is released.
    let client = running.connect(&setup.socket(&id));
    assert!(
        server.await_requests(1, DEADLINE),
        "the held response was requested"
    );
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client);
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    assert!(
        server.await_requests(1, DEADLINE),
        "the held response was requested"
    );
    server.release();

    let tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    let mut stream = vec![sub];
    stream.extend(tail);
    assert_eq!(
        kinds(&stream),
        [
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "clients",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(
        kinds(&out),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "clients",
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
fn a_session_that_never_got_a_prompt_leaves_nothing_behind() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.no_idle();
    let id = doors::mint("s_");
    let running = setup.start_session(&id, &[]);

    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    // No prompt, so no preamble and no turn: the opening lines, then the exit.
    assert_eq!(
        kinds(&out),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ]
    );
    assert!(
        !setup.session_dir(&id).exists(),
        "a session with no turn_started deletes its own directory"
    );
    assert!(!setup.socket(&id).exists());
    assert!(server.requests().is_empty());
}

#[test]
fn a_bad_id_is_a_usage_error_and_creates_nothing() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    let (code, lines, stderr) = setup.run(&[
        "session",
        "--id",
        "../x",
        "--workspace",
        workspace.to_str().unwrap(),
    ]);

    assert_eq!(code, Some(2));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "fiber_exited");
    assert_eq!(lines[0].get("session_id"), None);
    assert_eq!(lines[0]["payload"]["exit_code"], 2);
    assert!(stderr.contains("session id"), "{stderr}");
    assert!(!setup.home().join("projects").exists());
    assert!(!setup.home().join("run").exists());
}

#[test]
fn an_id_whose_session_directory_exists_fails_before_any_session_line() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let dir = setup.session_dir(&id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("marker"), "untouched\n").unwrap();

    let workspace = setup.workspace();
    let (code, lines, stderr) = setup.run(&[
        "session",
        "--id",
        &id,
        "--workspace",
        workspace.to_str().unwrap(),
    ]);

    assert_ne!(code, Some(0), "stderr: {stderr}");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "fiber_exited");
    assert_eq!(lines[0].get("session_id"), None);
    assert_eq!(
        fs::read_to_string(dir.join("marker")).unwrap(),
        "untouched\n"
    );
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        1,
        "the existing directory is untouched"
    );
    assert!(!setup.socket(&id).exists());
    assert!(server.requests().is_empty());
}

#[test]
fn an_escalation_reaches_a_connected_client_and_its_allow_runs_the_call() {
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
    // A standing ask for this exact command: with a client connected the
    // loop must ask a person (`docs/permissions.md`, "Headless").
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
    // Behind the loop's startup: `extensions_loaded` is on stdout, so
    // every startup line is in the log and `clients` lands after them.
    // The loop parks waiting for a prompt, so nothing else moves.
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client);
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"run it"}]}}"#,
    );
    let asked = until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    });
    let request = asked.last().expect("the escalation was requested");
    assert_eq!(request["payload"]["step"], "standing_ask");
    assert_eq!(request["payload"]["standing_rule"]["prefix"], "echo hi");
    let request_id = request["payload"]["request_id"].as_str().unwrap();
    send(
        &client,
        &format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request_id}","decision":"allow"}}}}"#
        ),
    );
    let decided = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let resolved = decided
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .expect("the reply resolved the request");
    assert_eq!(resolved["payload"]["decision"], "allow");
    assert_eq!(resolved["payload"]["decided_by"], "person");
    let completed = decided
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .expect("the allowed call ran");
    assert_eq!(completed["payload"]["status"], "completed");
    assert!(
        decided
            .iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the turn finished after the allowed call: {decided:?}"
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    let mut stream = vec![sub];
    stream.extend(asked);
    stream.extend(decided);
    stream.extend(tail);
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
}

#[test]
fn a_second_session_with_the_same_id_fails_and_leaves_the_first_alone() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    // Another workspace is another project, so the same id passes the
    // project-local session check and reaches the shared socket.
    let other = setup.root.path().join("w2");
    fs::create_dir_all(&other).unwrap();
    let id = doors::mint("s_");
    let mut first = setup.start_session(&id, &[]);
    let client = first.connect(&setup.socket(&id));

    let (code, lines, stderr) = setup.run(&[
        "session",
        "--id",
        &id,
        "--workspace",
        other.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "fiber_exited");
    assert_eq!(lines[0].get("session_id"), None);
    assert_eq!(lines[0]["payload"]["exit_code"], 1);
    assert_eq!(lines[0]["payload"]["error"]["code"], "session_held");
    // The second session's own directory is removed, and the first
    // session's socket still answers.
    let second_dir = log::sessions_dir(&setup.home(), &doors::project(&other)).join(&id);
    assert!(!second_dir.exists());
    assert!(setup.socket(&id).exists());

    // The first session serves its socket after the refusal. Behind its
    // startup: `extensions_loaded` is on stdout, so `clients` lands after
    // every startup line. The loop parks waiting for a prompt.
    first.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client);
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    let rest = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        rest.iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the first session answered after the refusal: {rest:?}"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = first.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    let mut stream = vec![sub];
    stream.extend(rest);
    stream.extend(tail);
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
            "fiber_exited",
        ]
    );
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
}
