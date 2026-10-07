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

mod support;

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, group_alive};

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
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
        self.provider_with(&json!({}), server);
    }

    /// [`Setup::provider`], with the fake model declaring `input`
    /// `["text", "image"]`, so a pasted image is sent as an image part.
    fn provider_with_images(&self, server: &ProviderServer) {
        self.provider_with(&json!({"input": ["text", "image"]}), server);
    }

    fn provider_with(&self, model_extra: &Value, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        let mut model = json!({"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000});
        for (key, value) in model_extra.as_object().unwrap() {
            model[key] = value.clone();
        }
        write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [model]
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
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                support::kill_group(self.deadline, group, "KILL").unwrap();
                let reaped = match finished.recv_timeout(self.deadline.cleanup()) {
                    Ok(_) => true,
                    Err(mpsc::RecvTimeoutError::Timeout) => false,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        panic!(
                            "the `fiber {}` wait thread ended without an exit after the kill",
                            args.join(" ")
                        )
                    }
                };
                assert!(
                    !group_alive(self.deadline, group),
                    "`fiber` left a process in its group behind"
                );
                panic!(
                    "waited until the deadline for `fiber {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "the `fiber {}` wait thread ended before it exited",
                    args.join(" ")
                )
            }
        };
        assert!(
            !group_alive(self.deadline, group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
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

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match support::kill_group_detached(self.0, "KILL") {
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
        let line = match self.lines.recv_timeout(self.deadline.left()) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's first stdout line")
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
            let line = match self.lines.recv_timeout(self.deadline.left()) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for {kind} on the session's stdout")
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

    /// Waits under [`DEADLINE`] for the process to exit, and asserts that
    /// nothing it started is left in its group.
    fn wait(self) -> (ExitStatus, Vec<Value>, String) {
        let (status, raw, stderr) = self.wait_raw();
        let out = raw
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (status, out, stderr)
    }

    /// [`Running::wait`], with stdout's lines as written.
    fn wait_raw(mut self) -> (ExitStatus, Vec<String>, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match finished.recv_timeout(self.deadline.left()) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session's wait thread ended before the session exited")
            }
        };
        assert!(
            !group_alive(self.deadline, self.group),
            "the session left a process in its group behind"
        );
        std::mem::forget(self.guard);
        // The process is gone, so its stdout is closed: the drain ends
        // and every line arrives, each waited under DEADLINE. A deadline
        // with no line is a hang, not the end: only the drain thread
        // ending (the channel disconnecting) ends the run. Lines taken
        // as the readiness signal come first.
        let mut out = std::mem::take(&mut self.first);
        loop {
            match self.lines.recv_timeout(self.deadline.left()) {
                Ok(line) => out.push(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for the session's stdout to close")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let stderr = match self.stderr.recv_timeout(self.deadline.left()) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's stderr")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => String::new(),
        };
        self.watchdog.stand_down(self.deadline.cleanup());
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
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// A client on the session's socket that tells a read deadline from the
/// session closing the socket. Reads wait [`DEADLINE`] each: expiry
/// panics naming what was awaited, while the session closing the socket
/// ends [`until_close`] and panics from [`recv`] and [`until`] naming
/// the wait the close cut short.
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
                panic!("waited until the deadline for {what}; got {got:?}")
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

fn recv(client: &Socket, what: &str) -> Value {
    match client.next(what, &[]) {
        Some(line) => line,
        None => panic!("the session closed the socket while waiting for {what}"),
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
/// lines is not what these tests pin (as `tests/ask.rs` filters it).
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}
/// The socket's event kinds for one prompt and `close`: the subscribe, the
/// prompt's turn, and the close. The prompt-over-the-socket, skill and
/// same-id tests run exactly this shape, so they share it.
const SOCKET_KINDS_ONE_TURN_AND_CLOSE: [&str; 19] = [
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
];

/// The same run's stdout kinds: the loop's own lines, without the
/// socket's `command_accepted` echoes.
const STDOUT_KINDS_ONE_TURN_AND_CLOSE: [&str; 16] = [
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
];

/// Collects lines up to this connection's own `clients` line. The
/// session writes it after the subscribe acknowledgement, with no order
/// against lines the loop writes meanwhile (`docs/events.md`, `clients`).
fn until_clients(client: &Socket) -> Vec<Value> {
    until(client, "the clients line", |line| line["kind"] == "clients")
}

/// Collects lines until `done`, waiting `DEADLINE` for each: expiry panics
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
    let sub = recv(&client, "the subscribe acknowledgement");
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
    assert_eq!(kinds(&stream), SOCKET_KINDS_ONE_TURN_AND_CLOSE);
    assert_eq!(kinds(&out), STDOUT_KINDS_ONE_TURN_AND_CLOSE);
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
    let sub = recv(&client, "the subscribe acknowledgement");
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
    assert_eq!(kinds(&stream), SOCKET_KINDS_ONE_TURN_AND_CLOSE);
    assert_eq!(kinds(&out), STDOUT_KINDS_ONE_TURN_AND_CLOSE);
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
        server.await_requests(1, setup.deadline.left()),
        "the held response was requested"
    );
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    // `clients` is written after the acknowledgement, so the reply is
    // released only once it has arrived; otherwise the deltas may come first.
    let attached = until_clients(&client);
    server.release();

    let tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    let mut stream = vec![sub];
    stream.extend(attached);
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
fn a_session_started_with_a_prompt_keeps_serving_after_that_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    // Default idle: the session stays up after its first turn. The first
    // response is held so the client subscribes before that turn
    // completes; the hold stays on for the second turn's request.
    server.hold();
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &["--prompt", "hi"]);

    let client = running.connect(&setup.socket(&id));
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the held first response was requested"
    );
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    let attached = until_clients(&client);
    server.release_one();
    let first = until(&client, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let started = attached
        .iter()
        .chain(&first)
        .find(|line| line["kind"] == "turn_started")
        .expect("the queued prompt started a turn");
    assert_eq!(started["payload"]["input"][0]["content"][0]["text"], "hi");
    assert!(
        first
            .iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the first turn ran: {first:?}"
    );

    // The session keeps serving: a second prompt over the socket runs a
    // second turn. With `one_turn` forced on, the session would have
    // closed after the first turn and this prompt would never run.
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"again"}]}}"#,
    );
    assert!(
        server.await_requests(2, setup.deadline.left()),
        "the held second response was requested"
    );
    server.release();
    let second = until(&client, "the second turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let restarted = second
        .iter()
        .find(|line| line["kind"] == "turn_started")
        .expect("the second prompt started a turn");
    assert_eq!(
        restarted["payload"]["input"][0]["content"][0]["text"],
        "again"
    );
    assert!(
        second
            .iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the second turn ran: {second:?}"
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    assert_eq!(
        tail.last()
            .expect("the close was answered and the session exited")["kind"],
        "fiber_exited"
    );
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    assert!(!setup.socket(&id).exists());
    assert_eq!(server.requests().len(), 2);
    let mut stream = vec![sub];
    stream.extend(attached);
    stream.extend(first);
    stream.extend(second);
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
fn a_served_session_whose_last_turn_failed_exits_0() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    // A zero budget fails the turn before the provider is called, and
    // idle exit 0 ends the session once that turn is done.
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "budget": {"usd": 0}, "session": {"idle_exit_ms": 0}}),
    );
    let id = doors::mint("s_");
    let running = setup.start_session(&id, &["--prompt", "hi"]);

    let (status, raw, stderr) = running.wait_raw();
    let out: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        status.success(),
        "{status}; fiber_exited: {}; stderr: {stderr}",
        out.last().unwrap()
    );
    assert_eq!(stderr, "");
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
            "turn_completed",
            "fiber_exited",
        ]
    );
    // Its client saw the failure on the turn's own `turn_completed`.
    let completed = out
        .iter()
        .find(|line| line["kind"] == "turn_completed")
        .unwrap();
    assert_eq!(completed["payload"]["outcome"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "budget_exceeded");
    // The process itself did not fail (`docs/errors.md`, "What a caller
    // gets"): exit 0, no `error` and no final message.
    let exited = &out.last().unwrap()["payload"];
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited.get("error"), None);
    assert_eq!(exited.get("text"), None);
    assert!(server.requests().is_empty());

    // Stdout filtered to this session's durable lines is the log, byte for
    // byte.
    let durable: String = raw
        .iter()
        .zip(&out)
        .filter(|(_, l)| l.get("seq").is_some() && l["session_id"] == id.as_str())
        .map(|(raw, _)| format!("{raw}\n"))
        .collect();
    assert_eq!(
        fs::read_to_string(setup.session_dir(&id).join("events.jsonl")).unwrap(),
        durable
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
    let sub = recv(&client, "the subscribe acknowledgement");
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
    let sub = recv(&client, "the subscribe acknowledgement");
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
    assert_eq!(kinds(&stream), SOCKET_KINDS_ONE_TURN_AND_CLOSE);
    assert_eq!(kinds(&out), STDOUT_KINDS_ONE_TURN_AND_CLOSE);
}

