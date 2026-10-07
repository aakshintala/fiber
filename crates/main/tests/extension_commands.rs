//! Binary-level tests of extension commands through a live session
//! (`docs/extensions.md`, "Commands and screens"; `docs/invocation.md`,
//! "What each command does"): the built `fiber` runs in its own process
//! group with its own `FIBER_HOME`, holding an ordinary provider and the
//! extensions under test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpListener;
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

    /// Installs `fiber.test/<short>`, whose entry script is `init`, with
    /// `manifest` merged into its `extension.json`.
    fn lua_with(&self, short: &str, init: &str, manifest: Value) {
        let source = self.root.path().join("src").join(short);
        fs::create_dir_all(&source).unwrap();
        let mut base = json!({"name": format!("fiber.test/{short}"), "version": "v2.0.0", "fiber": "0.1.0", "api": 1});
        for (key, value) in manifest.as_object().unwrap() {
            base[key] = value.clone();
        }
        write_json(&source.join("extension.json"), &base);
        fs::write(source.join("init.lua"), init).unwrap();
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn lua(&self, short: &str, init: &str) {
        self.lua_with(short, init, json!({}));
    }

    fn socket(&self, id: &str) -> PathBuf {
        self.home().join("run").join(id)
    }

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

fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    guard: KillGroup,
    lines: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
    first: Vec<String>,
}

impl Running {
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

    fn wait(self) -> (ExitStatus, Vec<Value>, String) {
        let (status, raw, stderr) = self.wait_raw();
        let out = raw
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (status, out, stderr)
    }

    fn wait_raw(mut self) -> (ExitStatus, Vec<String>, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match finished.recv_timeout(DEADLINE) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited {DEADLINE:?} for the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session wait thread ended before it exited")
            }
        };
        let mut lines = std::mem::take(&mut self.first);
        while let Ok(line) = self.lines.try_recv() {
            lines.push(line);
        }
        let stderr = self.stderr.recv_timeout(DEADLINE).unwrap_or_default();
        assert!(
            !group_alive(self.group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(self.guard);
        self.watchdog.stand_down(DEADLINE);
        (status, lines, stderr)
    }
}

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

fn until_close(client: &Socket) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.next("the session to close the socket", &lines) {
        lines.push(line);
    }
    lines
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
    Response::stream(body.into_bytes())
}

fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// Holds one HTTP connection open (head read, body never sent), so a
/// `host.http` against it stays parked.
fn hold_server() -> (String, mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut sock = listener.accept().unwrap().0;
        let mut buf = [0; 1];
        let mut seen = Vec::new();
        loop {
            use std::io::Read;
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => seen.push(buf[0]),
            }
            if seen.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let _ = accepted_tx.send(()).ok();
        let (_block_tx, block_rx) = mpsc::channel::<()>();
        let _ = block_rx.recv_timeout(Duration::from_secs(25)).ok();
    });
    (url, accepted_rx)
}

const SYNC: &str = "fiber.command(\"sync-now\", { timeout = 8000, description = \"Sync now.\", run = function(text) host.status(\"synced \" .. text) end })\n";

