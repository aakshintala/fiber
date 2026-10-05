//! A second client on `fiber ask`'s socket (`docs/testing.md`, "Levels").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use fakes::{Client, ProviderServer, Response};
use serde_json::{Value, json};

/// How long one `fiber` run, or one socket line, may take.
const DEADLINE: Duration = Duration::from_secs(20);

struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fa");
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
}

fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
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

fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let group_arg = group.to_string();
    let mut shell = Command::new("sh");
    shell
        .args(["-c", fakes::WATCHDOG_SCRIPT, "watchdog", group_arg.as_str()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut spawned = shell.spawn().unwrap();
    std::mem::forget(guard);
    let stdin = spawned.stdin.take().unwrap();
    (
        child,
        Watchdog {
            stdin: Some(stdin),
            child: Some(spawned),
        },
    )
}

struct Watchdog {
    stdin: Option<std::process::ChildStdin>,
    child: Option<Child>,
}

impl Watchdog {
    fn stand_down(mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            match writeln!(stdin) {
                Ok(()) | Err(_) => {}
            }
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        assert!(
            finished.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for the watchdog to exit"
        );
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        drop(self.stdin.take());
    }
}

struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

fn send(client: &Client, line: &str) {
    client.send(line).unwrap();
}

fn recv(client: &Client) -> Value {
    client.recv(DEADLINE).expect("a line arrived")
}

fn until(client: &Client, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
    let mut lines = Vec::new();
    loop {
        let line = recv(client);
        let stop = done(&line);
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

fn durable(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line.get("seq").is_some())
        .cloned()
        .collect()
}

/// `fiber ask` with its stdout kept drained and its stderr kept for a failure.
struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    guard: KillGroup,
    stdout: mpsc::Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

fn start(setup: &Setup) -> Running {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
    command
        .args(["ask", "hi"])
        .current_dir(setup.workspace())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", setup.home())
        .env("FIBER_TEST_FAKE_KEY", "sk-test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
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
        guard,
        stdout: stdout_rx,
        stderr: stderr_text,
    }
}

fn first_line(stdout: &mpsc::Receiver<String>) -> Value {
    serde_json::from_str(
        &stdout
            .recv_timeout(DEADLINE)
            .expect("waited for fiber_started"),
    )
    .unwrap()
}

fn answered<'a>(lines: &'a [Value], id: &str) -> &'a Value {
    lines
        .iter()
        .find(|line| line["payload"]["command_id"] == id)
        .unwrap_or_else(|| panic!("no answer for {id}"))
}

fn finish(running: Running) {
    let Running {
        mut child,
        watchdog,
        group,
        guard,
        stdout,
        stderr,
    } = running;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = finished
        .recv_timeout(DEADLINE)
        .expect("waited for fiber to exit")
        .unwrap();
    assert!(status.success(), "stderr: {}", stderr.lock().unwrap());
    assert!(!group_alive(group), "fiber left a process in its group");
    // The group is empty. Skip the drop, which would kill it again.
    std::mem::forget(guard);
    drop(stdout);
    watchdog.stand_down();
}

#[test]
fn two_clients_see_the_log_while_ask_is_held() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    let running = start(&setup);
    let started = first_line(&running.stdout);
    assert_eq!(started["kind"], "session_started");
    let session_id = started["session_id"].as_str().unwrap().to_owned();
    assert!(
        server.await_requests(1, DEADLINE),
        "the held response was requested"
    );

    let socket = setup.home().join("run").join(&session_id);
    let first = Client::connect(&socket).unwrap();
    let second = Client::connect(&socket).unwrap();
    send(
        &first,
        r#"{"id":"c_a","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &second,
        r#"{"id":"c_b","command":"subscribe","args":{"level":"full"}}"#,
    );
    assert_eq!(recv(&first)["payload"]["command_id"], "c_a");
    assert_eq!(recv(&second)["payload"]["command_id"], "c_b");
    send(
        &first,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"again"}]}}"#,
    );
    send(
        &first,
        r#"{"id":"c_steer","command":"steer","args":{"content":[{"type":"text","text":"more"}]}}"#,
    );
    // The reader hands a line to the inbox before it reads the next, so the
    // answer to `tools` proves the prompt and the steer are queued. Released
    // earlier, the turn can end and drop them: "The session ended before
    // answering." instead of the loop's answer, or none.
    send(&first, r#"{"id":"c_queued","command":"tools"}"#);
    let mut own = until(&first, |line| line["payload"]["command_id"] == "c_queued");
    server.release();

    own.extend(until(&first, |line| line["kind"] == "fiber_exited"));
    let other = until(&second, |line| line["kind"] == "fiber_exited");
    let prompt = answered(&own, "c_prompt");
    assert_eq!(prompt["kind"], "command_rejected");
    assert_eq!(prompt["payload"]["code"], "closing");
    assert_eq!(
        prompt["payload"]["message"],
        "The session is closing and takes no new turn."
    );
    assert_eq!(answered(&own, "c_steer")["kind"], "command_accepted");
    let leaked: Vec<_> = other
        .iter()
        .filter(|line| line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
        .cloned()
        .collect();
    assert!(
        leaked.is_empty(),
        "the other connection saw an acknowledgement: {leaked:?}"
    );
    finish(running);

    let sessions = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    let file = fs::read_to_string(sessions.join(&session_id).join("events.jsonl")).unwrap();
    let logged: Vec<Value> = file
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        logged.iter().any(|line| line["kind"] == "steering_applied"),
        "steer is applied in the step after the held reply"
    );
    assert_eq!(durable(&own), logged);
    assert_eq!(durable(&other), logged);
    drop(first);
    drop(second);
}
