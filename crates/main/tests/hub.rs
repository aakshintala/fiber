//! Binary-level tests of the internal hub command
//! (`docs/invocation.md`, "The hub"): the built `fiber` runs `hub serve`
//! in its own process group with its own `FIBER_HOME`, holding an ordinary
//! provider whose base URL is the fake server. Clients reach sessions
//! through the hub, and the hub outlives no session.

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
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

/// How long one `fiber` run, one socket line, or one hub exit may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// The prompt a started session runs, and the marker the log must never hold.
const PROMPT: &str = "the-volume-of-the-meeting-room";

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fh");
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

    fn hub_socket(&self) -> PathBuf {
        self.home().join("run").join("hub")
    }

    fn session_socket(&self, id: &str) -> PathBuf {
        self.home().join("run").join(id)
    }

    fn hub_log(&self) -> String {
        fs::read_to_string(self.home().join("logs").join("hub.log")).unwrap_or_default()
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

/// The process clock behind `contract::clock::Clock`.
struct SystemClock;

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
struct HubProc {
    child: Child,
    watchdog: Watchdog,
    group: u32,
}

impl HubProc {
    /// Spawns `fiber hub serve` in its own process group.
    fn spawn(setup: &Setup) -> Self {
        let mut child = setup.fiber(&["hub", "serve"]).spawn().unwrap();
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

    fn kill(&self, signal: &str) {
        fakes::kill_group(self.group, signal).unwrap();
    }

    /// Waits under [`DEADLINE`] for the process to exit, and asserts that
    /// nothing it started is left in its group.
    fn wait(self) -> ExitStatus {
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
}

/// A client on the hub's socket or a session's. Reads wait [`DEADLINE`]
/// each: expiry panics naming what was awaited, while the far end closing
/// the socket ends [`until_close`] and panics from [`recv`] and [`until`]
/// naming the wait the close cut short.
struct Socket {
    write: Mutex<UnixStream>,
    read: Mutex<BufReader<UnixStream>>,
}

impl Socket {
    fn connect(path: &Path) -> Self {
        Self::from(UnixStream::connect(path).expect("the socket accepted before the deadline"))
    }

    fn send(&self, line: &str) {
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
            Err(error) => panic!("reading the socket while waiting for {what}: {error}"),
        }
    }
}

fn recv(client: &Socket, what: &str) -> Value {
    match client.next(what, &[]) {
        Some(line) => line,
        None => panic!("the socket closed while waiting for {what}"),
    }
}

/// Collects socket lines until `done`, waiting `DEADLINE` for each: expiry
/// panics naming `what`, and the socket closing first panics too.
fn until(client: &Socket, what: &str, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
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
fn until_close(client: &Socket) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.next("the socket to close", &lines) {
        lines.push(line);
    }
    lines
}

/// Connects to the hub through `doors::hub::connect`, starting
/// `fiber hub serve` when none runs. The starter records the hub for the
/// caller to kill and wait. Returns the client and the `hub_hello`.
fn connect_hub(setup: &Setup, hub: &Arc<Mutex<Option<HubProc>>>) -> (Socket, Value) {
    let slot = Arc::clone(hub);
    let mut start = move || {
        *slot.lock().unwrap() = Some(HubProc::spawn(setup));
        Ok(())
    };
    let connected = doors::hub::connect(&setup.home(), &mut start, &SystemClock).unwrap();
    let hello = serde_json::to_value(&connected.1).unwrap();
    (Socket::from(connected.0), hello)
}

impl Socket {
    fn from(stream: UnixStream) -> Self {
        let read = stream.try_clone().unwrap();
        read.set_read_timeout(Some(DEADLINE)).unwrap();
        Self {
            write: Mutex::new(stream),
            read: Mutex::new(BufReader::new(read)),
        }
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

/// Kills process group `group` on drop. After the group is empty,
/// [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// A session the hub started in its own process group: killed on drop,
/// and by its watchdog if the test process dies.
struct SessionProc {
    id: String,
    group: u32,
    guard: KillGroup,
    watchdog: Watchdog,
}

impl SessionProc {
    /// Waits under [`DEADLINE`] for the session's process group to empty,
    /// then stands the guards down.
    fn wait_gone(self) {
        let Self {
            group,
            guard,
            watchdog,
            ..
        } = self;
        let (done, gone) = mpsc::channel();
        thread::spawn(move || {
            while group_alive(group) {
                thread::yield_now();
            }
            match done.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
        });
        assert!(
            gone.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for the session's process group to empty"
        );
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
    }
}

/// Starts a session through the hub with `content`, and guards its process
/// group. The session writes its pid into its log's lock before it binds
/// `run/<id>`, and the hub acknowledges only once that socket accepts, so
/// the pid is there once the acknowledgement arrives. The hub starts each
/// session in its own process group, so the pid is the group.
fn start_session(setup: &Setup, client: &Socket, workspace: &str, content: &str) -> SessionProc {
    client.send(&format!(
        "{{\"id\":\"c_start\",\"command\":\"start\",\"args\":{{\"workspace\":\"{workspace}\",\"content\":[{{\"type\":\"text\",\"text\":\"{content}\"}}]}}}}"
    ));
    let ack = recv(client, "the start acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let id = ack["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned();
    let project = fs::canonicalize(workspace).unwrap();
    let lock = log::sessions_dir(&setup.home(), &project)
        .join(&id)
        .join("session.lock");
    let group: u32 = fs::read_to_string(&lock)
        .unwrap()
        .trim()
        .parse()
        .expect("the session's lock names its pid");
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    assert!(
        group_alive(group),
        "the lock's pid names the session's group"
    );
    SessionProc {
        id,
        group,
        guard,
        watchdog,
    }
}

/// Subscribes `full` to `session` through the hub.
fn subscribe(client: &Socket, session: &str) {
    client.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{session}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let ack = recv(client, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
}

/// Closes the session on the direct socket, and waits for it to leave.
fn close_session(socket: &Socket) {
    socket.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let ack = recv(socket, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    socket.send(r#"{"id":"c_close","command":"close"}"#);
    until_close(socket);
}

#[test]
fn a_turn_runs_through_the_hub_and_the_hub_outlives_no_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, hello) = connect_hub(&setup, &hub);
    assert_eq!(hello["kind"], "hub_hello");
    assert_eq!(hello["payload"]["fiber_version"], env!("CARGO_PKG_VERSION"));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let session = start_session(&setup, &client, &workspace, PROMPT);
    subscribe(&client, &session.id);
    let rest = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        rest.iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the fake model's text arrived through the hub: {rest:?}"
    );
    drop(client);
    // SIGKILL the hub: the session keeps running on its own socket.
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill("KILL");
    let status = hub.wait();
    assert!(!status.success());
    let direct = Socket::connect(&setup.session_socket(&session.id));
    close_session(&direct);
    drop(direct);
    assert!(
        !setup.session_socket(&session.id).exists(),
        "the closed session unlinked its socket"
    );
    session.wait_gone();
}

#[test]
fn two_racing_clients_share_one_hub() {
    let setup = Setup::new();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            let hub = Arc::new(Mutex::new(None));
            let (client, hello) = connect_hub(&setup, &hub);
            // Both clients are registered once both hellos arrived.
            barrier.wait();
            client.send(r#"{"id":"c_status","command":"status","args":{}}"#);
            let status = recv(&client, "the status acknowledgement");
            // Both stay open until both `status` answers are read.
            barrier.wait();
            drop(client);
            (hello, status, hub)
        });
        barrier.wait();
        let hub = Arc::new(Mutex::new(None));
        let (client, hello) = connect_hub(&setup, &hub);
        barrier.wait();
        client.send(r#"{"id":"c_status","command":"status","args":{}}"#);
        let status = recv(&client, "the status acknowledgement");
        barrier.wait();
        let (other_hello, other_status, other_hub) = first.join().unwrap();
        assert_eq!(hello["kind"], "hub_hello");
        assert_eq!(other_hello["kind"], "hub_hello");
        assert_eq!(status["payload"]["result"]["clients"], 2);
        assert_eq!(other_status["payload"]["result"]["clients"], 2);
        // One hub: exactly one `hub_started` line.
        let started = setup
            .hub_log()
            .lines()
            .filter(|line| line.contains("\"code\":\"hub_started\""))
            .count();
        assert_eq!(started, 1);
        drop(client);
        for slot in [&hub, &other_hub] {
            if let Some(running) = slot.lock().unwrap().take() {
                running.kill("KILL");
                running.wait();
            }
        }
    });
}

#[test]
fn sigterm_stops_the_hub_and_leaves_sessions_accepting() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let session = start_session(&setup, &client, &workspace, PROMPT);
    subscribe(&client, &session.id);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill("TERM");
    let status = hub.wait();
    assert_eq!(status.code(), Some(143));
    // The session keeps accepting on its own socket.
    let direct = Socket::connect(&setup.session_socket(&session.id));
    close_session(&direct);
    drop(direct);
    session.wait_gone();
    drop(client);
    let log = setup.hub_log();
    for code in [
        "hub_started",
        "client_connected",
        "session_started",
        "hub_stopped",
    ] {
        assert!(
            log.contains(&format!("\"code\":\"{code}\"")),
            "{code}:\n{log}"
        );
    }
    assert!(log.contains("The hub stopped: signal."));
    for line in log.lines() {
        assert!(!line.contains(PROMPT), "no prompt text in the log");
        assert!(!line.contains(&workspace), "no workspace path in the log");
    }
}

#[test]
fn an_idle_hub_exits_on_its_own() {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"hub": {"idle_exit_ms": 200}}),
    );
    let hub = HubProc::spawn(&setup);
    let status = hub.wait();
    assert_eq!(status.code(), Some(0));
    assert!(!setup.hub_socket().exists());
}

#[test]
fn a_relative_workspace_is_invalid_arguments() {
    let setup = Setup::new();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    client.send(r#"{"id":"c_start","command":"start","args":{"workspace":"relative/path"}}"#);
    let rejected = recv(&client, "the start rejection");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    assert_eq!(rejected["payload"]["command_id"], "c_start");
    drop(client);
    if let Some(running) = hub.lock().unwrap().take() {
        running.kill("KILL");
        running.wait();
    }
}
