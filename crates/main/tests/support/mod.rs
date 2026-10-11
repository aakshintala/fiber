//! Helpers the binary-level tests share: the test's one deadline, Fiber
//! home and a fake provider, `fiber` invocations in their own process
//! group, the hub process, socket clients with deadlines, process signals
//! bounded by the deadline, and the guard that kills every session a test
//! started.

#![allow(
    dead_code,
    reason = "each test binary uses its own subset of the helpers"
)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
use std::fs;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

pub(crate) mod package;
pub(crate) mod pty;

/// Writes a healthy extension install record beside its manifest.
pub(crate) fn write_record(dir: &Path) {
    let text = fs::read_to_string(dir.join("extension.json")).unwrap();
    let manifest: Value = serde_json::from_str(&text).unwrap();
    let name = manifest.get("name").and_then(|n| n.as_str()).unwrap();
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("v0.0.0");
    fs::write(
        dir.join(".fiber.json"),
        json!({"name": name, "version": version, "requested": true, "source": {"path": "/p"}})
            .to_string(),
    )
    .unwrap();
}

/// The test's one deadline and its bounds, shared from `fakes`
/// (`docs/testing.md`, "Waits and timeouts").
pub(crate) use fakes::clock::SystemClock;
pub(crate) use fakes::deadline::Deadline;
#[allow(
    unused_imports,
    reason = "only the deadline test reads the bounds; each test binary compiles its own subset"
)]
pub(crate) use fakes::deadline::{BUDGET, CLEANUP, WAITS};

/// A `TimedOut` error naming `what`.
fn timed_out(what: &str) -> io::Error {
    io::Error::new(
        ErrorKind::TimedOut,
        format!("waited until the deadline for {what}"),
    )
}

/// One line from `reader`, with its newline, bounded as a whole by
/// `deadline.left()`. A full line already buffered is returned without
/// reading. Otherwise every read takes what remains, and at zero it returns
/// a `TimedOut` error naming `what` without reading. `Ok(None)` is the end
/// of the stream before any byte; a partial line at the end is a line.
pub(crate) fn read_line(
    reader: &mut BufReader<UnixStream>,
    deadline: Deadline,
    what: &str,
) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let buffered = reader.buffer();
        if let Some(end) = buffered.iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(buffered.get(..=end).unwrap_or_default());
            reader.consume(end + 1);
            break;
        }
        let taken = buffered.len();
        line.extend_from_slice(buffered);
        reader.consume(taken);
        let left = deadline.left();
        if left.is_zero() {
            return Err(timed_out(what));
        }
        // macOS refuses a timeout with EINVAL once the peer has closed; a
        // read of a closed socket does not block, so it goes ahead.
        match reader.get_ref().set_read_timeout(Some(left)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::InvalidInput => {}
            Err(error) => return Err(error),
        }
        match reader.fill_buf() {
            Ok([]) if line.is_empty() => return Ok(None),
            Ok([]) => break,
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error)
                if error.kind() == ErrorKind::TimedOut || error.kind() == ErrorKind::WouldBlock =>
            {
                return Err(timed_out(what));
            }
            Err(error) => return Err(error),
        }
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

