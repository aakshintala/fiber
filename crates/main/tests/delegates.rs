//! End-to-end tests of Fiber delegates (`docs/delegates.md`): a parent
//! session spawns a child `fiber session` through `delegate_spawn`, and the
//! child's end wakes the parent. The built `fiber` runs in its own process
//! group with its own `FIBER_HOME`. Parent and child use two fake models,
//! `pa/m` and `pb/m`, on two provider servers, so each side's requests read
//! back separately.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::Deadline;

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
        let root = fakes::TempDir::new("fe");
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

    /// Installs provider `name` with model `m` on `protocol` at `url`.
    fn install_url(&self, name: &str, url: &str, protocol: &str) {
        let source = self.root.path().join(format!("src-{name}"));
        write(
            &source.join("extension.json"),
            &json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write(
            &source.join(format!("providers/{name}.json")),
            &json!({
                "name": name,
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": protocol,
                    "base_url": format!("{url}/v1"), "context_window": 100000}]
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
    }

    /// Installs the parent's `pa/m` and the child's `pb/m`, both on
    /// `openai-responses`, and configures the parent's model and reviewer.
    fn providers(&self, parent: &ProviderServer, child: &ProviderServer) {
        self.providers_url(&parent.url(), &child.url(), "openai-responses");
    }

    /// As [`Setup::providers`], for base URLs and a protocol: the dynamic
    /// parent server is not a [`ProviderServer`].
    fn providers_url(&self, parent: &str, child: &str, protocol: &str) {
        self.install_url("pa", parent, protocol);
        self.install_url("pb", child, protocol);
        write(
            &self.home().join("config.json"),
            &json!({"model": "pa/m", "reviewer": {"model": "pa/m"}}),
        );
    }

    /// A standing allow for `delegate_spawn`: `always_reviewed` skips it,
    /// so the reviewer is still asked.
    fn standing_allow(&self) {
        fs::write(
            self.home().join("rules"),
            format!(
                "{}\n",
                json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""})
            ),
        )
        .unwrap();
    }

    /// The session keeps serving instead of idling out: an exit proves the
    /// run ended on its own.
    fn slow_idle(&self) {
        write(
            &self.home().join("config.json"),
            &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
                "session": {"idle_exit_ms": 3600000}}),
        );
    }

    /// The project's sessions directory for `fiber ask`: its workspace is
    /// the launch directory, the test root.
    fn sessions_dir(&self) -> PathBuf {
        Self::sessions_in(&self.home(), self.root.path())
    }

    /// The sessions directory for a session in `workspace`.
    fn sessions_in(home: &Path, workspace: &Path) -> PathBuf {
        let workspace = fs::canonicalize(workspace).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        home.join("projects").join(key).join("sessions")
    }

    /// One `fiber` invocation with `args`: the environment every test runs
    /// under. Stdout and stderr are piped; the caller lends the stdin and
    /// decides how to wait. The group is the child's own, so killing it
    /// kills only this process.
    fn fiber(&self, args: &[&str], stdin: Stdio) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }
}

/// Writes `value` as JSON to `file`, creating its parent directories.
fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, serde_json::to_string(value).unwrap()).unwrap();
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

/// An `openai-responses` stream answering `text`: what a reviewer verdict
/// and a delegate's final message read as.
fn text_reply(text: &str) -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// A `delegate_spawn` call for `model`: the parent's first reply.
fn spawn_call(model: &str) -> Value {
    function_call(
        "call_delegate",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello", "model": model}),
    )
}

