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

/// How long a socket connect is parked between attempts. The connect loop
/// below tries at most `DEADLINE / RETRY_WAIT` times, so every wait stays
/// under one named deadline with no sleep on the real clock beyond this
/// retry interval.
const RETRY_WAIT: Duration = Duration::from_millis(5);

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

    /// Starts `fiber session --id <id> --workspace <workspace>` with
    /// `extra` appended, in its own process group, its stdout drained on a
    /// thread and its stderr kept for a failure.
    fn start_session(&self, id: &str, extra: &[&str]) -> Running {
        let workspace = self.workspace();
        let mut args = vec![
            "session",
            "--id",
            id,
            "--workspace",
            workspace.to_str().unwrap(),
        ];
        args.extend(extra);
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(&args)
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
        }
    }

    /// Runs `fiber` once with `args`, waiting under [`DEADLINE`].
    fn run(&self, args: &[&str]) -> (Option<i32>, Vec<Value>, String) {
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
        let child = command.spawn().unwrap();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
        let output = child.wait_with_output().unwrap();
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
}

impl Running {
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
        // and every line arrives, each waited under DEADLINE.
        let mut out = Vec::new();
        while let Ok(line) = self.lines.recv_timeout(DEADLINE) {
            out.push(serde_json::from_str(&line).unwrap());
        }
        let stderr = self.stderr.recv_timeout(DEADLINE).unwrap_or_default();
        self.watchdog.stand_down(DEADLINE);
        (status, out, stderr)
    }
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

/// Connects to the session's socket, retrying until it accepts. At most
/// `DEADLINE / RETRY_WAIT` attempts, so the wait stays under one named
/// deadline.
fn connect(socket: &Path) -> Client {
    let (_tx, parked) = mpsc::channel::<()>();
    for _ in 0..(DEADLINE.as_millis() / RETRY_WAIT.as_millis()) {
        if let Ok(client) = Client::connect(socket) {
            return client;
        }
        parked.recv_timeout(RETRY_WAIT).unwrap_or(());
    }
    Client::connect(socket).expect("the session's socket accepted before the deadline")
}

fn send(client: &Client, line: &str) {
    client.send(line).unwrap();
}

fn recv(client: &Client) -> Value {
    client.recv(DEADLINE).expect("a line arrived")
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
    let running = setup.start_session(&id, &[]);

    let client = connect(&setup.socket(&id));
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    assert_eq!(recv(&client)["payload"]["command_id"], "c_sub");
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    let started = until(&client, "turn_started", |line| {
        line["kind"] == "turn_started"
    });
    assert_eq!(
        started.last().unwrap()["payload"]["input"][0]["content"][0]["text"],
        "hi"
    );
    let rest = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        rest.iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the fake model's text arrived: {rest:?}"
    );

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    assert!(!setup.socket(&id).exists());
    // The session was prompted, so its directory remains.
    assert!(setup.session_dir(&id).join("events.jsonl").is_file());
    drop(client);
}

#[test]
fn an_idle_session_exits_with_a_client_still_connected() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    setup.no_idle();
    let id = doors::mint("s_");
    let running = setup.start_session(&id, &["--prompt", "hi"]);

    // The turn is held at the provider, so the client subscribes first.
    let client = connect(&setup.socket(&id));
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    assert_eq!(recv(&client)["payload"]["command_id"], "c_sub");
    assert!(
        server.await_requests(1, DEADLINE),
        "the held response was requested"
    );
    server.release();

    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 0);
    // The process is gone, so the still-connected client reads to EOF.
    while client.recv(DEADLINE).is_some() {}
    drop(client);
}

#[test]
fn a_session_that_never_got_a_prompt_leaves_nothing_behind() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.no_idle();
    let id = doors::mint("s_");
    let running = setup.start_session(&id, &[]);

    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
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
