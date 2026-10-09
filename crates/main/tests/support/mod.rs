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
use std::time::{Duration, Instant};

use contract::clock::Clock;
use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

pub(crate) mod package;

/// nextest kills a test at 120 s (`.config/nextest.toml`): a test's
/// deadlines sum to half of that.
pub(crate) const BUDGET: Duration = Duration::from_secs(60);
/// From the test's start, when its success-path waits must be over.
pub(crate) const WAITS: Duration = Duration::from_secs(40);
/// From the test's start, when cleanup waits must be over. `BUDGET -
/// CLEANUP` holds the fixed bounds inside `fakes` that take no deadline.
pub(crate) const CLEANUP: Duration = Duration::from_secs(50);

/// The test's one deadline, started at its first operation. Every wait
/// takes what remains of it, so a second wait gets only what the first
/// left, and the waits of one test sum to [`BUDGET`].
#[derive(Clone, Copy)]
pub(crate) struct Deadline {
    start: Instant,
    clock: &'static dyn Clock,
}

impl Deadline {
    /// A deadline on the process clock, starting now.
    pub(crate) fn start() -> Self {
        Self::on(&SystemClock)
    }

    /// A deadline on `clock`, starting at its `now()`.
    pub(crate) fn on(clock: &'static dyn Clock) -> Self {
        Self {
            start: clock.now(),
            clock,
        }
    }

    /// What remains for success-path waits: zero from [`WAITS`] after the
    /// start on, every time. It never panics; a wait handed zero takes only
    /// an already-arrived result, then runs its own timeout branch.
    pub(crate) fn left(&self) -> Duration {
        (self.start + WAITS).saturating_duration_since(self.clock.now())
    }

    /// What remains for cleanup waits (a reap or group check after a wait
    /// expired, a watchdog's stand-down): zero from [`CLEANUP`] on.
    pub(crate) fn cleanup(&self) -> Duration {
        (self.start + CLEANUP).saturating_duration_since(self.clock.now())
    }
}

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
pub(crate) fn bounded<T: Send + 'static>(
    deadline: Deadline,
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    fakes::within(what, deadline.left(), work)
}

/// `fakes::kill_group` bounded by `deadline.cleanup()`: it sends a signal,
/// it does not wait on the code under test.
pub(crate) fn kill_group(deadline: Deadline, group: u32, signal: &'static str) -> io::Result<bool> {
    fakes::within(
        &format!("kill -{signal} of process group {group}"),
        deadline.cleanup(),
        move || fakes::kill_group(group, signal),
    )
}

/// `fakes::kill_pid` bounded by `deadline.cleanup()`.
pub(crate) fn kill_pid(deadline: Deadline, pid: u32, signal: &'static str) -> io::Result<bool> {
    fakes::within(
        &format!("kill -{signal} of pid {pid}"),
        deadline.cleanup(),
        move || fakes::kill_pid(pid, signal),
    )
}

/// `fakes::kill_matching` bounded by `deadline.cleanup()`.
pub(crate) fn kill_matching(deadline: Deadline, text: &str) -> io::Result<()> {
    let text = text.to_owned();
    fakes::within(
        &format!("killing every process matching {text}"),
        deadline.cleanup(),
        move || fakes::kill_matching(&text),
    )
}

/// Whether any process remains in process group `group`.
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
    let reaped = reap.recv_timeout(deadline.cleanup()).is_ok();
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

/// The process clock behind `contract::clock::Clock`.
pub(crate) struct SystemClock;

impl Clock for SystemClock {
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

    pub(crate) fn kill(&self, signal: &'static str) {
        kill_group(self.deadline, self.group, signal).unwrap();
    }

    /// Waits under the test's [`Deadline`] for a hub that exits on its own,
    /// then asserts nothing remains in its group.
    pub(crate) fn wait(self) -> ExitStatus {
        let Self {
            mut child,
            watchdog,
            group,
            deadline,
        } = self;
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match finished.recv_timeout(deadline.left()) {
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
        let status = match finished.recv_timeout(deadline.left()) {
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
    deadline: Deadline,
}

impl Socket {
    /// Connects to `path` on a thread bounded by the deadline.
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

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// [`connect_hub`] with the whole connect, the hub's start and the
/// `hub_hello` read included, bounded by `wait` on the wall clock. `connect`
/// blocks, so it runs on a thread whose result the test receives with the
/// deadline. A zero `wait` keeps the clock's scale at 1.
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
    (Socket::from(deadline, connected.0), hello)
}

impl Socket {
    pub(crate) fn from(deadline: Deadline, stream: UnixStream) -> Self {
        let read = stream.try_clone().unwrap();
        Self {
            write: Mutex::new(stream),
            read: Mutex::new(BufReader::new(read)),
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
    pub(crate) fn stand_down_watchdog(&mut self) {
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(self.deadline.cleanup());
        }
    }

    /// Called after the signal that every session in the workspace is ending
    /// (its socket closed, its session_left, or the guard's SIGKILL): waits
    /// under the test's [`Deadline`] for every process holding the workspace
    /// path to exit, then stands the guard down.
    pub(crate) fn wait_gone(mut self) {
        assert!(
            fakes::matching_exits(&self.workspace, self.deadline.left()),
            "waited until the deadline for every process holding the workspace path to exit"
        );
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
            match finished.recv_timeout(self.deadline.cleanup()) {
                Ok(_) | Err(_) => {}
            }
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

/// Runs `command`, which runs in its own process group, to its exit under
/// the test's [`Deadline`], naming `what` on expiry, and checks it left no
/// process in its group.
pub(crate) fn run_to_exit(deadline: Deadline, what: &str, mut command: Command) -> Output {
    let child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(deadline.left()) {
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