/// Reads `path` as one JSON value per line, skipping blank lines. A log
/// that does not exist yet reads as empty, so polls can wait for it.
fn read_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Polls `read` until it returns `Some`, or panics after `within`: every
/// wait in these tests carries its own wall-clock bound, so a failing test
/// fails fast.
#[expect(
    clippy::disallowed_methods,
    reason = "polling a child process's log file on the wall clock; the bound is explicit"
)]
fn poll<T>(what: &str, within: Duration, mut read: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = read() {
            return value;
        }
        if start.elapsed() >= within {
            panic!("waited {within:?} for {what}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// The log's lines once `kind` has been written.
fn poll_log(dir: &Path, kind: &str) -> Vec<Value> {
    poll(
        &format!("{kind} in {}", dir.display()),
        Duration::from_secs(5),
        || {
            let lines = read_lines(&dir.join("events.jsonl"));
            lines
                .iter()
                .any(|line| line["kind"] == kind)
                .then_some(lines)
        },
    )
}

/// The `tool_call_completed` line for the log's one call of `name`.
fn completed_for(lines: &[Value], name: &str) -> Value {
    let requested = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested" && line["payload"]["name"] == name)
        .unwrap_or_else(|| panic!("no {name} call in {:?}", kinds(lines)));
    let action = requested["action_id"].as_str().unwrap();
    lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == action)
        .unwrap()
        .clone()
}

/// The payload of the log's one `kind` line.
fn one(lines: &[Value], kind: &str) -> Value {
    let mut found = lines.iter().filter(|line| line["kind"] == kind);
    let line = found
        .next()
        .unwrap_or_else(|| panic!("no {kind} in {:?}", kinds(lines)));
    assert!(found.next().is_none(), "two {kind} lines");
    line["payload"].clone()
}

/// The log's kinds, for a failure.
fn kinds(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// Runs `fiber` once with `args`, waiting under the test's [`Deadline`].
/// The wait runs on a thread and is received under the deadline, so a hang
/// reports what it waited for. The watchdog kills the group if this process
/// dies first.
fn run_to_exit(setup: &Setup, args: &[&str]) -> (Option<i32>, Vec<Value>, String) {
    let child = setup.fiber(args, Stdio::null()).spawn().unwrap();
    let group = child.id();
    let _guard = KillGroup(group);
    let _watchdog = Watchdog::group(group);
    let output = child.wait_with_output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// Kills the process group on drop, waited out under cleanup time.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        support::kill_group_detached(self.0, "KILL");
    }
}

/// A running `fiber session`: its drained stdout lines, its stderr, and its
/// group, killed on drop.
struct Running {
    child: Child,
    group: u32,
    _guard: KillGroup,
    _watchdog: Watchdog,
    lines: mpsc::Receiver<Value>,
    stderr: mpsc::Receiver<String>,
    deadline: Deadline,
}

impl Running {
    /// Starts `fiber session --id <id>` with `extra` appended, draining
    /// stdout on a thread and keeping stderr for a failure. The watchdog
    /// is armed before spawning, so a failing test leaves no child behind.
    fn start(setup: &Setup, id: &str, extra: &[&str]) -> Self {
        let workspace = setup.workspace();
        let mut args = vec!["session", "--id", id, "--workspace"];
        args.push(workspace.to_str().unwrap());
        args.extend(extra);
        Self::spawn(setup, &args)
    }

    /// Spawns `fiber` with `args`, draining stdout on a thread and keeping
    /// stderr for a failure.
    fn spawn(setup: &Setup, args: &[&str]) -> Self {
        let (running, _) = Self::spawn_stdin(setup, args, Stdio::null());
        running
    }

    /// As [`Running::spawn`], with `stdin`: the caller holds the returned
    /// stdin open, which a delegate reads as its lifeline.
    fn spawn_stdin(
        setup: &Setup,
        args: &[&str],
        stdin: Stdio,
    ) -> (Self, Option<std::process::ChildStdin>) {
        let mut command = setup.fiber(args, stdin);
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                match tx.send(serde_json::from_str(&line.unwrap()).unwrap()) {
                    Ok(()) => {}
                    Err(mpsc::SendError(_)) => break,
                }
            }
        });
        let (err_tx, stderr) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            match std::io::Read::read_to_string(
                &mut std::io::BufReader::new(stderr_pipe),
                &mut text,
            ) {
                Ok(_) | Err(_) => {}
            }
            match err_tx.send(text) {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
        });
        (
            Self {
                child,
                group,
                _guard: guard,
                _watchdog: watchdog,
                lines,
                stderr,
                deadline: setup.deadline,
            },
            stdin,
        )
    }

    /// Connects a client to the session's socket.
    fn connect(&self, setup: &Setup, id: &str) -> Socket {
        Socket::connect(setup.deadline, &setup.home().join("run").join(id))
    }

    /// Waits for the process to exit, bounded by the deadline. Stands
    /// the watchdog down first: dropping it would kill the group. Every
    /// stdout line is read: the drain ends at end of file once the process
    /// is gone, so a disconnect means the output is whole.
    fn wait(self) -> (ExitStatus, Vec<Value>, String) {
        let watchdog = self._watchdog;
        watchdog.stand_down(self.deadline.cleanup());
        let mut out = Vec::new();
        while let Ok(line) = self.lines.try_recv() {
            out.push(line);
        }
        let (done, finished) = mpsc::channel();
        let mut child = self.child;
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match finished.recv_timeout(self.deadline.left()) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited for the session to exit: {out:?}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session wait thread ended without a result")
            }
        };
        while let Ok(line) = self.lines.try_recv() {
            out.push(line);
        }
        loop {
            match self.lines.recv_timeout(self.deadline.left()) {
                Ok(line) => out.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited for the session's last stdout lines: {out:?}")
                }
            }
        }
        let stderr = self.stderr.recv().unwrap_or_default();
        (status, out, stderr)
    }
}

/// A socket client of a session, reading with the test's deadline.
struct Socket {
    write: Mutex<UnixStream>,
    read: Mutex<BufReader<UnixStream>>,
    deadline: Deadline,
}