/// Writes all of `bytes` to `stream` in chunks of at most 4 KiB, each
/// bounded by what remains of `deadline.left()`. At zero it returns a
/// `TimedOut` error naming `what` without writing.
#[track_caller]
pub(crate) fn write_line(
    stream: &mut UnixStream,
    deadline: Deadline,
    bytes: &[u8],
    what: &str,
) -> io::Result<()> {
    let mut rest = bytes;
    while !rest.is_empty() {
        let left = deadline.left();
        if left.is_zero() {
            return Err(timed_out(what));
        }
        stream.set_write_timeout(Some(left))?;
        let chunk = rest.get(..4096).unwrap_or(rest);
        match stream.write(chunk) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(written) => rest = rest.get(written..).unwrap_or_default(),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error)
                if error.kind() == ErrorKind::TimedOut || error.kind() == ErrorKind::WouldBlock =>
            {
                return Err(timed_out(what));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Runs the blocking call `work` (a connect, a terminal write) on a thread
/// and returns its value, waiting `deadline.left()`: on expiry it panics
/// naming `what`, and a panic in `work` is raised again naming `what`. It
/// never joins the thread.
#[track_caller]
pub(crate) fn bounded<T: Send + 'static>(
    deadline: Deadline,
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    fakes::within(what, deadline.left(), work)
}

/// `fakes::kill_group` bounded by `deadline.cleanup()`: it sends a signal,
/// it does not wait on the code under test.
#[track_caller]
pub(crate) fn kill_group(deadline: Deadline, group: u32, signal: &'static str) -> io::Result<bool> {
    fakes::within(
        &format!("kill -{signal} of process group {group}"),
        deadline.cleanup(),
        move || fakes::kill_group(group, signal),
    )
}

/// `fakes::kill_pid` bounded by `deadline.cleanup()`.
#[track_caller]
pub(crate) fn kill_pid(deadline: Deadline, pid: u32, signal: &'static str) -> io::Result<bool> {
    fakes::within(
        &format!("kill -{signal} of pid {pid}"),
        deadline.cleanup(),
        move || fakes::kill_pid(pid, signal),
    )
}

/// `fakes::kill_matching` bounded by `deadline.cleanup()`.
#[track_caller]
pub(crate) fn kill_matching(deadline: Deadline, text: &str) -> io::Result<()> {
    let text = text.to_owned();
    fakes::within(
        &format!("killing every process matching {text}"),
        deadline.cleanup(),
        move || fakes::kill_matching(&text),
    )
}

/// Whether any process remains in process group `group`.
#[track_caller]
pub(crate) fn group_alive(deadline: Deadline, group: u32) -> bool {
    kill_group(deadline, group, "0").unwrap()
}

/// For `Drop` impls: sends `signal` to `group` on a thread and returns at
/// once, never waiting and never panicking. A thread that cannot start
/// sends nothing; the group's watchdog still kills it when the test
/// process exits.
pub(crate) fn kill_group_detached(group: u32, signal: &'static str) {
    let spawned = thread::Builder::new().spawn(move || match fakes::kill_group(group, signal) {
        Ok(_) | Err(_) => {}
    });
    match spawned {
        Ok(_) | Err(_) => {}
    }
}

/// [`kill_group_detached`] for every process whose command line holds
/// `text`, and its process group.
pub(crate) fn kill_matching_detached(text: &str) {
    let text = text.to_owned();
    let spawned = thread::Builder::new().spawn(move || match fakes::kill_matching(&text) {
        Ok(()) | Err(_) => {}
    });
    match spawned {
        Ok(_) | Err(_) => {}
    }
}

/// The timeout branch of a wait for a started process's exit: kills
/// `group`, reaps through `reap` and checks the group empties, each within
/// `deadline.cleanup()`, then panics naming `what`.
#[track_caller]
pub(crate) fn expired<T>(
    deadline: Deadline,
    group: u32,
    reap: &mpsc::Receiver<T>,
    what: &str,
) -> ! {
    // A kill that fails shows as the group not emptying below.
    match kill_group(deadline, group, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let reaped = deadline.cleanup_phase().recv(reap).is_ok();
    assert!(
        fakes::group_empties(group, deadline.cleanup()),
        "waited until the deadline for {what}, and its group outlived the kill"
    );
    panic!("waited until the deadline for {what} (reaped after the kill: {reaped})")
}

/// A temporary root holding Fiber home and the workspace, removed on drop,
/// and the test's deadline. Its name is short: a session's socket path must
/// fit in 103 bytes on macOS.
pub(crate) struct Setup {
    pub(crate) root: fakes::TempDir,
    pub(crate) deadline: Deadline,
}

impl Setup {
    /// Starts the test's deadline before any setup work.
    pub(crate) fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fh");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root, deadline }
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
            .envs(fakes::check_run())
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

/// A hub the test started: killed on drop unless forgotten after a clean wait.
pub(crate) struct HubProc {
    pub(crate) child: Child,
    pub(crate) watchdog: Watchdog,
    pub(crate) group: u32,
    pub(crate) deadline: Deadline,
}

impl HubProc {
    /// Spawns `fiber hub serve` in its own process group.
    pub(crate) fn spawn(setup: &Setup) -> Self {
        Self::spawn_command(setup.deadline, &mut setup.fiber(&["hub", "serve"]))
    }

    /// Spawns the `hub serve` command `serve`, which runs in its own
    /// process group.
    pub(crate) fn spawn_command(deadline: Deadline, serve: &mut Command) -> Self {
        let mut child = serve.spawn().unwrap();
        let group = child.id();
        let _ = child.stdout.take();
        let _ = child.stderr.take();
        let watchdog = Watchdog::group(group);
        Self {
            child,
            watchdog,
            group,
            deadline,
        }
    }

    #[track_caller]
    pub(crate) fn kill(&self, signal: &'static str) {
        kill_group(self.deadline, self.group, signal).unwrap();
    }

    /// Waits under the test's [`Deadline`] for a hub that exits on its own,
    /// then asserts nothing remains in its group.
    #[track_caller]
    pub(crate) fn wait(self) -> ExitStatus {
        let Self {
            mut child,
            watchdog,
            group,
            deadline,
        } = self;
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                expired(deadline, group, &finished, "the hub to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the hub's wait thread ended before the hub exited")
            }
        };
        assert!(
            !group_alive(deadline, group),
            "the hub left a process in its group behind"
        );
        watchdog.stand_down(deadline.cleanup());
        status
    }

    /// Kills the hub's group, then waits under the test's [`Deadline`] for
    /// the hub to exit. For a test-sent SIGKILL: the hub's exit is proved by
    /// reaping it, and other members are not waited on.
    #[track_caller]
    pub(crate) fn kill_and_wait(self) -> ExitStatus {
        let Self {
            mut child,
            watchdog,
            group,
            deadline,
        } = self;
        kill_group(deadline, group, "KILL").unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                expired(deadline, group, &finished, "the killed hub to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the hub's wait thread ended before the hub exited")
            }
        };
        watchdog.stand_down(deadline.cleanup());
        status
    }
}

