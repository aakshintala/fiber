//! Helpers the binary-level hub tests share: Fiber home and a fake
//! provider, `fiber` invocations in their own process group, the hub
//! process, socket clients with deadlines, and the guard that kills every
//! session a test started.

#![allow(
    dead_code,
    reason = "each test binary uses its own subset of the helpers"
)]
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

/// How long one `fiber` run, one socket line, or one hub exit may take.
pub(crate) const DEADLINE: Duration = Duration::from_secs(20);

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
pub(crate) struct Setup {
    pub(crate) root: fakes::TempDir,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let root = fakes::TempDir::new("fh");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
    pub(crate) fn provider(&self, server: &ProviderServer) {
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
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
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

    /// One `fiber` invocation with `args`: the environment every test
    /// runs under. Stdio is piped; the caller decides how to wait.
    pub(crate) fn fiber(&self, args: &[&str]) -> Command {
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

    pub(crate) fn hub_socket(&self) -> PathBuf {
        self.home().join("run").join("hub")
    }

    pub(crate) fn session_socket(&self, id: &str) -> PathBuf {
        self.home().join("run").join(id)
    }

    pub(crate) fn hub_log(&self) -> String {
        fs::read_to_string(self.home().join("logs").join("hub.log")).unwrap_or_default()
    }
}

pub(crate) fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Whether any process remains in process group `group`.
pub(crate) fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

/// The process clock behind `contract::clock::Clock`.
pub(crate) struct SystemClock;

impl contract::clock::Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::now"
    )]
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::wall"
    )]
    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::sleep"
    )]
    fn sleep(&self, d: Duration) {
        thread::sleep(d);
    }

    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        let bound = until.map(|until| until.saturating_duration_since(self.now()));
        wait(bound);
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// A hub the test started: killed on drop unless forgotten after a clean wait.
pub(crate) struct HubProc {
    pub(crate) child: Child,
    pub(crate) watchdog: Watchdog,
    pub(crate) group: u32,
}

impl HubProc {
    /// Spawns `fiber hub serve` in its own process group.
    pub(crate) fn spawn(setup: &Setup) -> Self {
        Self::spawn_command(&mut setup.fiber(&["hub", "serve"]))
    }

    /// Spawns the `hub serve` command `serve`, which runs in its own
    /// process group.
    pub(crate) fn spawn_command(serve: &mut Command) -> Self {
        let mut child = serve.spawn().unwrap();
        let group = child.id();
        let _ = child.stdout.take();
        let _ = child.stderr.take();
        let watchdog = Watchdog::group(group);
        Self {
            child,
            watchdog,
            group,
        }
    }

    pub(crate) fn kill(&self, signal: &str) {
        fakes::kill_group(self.group, signal).unwrap();
    }

    /// Waits under [`DEADLINE`] for a hub that exits on its own, then
    /// asserts nothing remains in its group.
    pub(crate) fn wait(self) -> ExitStatus {
        let Self {
            mut child,
            watchdog,
            group,
        } = self;
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match finished.recv_timeout(DEADLINE) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited {DEADLINE:?} for the hub to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the hub's wait thread ended before the hub exited")
            }
        };
        assert!(
            !group_alive(group),
            "the hub left a process in its group behind"
        );
        watchdog.stand_down(DEADLINE);
        status
    }

    /// Kills the hub's group, then waits under [`DEADLINE`] for the hub
    /// to exit. For a test-sent SIGKILL: the hub's exit is proved by
    /// reaping it, and other members are not waited on.
    pub(crate) fn kill_and_wait(self) -> ExitStatus {
        let Self {
            mut child,
            watchdog,
            group,
        } = self;
        fakes::kill_group(group, "KILL").unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match finished.recv_timeout(DEADLINE) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited {DEADLINE:?} for the hub to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the hub's wait thread ended before the hub exited")
            }
        };
        watchdog.stand_down(DEADLINE);
        status
    }
}

/// A client on the hub's socket or a session's. Reads wait [`DEADLINE`]
/// each: expiry panics naming what was awaited, while the far end closing
/// the socket ends [`until_close`] and panics from [`recv`] and [`until`]
/// naming the wait the close cut short.
pub(crate) struct Socket {
    pub(crate) write: Mutex<UnixStream>,
    pub(crate) read: Mutex<BufReader<UnixStream>>,
}