impl Socket {
    fn connect(deadline: Deadline, path: &Path) -> Self {
        let stream = poll("the session's socket", Duration::from_secs(5), || {
            UnixStream::connect(path).ok()
        });
        let read = stream.try_clone().unwrap();
        Self {
            write: Mutex::new(stream),
            read: Mutex::new(BufReader::new(read)),
            deadline,
        }
    }

    fn send(&self, line: &str) {
        let mut write = self.write.lock().unwrap();
        write.write_all(line.as_bytes()).unwrap();
        write.write_all(b"\n").unwrap();
        write.flush().unwrap();
    }

    /// The next line, once one arrives.
    fn next(&self, what: &str) -> Value {
        let mut read = self.read.lock().unwrap();
        loop {
            let mut line = String::new();
            if let Some(text) = support::read_line(&mut read, self.deadline, what).unwrap() {
                line.push_str(&text);
            }
            if line.is_empty() {
                continue;
            }
            return serde_json::from_str(&line).unwrap();
        }
    }

    /// Every line until the socket closes.
    fn until_close(&self) -> Vec<Value> {
        let mut got = Vec::new();
        loop {
            let mut read = self.read.lock().unwrap();
            match support::read_line(&mut read, self.deadline, "the session to close") {
                Ok(Some(text)) => got.push(serde_json::from_str(&text).unwrap()),
                Ok(None) => return got,
                Err(_) => return got,
            }
        }
    }
}