/// A client on the hub's socket or a session's. Every read and write takes
/// what remains of the test's [`Deadline`]: expiry panics naming what was
/// awaited, while the far end closing the socket ends [`until_close`] and
/// panics from [`recv`] and [`until`] naming the wait the close cut short.
pub(crate) struct Socket {
    pub(crate) write: Mutex<UnixStream>,
    pub(crate) read: Mutex<BufReader<UnixStream>>,
    /// Lines [`recv_answer`] read past, returned by [`Socket::next`] first.
    kept: Mutex<std::collections::VecDeque<Value>>,
    deadline: Deadline,
}

impl Socket {
    /// Connects to `path` on a thread bounded by the deadline.
    #[track_caller]
    pub(crate) fn connect(deadline: Deadline, path: &Path) -> Self {
        let target = path.to_owned();
        let stream = bounded(
            deadline,
            &format!("a connection to {}", path.display()),
            move || UnixStream::connect(target),
        );
        Self::from(
            deadline,
            stream.expect("the socket accepted before the deadline"),
        )
    }

    #[track_caller]
    pub(crate) fn send(&self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        if !line.ends_with('\n') {
            bytes.push(b'\n');
        }
        let mut write = self.write.lock().unwrap();
        if let Err(error) = write_line(&mut write, self.deadline, &bytes, "sending a line") {
            panic!("writing the socket: {error}");
        }
    }