impl Socket {
    pub(crate) fn connect(path: &Path) -> Self {
        Self::from(UnixStream::connect(path).expect("the socket accepted before the deadline"))
    }

    pub(crate) fn send(&self, line: &str) {
        let mut write = self.write.lock().unwrap();
        write.write_all(line.as_bytes()).unwrap();
        if !line.ends_with('\n') {
            write.write_all(b"\n").unwrap();
        }
        write.flush().unwrap();
    }

    /// One socket line: a line, or `None` when the far end closed the
    /// socket. A [`DEADLINE`] with neither panics naming `what`, with
    /// the lines before it.
    pub(crate) fn next(&self, what: &str, got: &[Value]) -> Option<Value> {
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
            Err(error) => panic!("reading the socket while waiting for {what}: {error}"),
        }
    }
}

pub(crate) fn recv(client: &Socket, what: &str) -> Value {
    match client.next(what, &[]) {
        Some(line) => line,
        None => panic!("the socket closed while waiting for {what}"),
    }
}

/// The next socket line that is not an `attention` hub line: a turn can
/// end between a command and its acknowledgement. Waits [`DEADLINE`] for
/// each line, like [`recv`].
pub(crate) fn recv_reply(client: &Socket, what: &str) -> Value {
    let mut got = Vec::new();
    loop {
        match client.next(what, &got) {
            Some(line) if line.get("kind").and_then(Value::as_str) == Some("attention") => {
                got.push(line);
            }
            Some(line) => return line,
            None => panic!("the socket closed while waiting for {what}"),
        }
    }
}