/// Starts a session on a standing ask that idle-exits on it at once, and
/// returns the request it stopped on, after restoring the default idle
/// delay for the resume.
fn suspend_on_an_approval(setup: &Setup, id: &str) -> String {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    setup.no_idle();
    let running = setup.start_session(id, &["--prompt", "run it"]);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    let pending = exited["payload"]["suspended_on"]
        .as_str()
        .expect("the session exited suspended on the approval")
        .to_owned();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m"}),
    );
    pending
}

#[test]
fn a_resumed_session_raises_its_request_again_and_a_reply_runs_the_call() {
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
    let id = doors::mint("s_");
    let pending = suspend_on_an_approval(&setup, &id);

    let mut running = setup.start_session(&id, &["--resume"]);
    let client = running.connect(&setup.socket(&id));
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    // The replay holds the first process's request; the resumed process
    // raises it again after its own `fiber_started`.
    let mut resumed = false;
    let replay = until(&client, "the re-raised permission_requested", |line| {
        if line["kind"] == "fiber_started" && line["payload"]["resumed"] == true {
            resumed = true;
        }
        resumed && line["kind"] == "permission_requested"
    });
    let raised = replay.last().unwrap();
    assert_eq!(raised["payload"]["request_id"], pending.as_str());

    // A second resume while this one runs fails on the held lock and
    // writes nothing to the log.
    let (code, lines, stderr) = setup.run(&[
        "session",
        "--id",
        &id,
        "--workspace",
        setup.workspace().to_str().unwrap(),
        "--resume",
    ]);
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["payload"]["error"]["code"], "session_held");

    send(
        &client,
        &format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{pending}","decision":"allow"}}}}"#
        ),
    );
    let decided = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let accepted = decided
        .iter()
        .find(|line| line["kind"] == "command_accepted")
        .expect("the reply was accepted");
    assert_eq!(accepted["payload"]["command_id"], "c_reply");
    let resolved = decided
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .expect("the reply resolved the request");
    assert_eq!(resolved["payload"]["request_id"], pending.as_str());
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
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");

    // One log, one writer at a time: one `session_started`, the second
    // `fiber_started` resumed, and `seq` carrying on without a gap.
    let text = fs::read_to_string(setup.session_dir(&id).join("events.jsonl")).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["kind"] == "session_started")
            .count(),
        1
    );
    let started: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "fiber_started")
        .collect();
    assert_eq!(started.len(), 2);
    assert_eq!(started[1]["payload"]["resumed"], true);
    let seqs: Vec<u64> = lines
        .iter()
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    assert!(
        seqs.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "{seqs:?}"
    );
}