/// Sends `close` with `now` and reads its accept.
fn close_now(client: &Socket) {
    client.send(r#"{"id":"c_close_now","command":"close","args":{"now":true}}"#);
    loop {
        let line = client.next("the close accept");
        if line["kind"] == "command_accepted" {
            return;
        }
    }
}

/// A fake model server that answers the parent's requests by their content:
/// the first session request gets `first`, every reviewer request gets
/// `reviewer`, the session request carrying the spawn receipt gets
/// `after` built with the receipt's job id, and every later session request
/// gets `rest`. Requests are recorded for the test's assertions.
struct Dynamic {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Dynamic {
    /// Serves on a free port: `first` answers the first session request,
    /// `reviewer` every reviewer request, `after` the request carrying the
    /// spawn receipt (built with its job id), and `rest` every later one.
    /// The `after` answer waits, at most 5 s, for `ready`: the delegate is
    /// only stopped once it is up.
    fn start(
        first: Response,
        reviewer: Response,
        after: impl Fn(&str) -> Response + Send + Sync + 'static,
        rest: Response,
        ready: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let after: Arc<dyn Fn(&str) -> Response + Send + Sync> = Arc::new(after);
        let first = Arc::new(first);
        let reviewer = Arc::new(reviewer);
        let rest = Arc::new(rest);
        thread::spawn({
            let requests = Arc::clone(&requests);
            move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let requests = Arc::clone(&requests);
                    let after = Arc::clone(&after);
                    let first = Arc::clone(&first);
                    let reviewer = Arc::clone(&reviewer);
                    let rest = Arc::clone(&rest);
                    let ready = Arc::clone(&ready);
                    thread::spawn(move || {
                        Self::serve(stream, &requests, &first, &reviewer, &after, &rest, &ready);
                    });
                }
            }
        });
        let _ = addr;
        Self { addr, requests }
    }

    /// The base URL, for a provider definition.
    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one scripted answer: what each request kind gets"
    )]
    fn serve(
        stream: std::net::TcpStream,
        requests: &Mutex<Vec<Request>>,
        first: &Response,
        reviewer: &Response,
        after: &Arc<dyn Fn(&str) -> Response + Send + Sync>,
        rest: &Response,
        ready: &Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (
            parts.next().unwrap_or("").to_owned(),
            parts.next().unwrap_or("").to_owned(),
        );
        let mut length = 0;
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let header = line.trim_end_matches(['\r', '\n']);
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
                && let Ok(parsed) = value.trim().parse()
            {
                length = parsed;
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        let response = {
            let mut requests = requests.lock().unwrap_or_else(PoisonError::into_inner);
            let session = text.contains(r#""name":"delegate_spawn""#);
            let answered = if !session {
                reviewer.clone()
            } else if requests
                .iter()
                .any(|request| String::from_utf8_lossy(&request.body).contains("Started delegate"))
            {
                rest.clone()
            } else if text.contains("Started delegate") {
                // Gated: the call that ends the delegate only goes out once
                // it is up, so the stop meets a running child.
                poll("the delegate to start", Duration::from_secs(5), || {
                    ready().then_some(())
                });
                after(&job_id(&text))
            } else {
                first.clone()
            };
            requests.push(Request {
                method,
                path,
                headers: Vec::new(),
                body_len: body.len(),
                body,
            });
            answered
        };
        let head = format!(
            "HTTP/1.1 {} Fake\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response.status,
            response.body.len()
        );
        writer.write_all(head.as_bytes()).unwrap_or(());
        writer.write_all(&response.body).unwrap_or(());
        writer.flush().unwrap_or(());
    }

    /// Every request received so far, in arrival order.
    fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The session requests' bodies, in order: reviewer requests carry no
    /// tools, so they never match.
    fn session_bodies(&self) -> Vec<Vec<u8>> {
        self.requests()
            .into_iter()
            .filter(|request| {
                String::from_utf8_lossy(&request.body).contains(r#""name":"delegate_spawn""#)
            })
            .map(|request| request.body)
            .collect()
    }
}

/// True once two sessions stepped: the parent's and the child's. The
/// child's first step starts its stalled model call, so the stop meets a
/// running child. Only path data is captured, so it shares across threads.
fn started(setup: &Setup) -> Arc<dyn Fn() -> bool + Send + Sync> {
    started_in(setup.sessions_dir())
}

/// As [`started`], for a sessions directory: `fiber session` runs in its
/// `--workspace`, not the test root.
fn started_in(sessions: PathBuf) -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(move || {
        let Ok(entries) = std::fs::read_dir(&sessions) else {
            return false;
        };
        entries
            .flatten()
            .filter(|entry| {
                read_lines(&entry.path().join("events.jsonl"))
                    .iter()
                    .any(|line| line["kind"] == "step_started")
            })
            .take(2)
            .count()
            == 2
    })
}

/// The job id the spawn receipt names: `Started delegate <id>.`.
fn job_id(text: &str) -> String {
    let marker = "Started delegate ";
    let start = text.find(marker).unwrap() + marker.len();
    text[start..]
        .split([' ', '.', '"', '\\'])
        .next()
        .unwrap()
        .to_owned()
}

/// The parent's log lines once `fiber ask` exits.
struct Ask {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl Ask {
    fn session_id(&self) -> &str {
        self.lines[0]["session_id"].as_str().unwrap()
    }
}

#[test]
fn a_delegate_runs_to_its_end_and_its_finish_wakes_the_parent() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    let full = "x".repeat(20 * 1024);
    let parent = ProviderServer::start([
        stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        hello(),
        hello(),
        hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([text_reply(&full)]).unwrap();
    // Held until the parent's ending-notice turn is in flight: the
    // delegate then ends after it, so the wake turn carries the finish
    // alone, whatever the scheduling.
    child.hold();
    setup.providers(&parent, &child);
    setup.standing_allow();
    setup.slow_idle();

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    poll(
        "the parent's ending-notice request",
        Duration::from_secs(5),
        || (parent.requests().len() >= 4).then_some(()),
    );
    child.release();
    let (status, lines, stderr) = running.wait();
    let ask = Ask {
        code: status.code(),
        lines,
        stderr,
    };

    assert_eq!(ask.code, Some(0), "stderr: {}", ask.stderr);
    // The reviewer was asked, despite the standing allow: the call, the
    // verdict, the receipt's turn, the ending notice and the wake.
    assert_eq!(parent.requests().len(), 5);
    assert_eq!(child.requests().len(), 1);
    let resolved = one(&ask.lines, "permission_resolved");
    assert_eq!(resolved["decision"], "allow");
    assert_eq!(resolved["decided_by"], "reviewer");
    // The receipt arrives in the same turn as the call.
    let completed = ask
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    let receipt = completed["payload"]["content"][0]["text"].as_str().unwrap();
    // One job, one delegate: the log holds each exactly once, in order.
    let kinds = kinds(&ask.lines);
    let at = |kind: &str| kinds.iter().position(|got| got == kind).unwrap();
    assert!(at("job_started") < at("delegate_started"));
    assert!(at("delegate_started") < at("delegate_finished"));
    assert!(at("delegate_finished") < at("job_completed"));
    let started = one(&ask.lines, "job_started");
    let job_id = started["job_id"].as_str().unwrap();
    assert!(receipt.contains(job_id), "{receipt}");
    assert!(
        receipt.contains("events.jsonl"),
        "the receipt names the delegate's log: {receipt}"
    );
    let delegate = one(&ask.lines, "delegate_started");
    assert_eq!(delegate["job_id"], job_id);
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    // The wake turn's input holds one jobs item naming the job, and no
    // message item. The prompt's turn and the ending notice come first.
    let turns: Vec<_> = ask
        .lines
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .collect();
    assert_eq!(turns.len(), 3);
    assert_eq!(
        turns[2]["payload"]["input"],
        json!([{"type": "jobs", "job_ids": [job_id]}])
    );
    // The child's session names its parent.
    let child_dir = setup.sessions_dir().join(child_id);
    let child_lines = read_lines(&child_dir.join("events.jsonl"));
    let child_started = one(&child_lines, "session_started");
    assert_eq!(
        child_started["parent"],
        json!({"session_id": ask.session_id(), "delegate_id": job_id})
    );
    // The 20 KiB final message is cut to 16 KiB in the wake, with the full
    // text in the artifact the cut names.
    let finished = one(&ask.lines, "delegate_finished");
    assert_eq!(finished["job_id"], job_id);
    let text = finished["text"].as_str().unwrap();
    assert!(text.starts_with(&"x".repeat(16 * 1024)), "cut at 16 KiB");
    assert!(text.contains("4096 bytes cut"), "{text}");
    let artifact = finished["artifact"].as_str().unwrap();
    assert_eq!(
        fs::read(setup.sessions_dir().join(ask.session_id()).join(artifact)).unwrap(),
        full.as_bytes()
    );
    let done = one(&ask.lines, "job_completed");
    assert_eq!(done["job_id"], job_id);
    assert_eq!(done["status"], "completed");
    let requests = parent.requests();
    let carrying = requests
        .iter()
        .filter(|request| String::from_utf8_lossy(&request.body).contains("4096 bytes cut"))
        .count();
    assert_eq!(carrying, 1, "one later request carries the cut text");
}

#[test]
fn a_jobs_wait_returns_the_delegate_final_message_with_no_notice_after() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    let full = "the scan found three caches";
    let child = ProviderServer::start([text_reply(full)]).unwrap();
    // The wait names the job the receipt minted, which the script cannot
    // know: the server builds the wait call from the receipt it sees.
    let dynamic = Dynamic::start(
        stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        |job| {
            stream(&[function_call(
                "call_wait",
                "jobs",
                &json!({"action": "wait", "job_id": job, "timeout_ms": 30000}),
            )])
        },
        hello(),
        Arc::new(|| true),
    );
    setup.providers_url(&dynamic.url(), &child.url(), "openai-responses");
    setup.standing_allow();
    setup.slow_idle();

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    let started = one(&lines, "job_started");
    let job_id = started["job_id"].as_str().unwrap().to_owned();
    // The wait ran with the minted id, and returned the final message.
    let bodies = dynamic.session_bodies();
    assert_eq!(bodies.len(), 3);
    // The wait call goes out on the request after the receipt: its history
    // replays the spawn call first, so the jobs call is the last one.
    let waiting: Value = serde_json::from_slice(&bodies[2]).unwrap();
    let calls: Vec<_> = waiting["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .collect();
    let call = calls.last().unwrap();
    assert_eq!(call["name"], "jobs");
    let arguments: Value = serde_json::from_str(call["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(arguments["action"], "wait");
    assert_eq!(arguments["job_id"], job_id);
    let waited = completed_for(&lines, "jobs");
    assert_eq!(waited["payload"]["status"], "completed");
    let text = waited["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains(full), "{text}");
    // Claimed once, under the wait: neither line arrived as a notice.
    let action = waited["action_id"].as_str().unwrap();
    for kind in ["delegate_finished", "job_completed"] {
        let line = lines.iter().find(|line| line["kind"] == kind).unwrap();
        assert_eq!(line["payload"]["job_id"], job_id);
        assert_eq!(line["action_id"], action, "{kind} came as a notice");
    }
}

#[test]
fn a_jobs_stop_cancels_the_delegate_which_exits_143() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    // The child stalls mid-reply, so it is still running when the stop
    // runs: the stop, not the end, finishes it.
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    let child = ProviderServer::start([Response::stall(200, prefix, prefix.len() + 100000)
        .header("content-type", "text/event-stream")])
    .unwrap();
    let dynamic = Dynamic::start(
        stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        |job| {
            stream(&[function_call(
                "call_stop",
                "jobs",
                &json!({"action": "stop", "job_id": job}),
            )])
        },
        hello(),
        started(&setup),
    );
    setup.providers_url(&dynamic.url(), &child.url(), "openai-responses");
    setup.standing_allow();
    setup.slow_idle();

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    let started = one(&lines, "job_started");
    let job_id = started["job_id"].as_str().unwrap();
    let stopped = completed_for(&lines, "jobs");
    assert_eq!(stopped["payload"]["status"], "completed");
    // The stop call reports the cancellation in its text: the job, not the
    // call, ends cancelled.
    let report = stopped["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(
        report.contains(&format!("Job {job_id} cancelled")),
        "{report}"
    );
    // The stop claims the end: the lines carry its action.
    let action = stopped["action_id"].as_str().unwrap();
    for kind in ["delegate_finished", "job_completed"] {
        let line = lines.iter().find(|line| line["kind"] == kind).unwrap();
        assert_eq!(line["payload"]["job_id"], job_id);
        assert_eq!(line["action_id"], action, "{kind} came as a notice");
    }
    let done = one(&lines, "job_completed");
    assert_eq!(done["status"], "cancelled");
    // The child shut down on SIGTERM.
    let delegate = one(&lines, "delegate_started");
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let child_dir = setup.sessions_dir().join(child_id);
    let child_lines = poll_log(&child_dir, "fiber_exited");
    let exited = child_lines.last().unwrap();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 143);
}

/// A child that stalls mid-reply: the parent's stop, close or budget end is
/// what finishes it.
fn stall() -> Response {
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    Response::stall(200, prefix, prefix.len() + 100000).header("content-type", "text/event-stream")
}

/// Subscribes `client` full, for the session's later lines.
fn subscribe(client: &Socket) {
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    loop {
        let line = client.next("the subscribe acknowledgement");
        if line["kind"] == "command_accepted" {
            assert_eq!(line["payload"]["command_id"], "c_sub");
            return;
        }
    }
}

#[test]
fn killing_the_parent_shuts_the_stalled_child_down_with_129() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    let parent = ProviderServer::start([
        stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([stall()]).unwrap();
    setup.providers(&parent, &child);
    setup.standing_allow();
    setup.slow_idle();

    let id = doors::mint("s_");
    let running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    // The delegate runs its turn: its log names the run, and the child
    // stalled mid-reply.
    let dir = Setup::sessions_in(&setup.home(), &setup.workspace()).join(&id);
    let lines = poll_log(&dir, "delegate_started");
    let delegate = one(&lines, "delegate_started");
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let ready = started_in(Setup::sessions_in(&setup.home(), &setup.workspace()));
    poll("the child to stall", Duration::from_secs(5), || {
        ready().then_some(())
    });
    // The parent is gone however it died: SIGKILL to its group, which the
    // child left when it became its own leader.
    support::kill_group(setup.deadline, running.group, "KILL").unwrap();
    let (status, _, _) = running.wait();
    assert_eq!(status.code(), None);
    // End of file on the lifeline is a hangup: the child shuts down with
    // 129 within the bound.
    let child_lines = poll_log(
        &Setup::sessions_in(&setup.home(), &setup.workspace()).join(child_id),
        "fiber_exited",
    );
    let exited = child_lines.last().unwrap();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 129);
}

#[test]
fn a_close_with_now_stops_the_delegate_and_exits_0() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    let parent = ProviderServer::start([
        stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([stall()]).unwrap();
    setup.providers(&parent, &child);
    setup.standing_allow();
    setup.slow_idle();

    let id = doors::mint("s_");
    let running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    let dir = Setup::sessions_in(&setup.home(), &setup.workspace()).join(&id);
    let lines = poll_log(&dir, "delegate_started");
    let delegate = one(&lines, "delegate_started");
    let job_id = delegate["job_id"].as_str().unwrap().to_owned();
    let child_id = delegate["delegate_session_id"].as_str().unwrap().to_owned();
    let ready = started_in(Setup::sessions_in(&setup.home(), &setup.workspace()));
    poll("the child to stall", Duration::from_secs(5), || {
        ready().then_some(())
    });
    let client = running.connect(&setup, &id);
    subscribe(&client);
    close_now(&client);
    let _tail = client.until_close();
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    // The shutdown stops the delegate as a stop does: its finish, then its
    // cancelled end, each once.
    let kinds = kinds(&out);
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| *kind == "delegate_finished")
            .count(),
        1
    );
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "job_completed").count(),
        1
    );
    let finished = one(&out, "delegate_finished");
    assert_eq!(finished["job_id"], job_id);
    let done = one(&out, "job_completed");
    assert_eq!(done["job_id"], job_id);
    assert_eq!(done["status"], "cancelled");
    // The child shut down on SIGTERM.
    let child_lines = poll_log(
        &Setup::sessions_in(&setup.home(), &setup.workspace()).join(&child_id),
        "fiber_exited",
    );
    let exited = child_lines.last().unwrap();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 143);
}