/// Collects socket lines until `done`, waiting `DEADLINE` for each: expiry
/// panics naming `what`, and the socket closing first panics too.
pub(crate) fn until(
    client: &Socket,
    what: &str,
    mut done: impl FnMut(&Value) -> bool,
) -> Vec<Value> {
    let mut lines = Vec::new();
    loop {
        let line = match client.next(what, &lines) {
            Some(line) => line,
            None => {
                panic!("the socket closed while waiting for {what}; got {lines:?}")
            }
        };
        let stop = done(&line);
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

/// Collects socket lines until the far end closes the socket, waiting
/// [`DEADLINE`] for each.
pub(crate) fn until_close(client: &Socket) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.next("the socket to close", &lines) {
        lines.push(line);
    }
    lines
}

/// Connects to the hub through `doors::hub::connect`, starting
/// `fiber hub serve` when none runs. The starter records the hub for the
/// caller to kill and wait. Returns the client and the `hub_hello`.
pub(crate) fn connect_hub(setup: &Setup, hub: &Arc<Mutex<Option<HubProc>>>) -> (Socket, Value) {
    connect_hub_within(setup, hub, DEADLINE)
}

/// The process clock running `scale` times slower, so the product's 5 s
/// connect deadline spans the test's own: a hub started from a freshly
/// built executable can take longer than 5 s on a loaded machine.
struct StretchedClock {
    anchor: std::time::Instant,
    scale: u32,
}

impl contract::clock::Clock for StretchedClock {
    fn now(&self) -> std::time::Instant {
        self.anchor + SystemClock.now().saturating_duration_since(self.anchor) / self.scale
    }

    fn wall(&self) -> std::time::SystemTime {
        SystemClock.wall()
    }

    fn sleep(&self, d: Duration) {
        SystemClock.sleep(d);
    }

    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        let bound = until.map(|until| until.saturating_duration_since(self.now()) * self.scale);
        wait(bound);
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// [`connect_hub`] with the whole connect, the hub's start and the
/// `hub_hello` read included, bounded by `wait` on the wall clock. `connect`
/// blocks, so it runs on a thread whose result the test receives with the
/// deadline.
pub(crate) fn connect_hub_within(
    setup: &Setup,
    hub: &Arc<Mutex<Option<HubProc>>>,
    wait: Duration,
) -> (Socket, Value) {
    use contract::clock::Clock as _;
    let slot = Arc::clone(hub);
    let mut serve = setup.fiber(&["hub", "serve"]);
    let home = setup.home();
    let (done, connected) = mpsc::channel();
    thread::spawn(move || {
        let mut start = move || {
            *slot.lock().unwrap() = Some(HubProc::spawn_command(&mut serve));
            Ok(())
        };
        let clock = StretchedClock {
            anchor: SystemClock.now(),
            scale: u32::try_from(wait.as_millis() / doors::hub::CONNECT_DEADLINE.as_millis())
                .unwrap_or(1)
                .max(1),
        };
        done.send(doors::hub::connect(&home, &mut start, &clock))
            .unwrap();
    });
    let connected = match connected.recv_timeout(wait) {
        Ok(connected) => connected.unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("waited {wait:?} for the hub to start and say hub_hello")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the hub connect thread ended without a result")
        }
    };
    let hello = serde_json::to_value(&connected.1).unwrap();
    (Socket::from(connected.0), hello)
}

impl Socket {
    pub(crate) fn from(stream: UnixStream) -> Self {
        let read = stream.try_clone().unwrap();
        read.set_read_timeout(Some(DEADLINE)).unwrap();
        Self {
            write: Mutex::new(stream),
            read: Mutex::new(BufReader::new(read)),
        }
    }
}

/// An `openai-responses` stream of `events`, then a completed reply.
pub(crate) fn stream(events: &[Value]) -> Response {
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

/// An `openai-responses` stream answering `Hello.` in two fragments.
pub(crate) fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// Waits under [`DEADLINE`] until `done` holds for the processes whose
/// command line contains `text`, naming `what` on expiry.
pub(crate) fn until_matching(text: &str, what: &str, done: fn(&[u32]) -> bool) {
    let (tx, rx) = mpsc::channel();
    let text = text.to_owned();
    thread::spawn(move || {
        while !done(&fakes::matching(&text).unwrap()) {
            thread::yield_now();
        }
        match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
    });
    assert!(
        rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for {what}"
    );
}

/// Guards every session the hub starts in `workspace`: each carries the
/// workspace path on its command line. Dropping it kills every process
/// whose command line holds the path, and its process group; its watchdog
/// does the same if the test process dies. Armed before `start` is sent,
/// so a session stuck in setup, or a start that fails or times out, leaves
/// nothing behind.
pub(crate) struct SessionGuard {
    pub(crate) workspace: String,
    pub(crate) watchdog: Option<Watchdog>,
}

impl SessionGuard {
    pub(crate) fn arm(workspace: &str) -> Self {
        Self {
            workspace: workspace.to_owned(),
            watchdog: Some(Watchdog::matching(workspace)),
        }
    }

    /// Stands the watchdog down, leaving the drop's kill as the only one.
    pub(crate) fn stand_down_watchdog(&mut self) {
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(DEADLINE);
        }
    }

    /// Waits under [`DEADLINE`] until no process's command line holds the
    /// workspace path, then stands the guard down.
    pub(crate) fn wait_gone(mut self) {
        until_matching(
            &self.workspace,
            "every process holding the workspace path to exit",
            <[u32]>::is_empty,
        );
        self.stand_down_watchdog();
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        match fakes::kill_matching(&self.workspace) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// Starts a session through the hub with `content`, and returns its id.
/// The caller arms a [`SessionGuard`] first.
pub(crate) fn start_session(client: &Socket, workspace: &str, content: &str) -> String {
    client.send(&format!(
        "{{\"id\":\"c_start\",\"command\":\"start\",\"args\":{{\"workspace\":\"{workspace}\",\"content\":[{{\"type\":\"text\",\"text\":\"{content}\"}}]}}}}"
    ));
    let ack = recv_reply(client, "the start acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    ack["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned()
}

/// Subscribes `full` to `session` through the hub.
pub(crate) fn subscribe(client: &Socket, session: &str) {
    client.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{session}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let ack = recv_reply(client, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
}

/// Closes the session on the direct socket, and waits for it to leave.
pub(crate) fn close_session(socket: &Socket) {
    socket.send(r#"{"id":"c_close_sub","command":"subscribe","args":{"level":"full"}}"#);
    let ack = recv(socket, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    socket.send(r#"{"id":"c_close","command":"close"}"#);
    until_close(socket);
}

/// Runs `command` to its exit under [`DEADLINE`], naming `what` on expiry,
/// and checks it left no process in its group.
pub(crate) fn run_to_exit(what: &str, mut command: Command) -> std::process::Output {
    let child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(DEADLINE) {
        Ok(output) => output.unwrap(),
        Err(error) => panic!("waited {DEADLINE:?} for {what} to exit: {error}"),
    };
    assert!(
        !group_alive(group),
        "{what} left a process in its group behind"
    );
    watchdog.stand_down(DEADLINE);
    output
}