/// Writes a skill under the workspace's `.agents/skills/<name>/`.
fn workspace_skill(setup: &Setup, name: &str, header: &str) {
    let dir = setup.workspace().join(".agents/skills").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\n{header}---\nBody.\n"),
    )
    .unwrap();
}

/// Subscribes `client`, sends `commands`, and returns its answer's `result`.
fn commands_answer(client: &Socket) -> Value {
    send(
        client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(client, r#"{"id":"c_cmds","command":"commands"}"#);
    let lines = until(client, "the commands answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_cmds"
    });
    lines.last().unwrap()["payload"]["result"].clone()
}

#[test]
fn commands_answers_with_the_workspaces_skills_and_templates() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    workspace_skill(
        &setup,
        "review-pr",
        "description: Reviews a pull request.\n",
    );
    workspace_skill(
        &setup,
        "ship",
        "description: Ships it.\nargument-hint: <tag>\ndisable-model-invocation: true\n",
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    assert_eq!(
        commands_answer(&client),
        json!({"commands": [
            {"name": "review-pr", "description": "Reviews a pull request.", "tag": "skill"},
            {"name": "ship", "description": "Ships it.", "argument_hint": "<tag>",
             "tag": "template"}]})
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn a_resumed_session_answers_commands_from_its_recorded_workspace() {
    let setup = Setup::new();
    let server = ProviderServer::start([stream(&[function_call(
        "call_1",
        "shell",
        &json!({"command": "echo hi"}),
    )])])
    .unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    suspend_on_an_approval(&setup, &id);
    workspace_skill(&setup, "later", "description: Added before the resume.\n");

    let mut running = setup.start_session(&id, &["--resume"]);
    let client = running.connect(&setup.socket(&id));
    assert_eq!(
        commands_answer(&client),
        json!({"commands": [
            {"name": "later", "description": "Added before the resume.", "tag": "skill"}]})
    );
    // Dropping `running` kills the session, still waiting on the approval.
}

/// A 1x1 PNG, 69 bytes: within every cap, so the image child stores it byte
/// for byte (as in `tests/tools.rs`).
const PIXEL: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// [`PIXEL`] as base64.
const PIXEL_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// The acknowledgement or rejection naming `id`.
fn answer(client: &Socket, id: &str) -> Value {
    until(client, "the answer", |line| {
        line["payload"].get("command_id") == Some(&json!(id))
    })
    .into_iter()
    .next_back()
    .unwrap()
}

fn image_prompt(id: &str, text: &str, data: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"command\":\"prompt\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}},{{\"type\":\"image\",\"data\":\"{data}\",\"mime_type\":\"image/png\"}}]}}}}"
    )
}

#[test]
fn a_pasted_image_is_stored_logged_by_path_and_sent_as_an_image_part() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_images(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    recv(&client, "the subscribe acknowledgement");
    send(&client, &image_prompt("c_prompt", "look", PIXEL_BASE64));
    let rest = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let turn_started = rest
        .iter()
        .find(|line| line["kind"] == "turn_started")
        .expect("the prompt started a turn");
    let image = &turn_started["payload"]["input"][0]["content"][1];
    assert_eq!(image["type"], "image");
    let path = image["path"].as_str().unwrap();
    assert!(
        path.starts_with("artifacts/i_") && path.ends_with(".png"),
        "{path}"
    );
    assert_eq!(image["mime_type"], "image/png");
    assert_eq!(
        (image["width"].clone(), image["height"].clone()),
        (json!(1), json!(1))
    );

    let session = setup.session_dir(&id);
    // The artifact is the processed file: here, the input byte for byte.
    assert_eq!(fs::read(session.join(path)).unwrap(), PIXEL);
    // The log names the file and never holds its bytes.
    let log = fs::read(session.join("events.jsonl")).unwrap();
    assert!(
        !String::from_utf8_lossy(&log).contains(PIXEL_BASE64),
        "the log holds the image's base64"
    );
    assert!(
        !log.windows(PIXEL.len()).any(|window| window == PIXEL),
        "the log holds the image's bytes"
    );

    // The first request carries the image as an image part of the text.
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let user = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| {
            item["role"] == "user"
                && item["content"]
                    .as_array()
                    .is_some_and(|content| content.iter().any(|part| part["type"] == "input_image"))
        })
        .expect("the request carries the pasted image");
    assert_eq!(
        user["content"],
        json!([
            {"type": "input_text", "text": "look"},
            {"type": "input_image", "image_url": format!("data:image/png;base64,{PIXEL_BASE64}")},
        ])
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn a_steered_image_is_applied_and_sent_on_the_next_request() {
    let setup = Setup::new();
    // The first reply runs `sleep 5`, so the turn stays open seconds
    // after the prompt: the steer, pasted behind it, is delivered and
    // applied at the step boundary, inside the turn. The reply is held
    // until a `tools` answer proves the steer - image and all - is
    // stored and queued, so the image is in the inbox before the
    // sleeping call's step ends: the test forces the order instead of
    // racing the 5 s sleep. A standing rule allows the command without
    // asking.
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "sleep 5"}),
        )]),
        hello(),
    ])
    .unwrap();
    server.hold();
    setup.provider_with_images(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "sleep 5"})
        ),
    )
    .unwrap();
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    recv(&client, "the subscribe acknowledgement");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"go"}]}}"#,
    );
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the first request is in flight"
    );
    send(
        &client,
        &format!(
            "{{\"id\":\"c_steer\",\"command\":\"steer\",\"args\":{{\"content\":[{{\"type\":\"image\",\"data\":\"{PIXEL_BASE64}\",\"mime_type\":\"image/png\"}}]}}}}"
        ),
    );
    // The turn stays open on the held reply while the reader stores
    // the image and queues the steer. `tools` is answered on the reader
    // thread, in line order, so its answer proves the steer - image and
    // all - was processed and queued before the release lets the reply
    // through: the test forces the order instead of racing the 5 s
    // sleep. (Waiting for the steer's own acceptance here would
    // deadlock: the loop answers it at the next step boundary, which
    // needs the held reply.)
    send(&client, r#"{"id":"c_tools","command":"tools"}"#);
    let _queued = answer(&client, "c_tools");
    server.release();
    let accepted = answer(&client, "c_steer");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");

    let mut saw_applied = false;
    let rest = until(
        &client,
        "the steer applied and the turn completed",
        |line| {
            if line["kind"] == "steering_applied" {
                saw_applied = true;
            }
            line["kind"] == "turn_completed" && saw_applied
        },
    );
    assert!(
        rest.iter().any(|line| line["kind"] == "steering_applied"),
        "the steer was applied: {:?}",
        kinds(&rest)
    );
    let applied = rest
        .iter()
        .find(|line| line["kind"] == "steering_applied")
        .expect("the steer was applied");
    let image = &applied["payload"]["content"][0];
    assert_eq!(image["type"], "image");
    assert!(
        image["path"].as_str().unwrap().starts_with("artifacts/i_"),
        "{image}"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let user = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| {
            item["role"] == "user"
                && item["content"]
                    .as_array()
                    .is_some_and(|content| content.iter().any(|part| part["type"] == "input_image"))
        })
        .expect("the second request carries the steered image");
    assert!(
        user["content"].as_array().unwrap().iter().any(|part| {
            part["type"] == "input_image"
                && part["image_url"] == format!("data:image/png;base64,{PIXEL_BASE64}")
        }),
        "{user}"
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn unreadable_pasted_images_are_rejected_without_a_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_images(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    recv(&client, "the subscribe acknowledgement");
    // Not an image.
    send(
        &client,
        r#"{"id":"c_1","command":"prompt","args":{"content":[{"type":"image","data":"bm90IGFuIGltYWdl","mime_type":"image/png"}]}}"#,
    );
    let first = answer(&client, "c_1");
    assert_eq!(first["kind"], "command_rejected", "{first}");
    assert_eq!(first["payload"]["code"], "invalid_arguments");
    assert!(
        first["payload"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Image 1 cannot be read: "),
        "{first}"
    );
    // Over 50 megapixels: an 8000x7000 header and no pixels.
    send(
        &client,
        r#"{"id":"c_2","command":"prompt","args":{"content":[{"type":"image","data":"iVBORw0KGgoAAAANSUhEUgAAH0AAABtYCAIAAACSWZ5GAAAAA0lEQVR4nAB+3LJc","mime_type":"image/png"}]}}"#,
    );
    let second = answer(&client, "c_2");
    assert_eq!(second["kind"], "command_rejected", "{second}");
    assert_eq!(second["payload"]["code"], "invalid_arguments");
    assert_eq!(
        second["payload"]["message"],
        "Image 1 cannot be read: 8000x7000 is 56000000 pixels; the limit is 50000000"
    );
    // The second image fails: the counter counts image parts only.
    send(
        &client,
        &format!(
            "{{\"id\":\"c_3\",\"command\":\"prompt\",\"args\":{{\"content\":[{{\"type\":\"image\",\"data\":\"{PIXEL_BASE64}\",\"mime_type\":\"image/png\"}},{{\"type\":\"image\",\"data\":\"bm90IGFuIGltYWdl\",\"mime_type\":\"image/png\"}}]}}}}"
        ),
    );
    let third = answer(&client, "c_3");
    assert_eq!(third["kind"], "command_rejected", "{third}");
    assert_eq!(third["payload"]["code"], "invalid_arguments");
    assert!(
        third["payload"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Image 2 cannot be read: "),
        "{third}"
    );
    assert!(server.requests().is_empty(), "no turn ran");
    let events = fs::read(setup.session_dir(&id).join("events.jsonl")).unwrap();
    assert!(
        !String::from_utf8_lossy(&events).contains("turn_started"),
        "no turn started"
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

/// The stdout kinds every `close` test's session opens with: startup, the
/// client's attach, and the prompt's turn starting. `clients` lands after
/// `extensions_loaded` because each test connects after
/// `running.wait_for("extensions_loaded")`, as the existing tests do.
const CLOSE_START: [&str; 7] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "clients",
    "preamble_built",
    "opening_message",
    "turn_started",
];

/// The stdout kinds of the backgrounding step: the model backgrounds a
/// shell call, which the standing rule allows, and the job starts.
const CLOSE_BG_STEP: [&str; 9] = [
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "permission_resolved",
    "tool_call_started",
    "job_started",
    "tool_call_completed",
];

/// The stdout kinds of a `Hello.` reply step and its turn end.
const CLOSE_REPLY: [&str; 8] = [
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

/// The stdout kinds of a turn a `close` with `now` interrupts mid-request:
/// the step starts, the assistant message starts, and the shutdown ends
/// the turn.
const CLOSE_HELD: [&str; 3] = [
    "step_started",
    "assistant_message_started",
    "turn_completed",
];

/// A background job that writes its group id to `<ready>`, then runs
/// forever: `close` with `now` must stop it.
const JOB_SCRIPT_FOREVER: &str = "echo $$ > '<ready>'\nwhile :; do sleep 0.05; done\n";

/// A background job that writes its group id to `<ready>`, then runs until
/// `go` appears in the workspace: `close` without `now` waits for it.
const JOB_SCRIPT_UNTIL_GO: &str = "echo $$ > '<ready>'\nwhile [ ! -e go ]; do sleep 0.05; done\n";

/// The background call every `close` test's first response makes.
fn bg_call() -> Value {
    function_call(
        "call_bg",
        "shell",
        &json!({"command": "sh job.sh", "run_in_background": true}),
    )
}

/// Starts a session whose `c_prompt` backgrounds `script`, and returns the
/// running session, its subscribed client and the background job's group.
/// `script` names the ready FIFO as `<ready>`. Once the prompt is sent,
/// `after_prompt` runs before the job's ready line is awaited: the held
/// mid-turn test releases one held response there, so the job starts while
/// the next request stays held.
fn job_session(
    setup: &Setup,
    script: &str,
    server: &ProviderServer,
    after_prompt: impl FnOnce(),
) -> (Running, Socket, u32) {
    let ready = fakes::children::Ready::new(setup.root.path());
    fs::write(
        setup.workspace().join("job.sh"),
        script.replace("<ready>", &ready.path().display().to_string()),
    )
    .unwrap();
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "sh job.sh"})
        ),
    )
    .unwrap();
    setup.provider(server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"start the job"}]}}"#,
    );
    after_prompt();
    let job_group = ready.wait(setup.deadline.left())[0];
    // The session must stop the job: dropping the watchdog here would kill
    // the group first, so it is forgotten and only fires if the test
    // process dies.
    std::mem::forget(Watchdog::group(job_group));
    (running, client, job_group)
}