/// An `openai-completions` chunk calling `name` with `arguments`.
fn ccall(name: &str, arguments: &Value) -> Value {
    json!({"id": "resp_1", "object": "chat.completion.chunk", "choices": [{
        "index": 0,
        "delta": {"role": "assistant", "tool_calls": [{
            "id": "call_1", "type": "function",
            "function": {"name": name, "arguments": arguments.to_string()}}]},
        "finish_reason": "tool_calls"}]})
}

/// An `openai-completions` chunk answering `text`.
fn ctext(text: &str) -> Value {
    json!({"id": "resp_1", "object": "chat.completion.chunk", "choices": [{
        "index": 0, "delta": {"role": "assistant", "content": text},
        "finish_reason": "stop"}]})
}

/// An `openai-completions` usage chunk reporting `cost`: the vendor's own
/// figure for the reply.
fn cusage(cost: f64) -> Value {
    json!({"id": "resp_1", "choices": [], "usage": {"prompt_tokens": 15,
        "completion_tokens": 9, "total_tokens": 24, "cost": cost,
        "prompt_tokens_details": {"cached_tokens": 14}}})
}

/// An `openai-completions` stream of `chunks`, then done.
fn cstream(chunks: &[Value]) -> Response {
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    Response::stream(body)
}

#[test]
fn a_turn_that_hits_the_budget_stops_its_running_delegate() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    // The first reply's own figure costs nothing, so the reviewer still
    // sends and allows; the read's reply costs past the budget, so the send
    // after its result ends the turn. The read is reads-only, so no second
    // verdict is asked.
    fs::write(setup.workspace().join("note.txt"), "the note\n").unwrap();
    let parent = ProviderServer::start([
        cstream(&[
            ccall(
                "delegate_spawn",
                &json!({"description": "scan", "prompt": "say hello", "model": "fiber:pb/m"}),
            ),
            cusage(0.0),
        ]),
        cstream(&[ctext("allow"), cusage(0.0)]),
        cstream(&[ccall("read", &json!({"path": "note.txt"})), cusage(100.0)]),
        // The delegate's end wakes one more turn, unless the close below
        // lands first; an unused reply is harmless.
        cstream(&[ctext("done"), cusage(0.0)]),
    ])
    .unwrap();
    let child = ProviderServer::start([stall()]).unwrap();
    setup.providers_url(&parent.url(), &child.url(), "openai-completions");
    setup.standing_allow();
    write(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "budget": {"usd": 1.0}, "session": {"idle_exit_ms": 3600000}}),
    );

    // Held until the child stalls: the budget then ends the turn on a
    // running delegate, whatever the scheduling.
    parent.hold();
    let id = doors::mint("s_");
    let running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    assert!(
        parent.await_requests(1, Duration::from_secs(5)),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, Duration::from_secs(5)),
        "the verdict was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(3, Duration::from_secs(5)),
        "the read was requested"
    );
    assert!(
        child.await_requests(1, Duration::from_secs(5)),
        "the child stalled its model call"
    );
    parent.release();
    let dir = Setup::sessions_in(&setup.home(), &setup.workspace()).join(&id);
    // The turn spent past the budget on its second reply, and ends failed.
    let lines = poll_log(&dir, "turn_completed");
    // The first turn is the budget end; the delegate's end may wake one
    // more while the session serves, so later turns are not pinned.
    let completed = lines
        .iter()
        .find(|line| line["kind"] == "turn_completed")
        .unwrap();
    assert_eq!(completed["payload"]["outcome"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "budget_exceeded");
    // The delegate was running when the turn ended: with the session still
    // serving and no close sent, its finish and cancelled end follow.
    let lines = poll_log(&dir, "job_completed");
    let delegate = one(&lines, "delegate_started");
    let job_id = delegate["job_id"].as_str().unwrap();
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let finished = one(&lines, "delegate_finished");
    assert_eq!(finished["job_id"], job_id);
    let done = one(&lines, "job_completed");
    assert_eq!(done["job_id"], job_id);
    assert_eq!(done["status"], "cancelled");
    // The budget end stops the delegate as a stop does.
    let child_dir = Setup::sessions_in(&setup.home(), &setup.workspace()).join(child_id);
    let child_lines = poll_log(&child_dir, "fiber_exited");
    let exited = child_lines.last().unwrap();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 143);
    let client = running.connect(&setup, &id);
    subscribe(&client);
    close_now(&client);
    let _tail = client.until_close();
    drop(client);
    let (status, _, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
}