    /// One socket line: a line, or `None` when the far end closed the
    /// socket. Neither by the deadline panics naming `what`, with the lines
    /// before it.
    pub(crate) fn next(&self, what: &str, got: &[Value]) -> Option<Value> {
        if let Some(line) = self.kept.lock().unwrap().pop_front() {
            return Some(line);
        }
        match read_line(&mut self.read.lock().unwrap(), self.deadline, what) {
            Ok(None) => None,
            Ok(Some(buf)) => {
                let line = buf.trim_end_matches(&['\r', '\n'][..]).to_owned();
                Some(serde_json::from_str(&line).unwrap_or(Value::String(line)))
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                panic!("waited until the deadline for {what}; got {got:?}")
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
/// end between a command and its acknowledgement. Each line takes what
/// remains of the test's deadline, like [`recv`].
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

/// The answer to command `id`: the line whose `payload.command_id` is `id`.
/// `docs/invocation.md` orders no other line against it, so a feed line may
/// come first: lines before it stay for the caller's next [`recv`] or
/// [`until`], in order, except `attention` lines, which [`recv_reply`] drops
/// too.
#[track_caller]
pub(crate) fn recv_answer(client: &Socket, id: &str, what: &str) -> Value {
    let mut passed = Vec::new();
    let answer = loop {
        match client.next(what, &passed) {
            Some(line) if line["payload"]["command_id"] == id => break line,
            Some(line) if line.get("kind").and_then(Value::as_str) == Some("attention") => {}
            Some(line) => passed.push(line),
            None => panic!("the socket closed while waiting for {what}; got {passed:?}"),
        }
    };
    let mut kept = client.kept.lock().unwrap();
    for line in passed.into_iter().rev() {
        kept.push_front(line);
    }
    answer
}

/// Collects socket lines until `done`, each taking what remains of the
/// test's deadline: expiry panics naming `what`, and the socket closing
/// first panics too.
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

/// Collects socket lines until the far end closes the socket, each taking
/// what remains of the test's deadline.
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
#[track_caller]
pub(crate) fn connect_hub(setup: &Setup, hub: &Arc<Mutex<Option<HubProc>>>) -> (Socket, Value) {
    connect_hub_within(setup, hub, setup.deadline.left())
}

/// The process clock running `scale` times slower, so the product's 5 s
/// connect deadline spans the test's own: a hub started from a freshly
/// built executable can take longer than 5 s on a loaded machine.
struct StretchedClock {
    anchor: std::time::Instant,
    scale: u32,
}

impl Clock for StretchedClock {
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

    #[track_caller]
    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// [`connect_hub`] with the whole connect, the hub's start and the
/// `hub_hello` read included, bounded by `wait` on the wall clock. `connect`
/// blocks, so it runs on a thread whose result the test receives with the
/// deadline. A zero `wait` keeps the clock's scale at 1.
#[track_caller]
pub(crate) fn connect_hub_within(
    setup: &Setup,
    hub: &Arc<Mutex<Option<HubProc>>>,
    wait: Duration,
) -> (Socket, Value) {
    let slot = Arc::clone(hub);
    let deadline = setup.deadline;
    let mut serve = setup.fiber(&["hub", "serve"]);
    let home = setup.home();
    let (done, connected) = mpsc::channel();
    thread::spawn(move || {
        let mut start = move || {
            *slot.lock().unwrap() = Some(HubProc::spawn_command(deadline, &mut serve));
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
    let connected = match Deadline::after(wait).recv(&connected) {
        Ok(connected) => connected.unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("waited {wait:?} for the hub to start and say hub_hello")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the hub connect thread ended without a result")
        }
    };
    let hello = serde_json::to_value(&connected.1).unwrap();
    (Socket::from(deadline, connected.0), hello)
}

impl Socket {
    pub(crate) fn from(deadline: Deadline, stream: UnixStream) -> Self {
        let read = stream.try_clone().unwrap();
        Self {
            write: Mutex::new(stream),
            read: Mutex::new(BufReader::new(read)),
            kept: Mutex::default(),
            deadline,
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

/// Guards every session the hub starts in `workspace`: each carries the
/// workspace path on its command line. Dropping it kills every process
/// whose command line holds the path, and its process group; its watchdog
/// does the same if the test process dies. Armed before `start` is sent,
/// so a session stuck in setup, or a start that fails or times out, leaves
/// nothing behind.
pub(crate) struct SessionGuard {
    pub(crate) workspace: String,
    pub(crate) watchdog: Option<Watchdog>,
    pub(crate) deadline: Deadline,
}

impl SessionGuard {
    pub(crate) fn arm(deadline: Deadline, workspace: &str) -> Self {
        Self {
            workspace: workspace.to_owned(),
            watchdog: Some(Watchdog::matching(workspace)),
            deadline,
        }
    }

    /// Stands the watchdog down, leaving the drop's kill as the only one.
    #[track_caller]
    pub(crate) fn stand_down_watchdog(&mut self) {
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(self.deadline.cleanup());
        }
    }

    /// Called after the signal that every session in the workspace is ending
    /// (its socket closed, its session_left, or the guard's SIGKILL): waits
    /// under the test's [`Deadline`] for every process holding the workspace
    /// path to exit, then stands the guard down.
    #[track_caller]
    pub(crate) fn wait_gone(mut self) {
        match fakes::try_matching_exits(&self.workspace, self.deadline.left()) {
            Ok(()) => {}
            Err(err) => panic!(
                "waited until the deadline for every process holding the workspace path to exit: {err}"
            ),
        }
        self.stand_down_watchdog();
    }
}

impl Drop for SessionGuard {
    /// Kills on a thread and waits for it up to `cleanup()`, so the session
    /// is dead when the drop returns; a drop never blocks past that or panics.
    fn drop(&mut self) {
        let text = self.workspace.clone();
        let (done, finished) = mpsc::channel();
        let spawned = thread::Builder::new().spawn(move || {
            let killed = fakes::kill_matching(&text);
            match done.send(killed) {
                Ok(()) | Err(_) => {}
            }
        });
        if spawned.is_ok() {
            match self.deadline.cleanup_phase().recv(&finished) {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

/// Starts a session through the hub with `content`, and returns its id.
/// The caller arms a [`SessionGuard`] first.
#[track_caller]
pub(crate) fn start_session(client: &Socket, workspace: &str, content: &str) -> String {
    client.send(&format!(
        "{{\"id\":\"c_start\",\"command\":\"start\",\"args\":{{\"workspace\":\"{workspace}\",\"content\":[{{\"type\":\"text\",\"text\":\"{content}\"}}]}}}}"
    ));
    let ack = recv_answer(client, "c_start", "the start acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    ack["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned()
}

/// Subscribes `full` to `session` through the hub.
#[track_caller]
pub(crate) fn subscribe(client: &Socket, session: &str) {
    client.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{session}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let ack = recv_answer(client, "c_sub", "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
}

/// Closes the session on the direct socket, and waits for it to leave.
#[track_caller]
pub(crate) fn close_session(socket: &Socket) {
    socket.send(r#"{"id":"c_close_sub","command":"subscribe","args":{"level":"full"}}"#);
    let ack = recv(socket, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    socket.send(r#"{"id":"c_close","command":"close"}"#);
    until_close(socket);
}

/// Runs `command`, which runs in its own process group, to its exit under
/// the test's [`Deadline`], naming `what` on expiry, and checks it left no
/// process in its group.
#[track_caller]
pub(crate) fn run_to_exit(deadline: Deadline, what: &str, mut command: Command) -> Output {
    let child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            expired(deadline, group, &finished, &format!("{what} to exit"))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the wait thread of {what} ended before it exited")
        }
    };
    assert!(
        !group_alive(deadline, group),
        "{what} left a process in its group behind"
    );
    watchdog.stand_down(deadline.cleanup());
    output
}

/// What the log holds of one writer at a time: one `session_started`,
/// `fiber_started` lines of which only the first is not resumed, and
/// `seq` carrying on without a gap.
pub(crate) fn assert_one_continued_log(lines: &[Value], started: usize) {
    let count = |kind: &str| lines.iter().filter(|line| line["kind"] == kind).count();
    assert_eq!(count("session_started"), 1);
    let fibers: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "fiber_started")
        .collect();
    assert_eq!(fibers.len(), started, "{lines:?}");
    assert_eq!(fibers[0]["payload"]["resumed"], false);
    assert!(
        fibers[1..]
            .iter()
            .all(|line| line["payload"]["resumed"] == true)
    );
    let seqs: Vec<u64> = lines
        .iter()
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    assert!(
        seqs.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "{seqs:?}"
    );
}

/// A finished `function_call` for `name` with `arguments`.
pub(crate) fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
}

/// A `session_status` line: ephemeral, and written by an observer thread, so
/// where it falls among the loop's own lines is not what these tests pin.
/// `tests/socket.rs` reads it.
pub(crate) fn is_status(line: &str) -> bool {
    line.contains(r#""kind":"session_status""#)
}

/// An `openai-responses` stream answering `text`: what a scripted
/// reviewer verdict reads as.
pub(crate) fn text_reply(text: &str) -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

pub(crate) fn tool_names(body: &[u8]) -> Vec<String> {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The first stdout line, waited for under the test's [`Deadline`].
#[track_caller]
pub(crate) fn first_line(deadline: Deadline, stdout: &mpsc::Receiver<String>) -> Value {
    serde_json::from_str(
        &deadline
            .recv(stdout)
            .expect("waited until the deadline for fiber_started"),
    )
    .unwrap()
}

/// The event kinds of a turn answered by [`hello`].
pub(crate) const HELLO_KINDS: [&str; 15] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
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

/// A 1x1 PNG, 69 bytes: within every cap, so the image child stores it byte
/// for byte (as in `tests/tools.rs`).
pub(crate) const PIXEL: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// [`PIXEL`] as base64.
pub(crate) const PIXEL_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// The prompt a started session runs, and the marker the log must never hold.
pub(crate) const PROMPT: &str = "the-volume-of-the-meeting-room";

/// The query every test searches for.
pub(crate) const QUERY: &str = "retry budget";

pub(crate) const ROOT: &str = "s_00000000000000d1";

/// The result of an `ask_user` call whose questions went to the driver.
pub(crate) const SENT: &str =
    "The questions went to the driver. The answers arrive as the next prompt.";

pub(crate) const SESSION_KINDS: &[&str] = &[
    "session_started",
    "fiber_started",
    "extensions_loaded",
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
];

/// Truecolour env for the layout runs.
pub(crate) const TRUECOLOUR: [(&str, &str); 3] = [
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("TERM_PROGRAM", "ghostty"),
];

pub(crate) fn answered<'a>(lines: &'a [Value], id: &str) -> &'a Value {
    lines
        .iter()
        .find(|line| line["payload"]["command_id"] == id)
        .unwrap_or_else(|| panic!("no answer for {id}"))
}

/// The tool outputs the second request carries, in order.
pub(crate) fn outputs_sent(server: &ProviderServer) -> Vec<String> {
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    second["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| item["output"].as_str().unwrap().to_owned())
        .collect()
}

/// Every file's bytes under `dir`, as text, joined.
pub(crate) fn on_disk(dir: &Path) -> String {
    let mut all = String::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.is_file() {
                all.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
            }
        }
    }
    all
}

/// Whether `bytes` holds `marker`.
pub(crate) fn holds_marker(bytes: &[u8], marker: &str) -> bool {
    let marker = marker.as_bytes();
    bytes.windows(marker.len()).any(|window| window == marker)
}

/// The session's directory, from its id.
pub(crate) fn session_dir(setup: &Setup, id: &str) -> PathBuf {
    log::sessions_dir(&setup.home(), &doors::project(&setup.workspace())).join(id)
}

/// Runs the system `git` in `dir`, in its own process group, to its exit
/// under the test's [`Deadline`].
#[track_caller]
pub(crate) fn git(deadline: Deadline, dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = run_to_exit(deadline, &format!("git {args:?}"), command);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// The event kinds of `lines`, in order, without `session_status`: an
/// observer thread writes it, so where it falls among the loop's own
/// lines is not what this test pins (as `tests/session_command.rs`
/// filters it).
pub(crate) fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// The event kinds of `lines`, in order, without `session_status` or
/// `attention`: an observer thread writes `session_status`, so where it
/// falls among the loop's own lines is not what this test pins (as
/// `tests/session_command.rs` filters it), and the hub's `attention` line
/// derives from that status, so whether it comes and where is not pinned
/// either.
pub(crate) fn kinds_without_attention(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status" && line["kind"] != "attention")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
pub(crate) fn hello_inline_completed() -> Response {
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
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// An `openai-responses` stream answering `Hello.`.
pub(crate) fn hello_single_delta() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hello."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// An `openai-responses` stream answering `Hello.`.
pub(crate) fn hello_item_done() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
    }})])
}

pub(crate) fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
// The group kill needs the id where the guard is armed, so the field is
// visible to the tests sharing this guard.
pub(crate) struct KillGroup(pub(crate) u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        kill_group_detached(self.0, "KILL");
    }
}

pub(crate) fn expected(kinds: &[&str]) -> Vec<Value> {
    kinds.iter().map(|kind| json!({"kind": kind})).collect()
}