/// The session's stdout kinds without `job_delta`: it is ephemeral, and
/// where it lands depends on when the job writes. Like [`kinds`], without
/// `session_status`.
fn stdout_kinds(out: &[Value]) -> Vec<&str> {
    out.iter()
        .filter(|line| line["kind"] != "job_delta" && line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// A `close` with `now`, answered `command_accepted`.
fn send_close_now(client: &Socket) {
    send(
        client,
        r#"{"id":"c_close_now","command":"close","args":{"now":true}}"#,
    );
    let accepted = answer(client, "c_close_now");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
}

fn assert_exited_0(status: ExitStatus, out: &[Value], stderr: &str) -> Value {
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    exited.clone()
}

/// `fiber_exited` carries no `error`, `text` or `suspended_on`.
fn assert_exited_clean(exited: &Value) {
    assert_eq!(exited["payload"].get("error"), None);
    assert_eq!(exited["payload"].get("text"), None);
    assert_eq!(exited["payload"].get("suspended_on"), None);
}

#[test]
fn close_now_mid_turn_stops_the_turn_and_a_background_job_and_exits_0() {
    let setup = Setup::new();
    let server = ProviderServer::start([stream(&[bg_call()]), hello()]).unwrap();
    server.hold();
    let (running, client, job_group) =
        job_session(&setup, JOB_SCRIPT_FOREVER, &server, || server.release_one());
    // The first response is through, the job runs, and the second request
    // is held: the turn is in flight.
    assert!(
        server.await_requests(2, setup.deadline.left()),
        "the held second response was requested"
    );
    send_close_now(&client);
    let _tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    server.release();
    assert_exited_clean(&assert_exited_0(status, &out, &stderr));
    let turn = out
        .iter()
        .find(|line| line["kind"] == "turn_completed")
        .expect("the interrupted turn completed");
    assert_eq!(turn["payload"]["outcome"], "interrupted");
    let job = out
        .iter()
        .find(|line| line["kind"] == "job_completed")
        .expect("the background job completed");
    assert_eq!(job["payload"]["status"], "cancelled");
    assert!(
        !group_alive(setup.deadline, job_group),
        "the job's group outlived the session"
    );
    assert_eq!(server.requests().len(), 2);
    assert_eq!(
        stdout_kinds(&out),
        [
            CLOSE_START.as_slice(),
            CLOSE_BG_STEP.as_slice(),
            CLOSE_HELD.as_slice(),
            &["job_completed", "fiber_exited"],
        ]
        .concat()
    );
}

#[test]
fn close_now_while_idle_with_a_job_starts_no_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([stream(&[bg_call()]), hello()]).unwrap();
    let (running, client, job_group) = job_session(&setup, JOB_SCRIPT_FOREVER, &server, || {});
    // The prompt's turn completed: the session is idle with the job running.
    let _done = until(&client, "the prompt's turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send_close_now(&client);
    let _tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_exited_clean(&assert_exited_0(status, &out, &stderr));
    let job = out
        .iter()
        .find(|line| line["kind"] == "job_completed")
        .expect("the background job completed");
    assert_eq!(job["payload"]["status"], "cancelled");
    assert!(
        !group_alive(setup.deadline, job_group),
        "the job's group outlived the session"
    );
    assert_eq!(server.requests().len(), 2, "no ending-notice request ran");
    assert_eq!(
        stdout_kinds(&out),
        [
            CLOSE_START.as_slice(),
            CLOSE_BG_STEP.as_slice(),
            CLOSE_REPLY.as_slice(),
            &["job_completed", "fiber_exited"],
        ]
        .concat()
    );
}

#[test]
fn close_now_on_a_pending_approval_leaves_it_pending_and_exits_0() {
    let setup = Setup::new();
    let server = ProviderServer::start([stream(&[function_call(
        "call_1",
        "shell",
        &json!({"command": "echo hi"}),
    )])])
    .unwrap();
    setup.provider(&server);
    // The standing ask, as `an_escalation_reaches_a_connected_client...`
    // writes it.
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
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"run it"}]}}"#,
    );
    let asked = until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    });
    let request = asked.last().expect("the escalation was requested");
    let request_id = request["payload"]["request_id"]
        .as_str()
        .expect("the request has an id")
        .to_owned();
    send_close_now(&client);
    let _tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    let exited = assert_exited_0(status, &out, &stderr);
    // This asserts presence, unlike the other tests' clean exit.
    assert_eq!(
        exited["payload"]["suspended_on"].as_str(),
        Some(request_id.as_str())
    );
    assert!(
        out.iter().all(|line| line["kind"] != "permission_resolved"),
        "the shutdown left the request pending"
    );
    assert!(
        out.iter().all(|line| line["kind"] != "tool_call_completed"),
        "the shutdown ran no call"
    );
    assert_eq!(
        stdout_kinds(&out),
        [
            CLOSE_START.as_slice(),
            &[
                "step_started",
                "assistant_message_started",
                "tool_call_requested",
                "usage_recorded",
                "assistant_message_completed",
                "permission_requested",
                "fiber_exited",
            ],
        ]
        .concat()
    );
}