/// Declares the MCP server `db` in the repository file of `workspace`: a
/// server nobody approved.
fn declare_db(workspace: &Path, extra: &Value) {
    let mut entry = json!({"command": "/bin/echo"});
    for (key, value) in extra.as_object().unwrap() {
        entry[key] = value.clone();
    }
    write(
        &workspace.join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": entry}}}),
    );
}

#[test]
fn a_delegate_skips_repository_code_nobody_approved() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    let parent = ProviderServer::start([
        stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        hello(),
        hello(),
        hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([hello(), hello()]).unwrap();
    child.hold();
    setup.providers(&parent, &child);
    setup.standing_allow();
    setup.slow_idle();
    // The workspace declares a server nobody approved: `fiber ask` runs
    // with the test root as its workspace.
    declare_db(setup.root.path(), &json!({}));

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    poll(
        "the parent's ending-notice request",
        Duration::from_secs(5),
        || (parent.requests().len() >= 4).then_some(()),
    );
    child.release();
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    let done = one(&lines, "job_completed");
    assert_eq!(done["status"], "completed");
    // The delegate raised no offer and never loaded the server: its
    // preamble declares no tool of `db`.
    let delegate = one(&lines, "delegate_started");
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let child_lines = read_lines(&setup.sessions_dir().join(child_id).join("events.jsonl"));
    assert!(
        child_lines
            .iter()
            .all(|line| line["kind"] != "repository_code_offered"),
        "an offer was raised"
    );
    let preamble = one(&child_lines, "preamble_built");
    let tools: Vec<_> = preamble["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(
        tools.iter().all(|name| !name.starts_with("mcp__db__")),
        "the unapproved server loaded: {tools:?}"
    );
    // The skip itself is ephemeral, as every notice is: it goes to the
    // delegate's stdout, which its parent drains. The same delegate mode
    // run directly shows it.
    let direct = doors::mint("s_");
    let job = format!("j_{}", "0".repeat(16));
    let parent_id = lines[0]["session_id"].as_str().unwrap();
    // stdin held open: it is the delegate's lifeline.
    let (direct_running, direct_stdin) = Running::spawn_stdin(
        &setup,
        &[
            "session",
            "--id",
            &direct,
            "--workspace",
            setup.root.path().to_str().unwrap(),
            "--model",
            "pb/m",
            "--prompt",
            "hi",
            "--parent",
            parent_id,
            "--delegate-id",
            &job,
        ],
        Stdio::piped(),
    );
    let direct_dir = setup.sessions_dir().join(&direct);
    // Its run ended on its own: one turn and out, with no idle wait. The
    // exit line is written before the process goes, so the code is decided
    // before stdin drops.
    poll_log(&direct_dir, "fiber_exited");
    drop(direct_stdin);
    let (direct_status, direct_lines, direct_stderr) = direct_running.wait();
    // Its run ended on its own: one turn and out, with no idle wait.
    assert_eq!(direct_status.code(), Some(0), "stderr: {direct_stderr}");
    let skipped = direct_lines
        .iter()
        .find(|line| {
            line["kind"] == "notice" && line["payload"]["code"] == "repository_code_skipped"
        })
        .unwrap();
    assert!(
        skipped["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("`db`"),
        "{skipped}"
    );
    assert!(
        direct_lines
            .iter()
            .all(|line| line["kind"] != "repository_code_offered"),
        "an offer was raised"
    );
}

#[test]
fn a_delegate_fails_on_a_required_server_nobody_approved() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    let parent = ProviderServer::start([
        stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        hello(),
        hello(),
        hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([hello()]).unwrap();
    // Held while the repository file is written: the parent started
    // without it, and the child starts with it required.
    parent.hold();
    setup.providers(&parent, &child);
    setup.standing_allow();
    setup.slow_idle();

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    assert!(
        parent.await_requests(1, Duration::from_secs(5)),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, Duration::from_secs(5)),
        "the verdict was requested"
    );
    declare_db(setup.root.path(), &json!({"required": true}));
    parent.release();
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    // The child failed before any session line: the job ends failed with
    // the server's error, and its finish carries no text.
    let done = one(&lines, "job_completed");
    assert_eq!(done["status"], "failed");
    assert_eq!(done["error"]["code"], "mcp_server_unapproved");
    let finished = one(&lines, "delegate_finished");
    assert_eq!(finished["text"], "");
}

#[test]
fn a_delegate_spawn_with_an_unknown_model_fails_and_starts_nothing() {
    let setup = Setup::new();
    // Every child holds the root on its command line: the watchdog kills
    // what is left if the test or this process dies first. Armed before
    // the first fallible step.
    let _watchdog = Watchdog::matching(&setup.root.path().to_string_lossy());
    let parent = ProviderServer::start([
        stream(&[spawn_call("fiber:fake/none")]),
        text_reply("allow"),
        hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([hello()]).unwrap();
    setup.providers(&parent, &child);
    setup.standing_allow();
    setup.slow_idle();

    let (code, lines, stderr) = run_to_exit(&setup, &["ask", "scan the tree"]);
    assert_eq!(code, Some(0), "stderr: {stderr}");

    let failed = completed_for(&lines, "delegate_spawn");
    let error = failed["payload"]["error"].as_object().unwrap();
    assert_eq!(error["code"], "invalid_arguments");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("fiber:pa/m"), "{message}");
    assert!(message.contains("fiber:pb/m"), "{message}");
    // No child started: no delegate line, and the parent's is the only
    // session directory.
    assert!(
        lines.iter().all(|line| line["kind"] != "delegate_started"),
        "a delegate started"
    );
    let entries: Vec<_> = std::fs::read_dir(setup.sessions_dir())
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(child.requests().is_empty());
}