#[test]
fn commands_lists_the_extensions_command_with_tag_and_description() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua("worker", SYNC);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(&client, r#"{"id":"c_cmds","command":"commands"}"#);
    let lines = until(&client, "the commands answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_cmds"
    });
    let result = lines.last().unwrap()["payload"]["result"].clone();
    let commands = result["commands"].as_array().unwrap();
    assert!(
        commands.iter().any(|c| c["name"] == "sync-now"
            && c["description"] == "Sync now."
            && c["tag"] == "fiber.test/worker"
            && c.get("argument_hint").is_none()),
        "{result}"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn command_runs_idle_reports_status_and_duplicates_reject() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua("worker", SYNC);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"sync-now","text":"3/10"}}"#,
    );
    let accepted = until(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    assert!(accepted.last().unwrap()["payload"].get("result").is_none());
    // Its `host.status` reaches the client as `extension_ui`...
    let ui = until(&client, "extension_ui", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(ui.last().unwrap()["payload"]["status"], "synced 3/10");
    // ...resending the accepted id gets `duplicate_command`...
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"sync-now","text":"3/10"}}"#,
    );
    let duplicate = until(&client, "duplicate_command", |line| {
        line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_1"
    });
    assert_eq!(
        duplicate.last().unwrap()["payload"]["code"],
        "duplicate_command"
    );
    // ...and an unknown name gets `unknown_command`.
    send(
        &client,
        r#"{"id":"c_2","command":"command","args":{"name":"nope"}}"#,
    );
    let unknown = until(&client, "unknown_command", |line| {
        line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_2"
    });
    assert_eq!(
        unknown.last().unwrap()["payload"]["code"],
        "unknown_command"
    );
    // A client attaching afterwards receives the latest status.
    let late = running.connect(&setup.socket(&id));
    send(
        &late,
        r#"{"id":"c_late","command":"subscribe","args":{"level":"full"}}"#,
    );
    let seeded = until(&late, "the seeded status", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(seeded.last().unwrap()["payload"]["status"], "synced 3/10");
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    drop(late);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn command_runs_during_a_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.lua("worker", SYNC);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    // A command is allowed during a turn: admitted at once, in session order.
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"sync-now","text":"turn"}}"#,
    );
    // Admitted at once even while the turn runs: the last line before this
    // returns is the command's own acceptance.
    let accepted = until(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    assert_eq!(
        accepted.last().unwrap()["payload"]["command_id"],
        "c_1",
        "{accepted:?}"
    );
    let ui = until(&client, "extension_ui", |line| {
        line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/worker"
    });
    assert_eq!(ui.last().unwrap()["payload"]["status"], "synced turn");
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn replacing_a_builtin_without_replaces_unloads_with_extension_failed() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    setup.lua_with(
        "worker",
        "fiber.command(\"model\", { timeout = 8000, run = function() end })\n",
        json!({}),
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    // The load notice names the command; it is written at start, so read
    // stdout until `extensions_loaded` and assert it is among those lines.
    // The load notices are written after `extensions_loaded`; wait for both.
    let mut started = Vec::new();
    loop {
        let line = match running.lines.recv_timeout(DEADLINE) {
            Ok(line) => line,
            Err(_) => panic!("waited for extensions_loaded and its notices on stdout"),
        };
        let value: Value = serde_json::from_str(&line).unwrap();
        started.push(value);
        let loaded = started
            .iter()
            .any(|v: &Value| v["kind"] == "extensions_loaded");
        let noticed = started
            .iter()
            .any(|v: &Value| v["kind"] == "notice" && v["payload"]["code"] == "extension_failed");
        if loaded && noticed {
            break;
        }
    }
    assert!(
        started.iter().any(|line| line["kind"] == "notice"
            && line["payload"]["code"] == "extension_failed"
            && line["payload"]["message"]
                .as_str()
                .unwrap()
                .contains("`model`")),
        "{started:?}"
    );
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(&client, r#"{"id":"c_cmds","command":"commands"}"#);
    let lines = until(&client, "the commands answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_cmds"
    });
    let result = lines.last().unwrap()["payload"]["result"].clone();
    assert!(
        !result["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "model" && c["tag"] == "fiber.test/worker"),
        "{result}"
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn a_command_parked_past_close_writes_no_line_after_fiber_exited() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let (url, _accepted) = hold_server();
    setup.lua(
        "worker",
        &format!(
            "fiber.command(\"slow\", {{ timeout = 20000, run = function() local r = host.http({{ url = \"{url}\" }}); host.status(\"late\"); return r.body end }})\n"
        ),
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    send(
        &client,
        r#"{"id":"c_1","command":"command","args":{"name":"slow"}}"#,
    );
    until(&client, "command_accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let tail = until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    let kinds: Vec<&str> = tail
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let exited = kinds
        .iter()
        .position(|k| *k == "fiber_exited")
        .expect("fiber_exited");
    assert!(
        !tail[exited..]
            .iter()
            .any(|line| line["kind"] == "extension_ui"),
        "no line follows fiber_exited: {kinds:?}"
    );
    assert!(
        !out.iter()
            .any(|line| { line["kind"] == "extension_ui" && line["payload"]["status"] == "late" }),
        "the sealed status never reaches stdout"
    );
}