#[test]
fn close_now_while_idle_with_no_jobs_exits_0() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let sub = recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["payload"]["command_id"], "c_sub");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    let _done = until(&client, "the prompt's turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send_close_now(&client);
    let _tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_exited_clean(&assert_exited_0(status, &out, &stderr));
    assert_eq!(server.requests().len(), 1);
    assert_eq!(stdout_kinds(&out), STDOUT_KINDS_ONE_TURN_AND_CLOSE);
}

#[test]
fn close_without_now_waits_for_the_job_to_finish() {
    let setup = Setup::new();
    let server = ProviderServer::start([stream(&[bg_call()]), hello(), hello(), hello()]).unwrap();
    let (mut running, client, job_group) = job_session(&setup, JOB_SCRIPT_UNTIL_GO, &server, || {});
    let _done = until(&client, "the prompt's turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let accepted = answer(&client, "c_close");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    // The ending-notice turn completes, and the session waits for the job.
    let _notice = until(&client, "the notice turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        running.child.try_wait().unwrap().is_none(),
        "the session waits for the job"
    );
    assert!(group_alive(setup.deadline, job_group), "the job still runs");
    fs::write(setup.workspace().join("go"), "").unwrap();
    let _tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_exited_0(status, &out, &stderr);
    let job = out
        .iter()
        .find(|line| line["kind"] == "job_completed")
        .expect("the background job completed");
    assert_eq!(job["payload"]["status"], "completed");
    assert_eq!(
        stdout_kinds(&out),
        [
            CLOSE_START.as_slice(),
            CLOSE_BG_STEP.as_slice(),
            CLOSE_REPLY.as_slice(),
            &[
                "turn_started",
                "step_started",
                "jobs_pending_notified",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
                "turn_started",
                "step_started",
                "job_completed",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
                "fiber_exited",
            ],
        ]
        .concat()
    );
}

#[test]
fn close_now_after_close_stops_the_job_it_was_waiting_for() {
    let setup = Setup::new();
    let server = ProviderServer::start([stream(&[bg_call()]), hello(), hello()]).unwrap();
    let (running, client, job_group) = job_session(&setup, JOB_SCRIPT_UNTIL_GO, &server, || {});
    let _done = until(&client, "the prompt's turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let accepted = answer(&client, "c_close");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    let _notice = until(&client, "the notice turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    send_close_now(&client);
    let _tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_exited_0(status, &out, &stderr);
    let job = out
        .iter()
        .find(|line| line["kind"] == "job_completed")
        .expect("the background job completed");
    assert_eq!(job["payload"]["status"], "cancelled");
    assert!(
        !group_alive(setup.deadline, job_group),
        "the job's group outlived the session"
    );
    assert_eq!(server.requests().len(), 3);
    assert_eq!(
        stdout_kinds(&out),
        [
            CLOSE_START.as_slice(),
            CLOSE_BG_STEP.as_slice(),
            CLOSE_REPLY.as_slice(),
            &[
                "turn_started",
                "step_started",
                "jobs_pending_notified",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
                "job_completed",
                "fiber_exited",
            ],
        ]
        .concat()
    );
}
