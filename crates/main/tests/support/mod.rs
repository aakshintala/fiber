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
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]
use std::fs;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};

pub(crate) mod hold;
pub(crate) mod journey;
pub(crate) mod package;
pub(crate) mod pty;
pub(crate) mod repo_provider;
pub(crate) mod review_sweep;

#[allow(
    unused_imports,
    reason = "each test binary uses its own subset of the helpers"
)]
pub(crate) use fakes::wire::{
    function_call, hello, hello_inline_completed, hello_item_done, hello_single_delta, stream,
    text_reply,
};
#[allow(
    unused_imports,
    reason = "each test binary uses its own subset of the helpers"
)]
pub(crate) use fakes::write_record;

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

/// One line from `reader`, with its newline, bounded as a whole by one
/// wait started at entry. A full line already buffered is returned without
/// reading. Otherwise every read subdivides that wait, and at zero it
/// returns a `TimedOut` error naming `what` without reading. `Ok(None)` is
/// the end of the stream before any byte; a partial line at the end is a
/// line.
pub(crate) fn read_line(
    reader: &mut BufReader<UnixStream>,
    deadline: Deadline,
    what: &str,
) -> io::Result<Option<String>> {
    let wait = deadline.start_wait();
    read_line_through(reader, &wait, what)
}

/// One line from `reader` through one started wait's bound: a loop helper's
/// socket reads subdivide the wait it started at entry.
pub(crate) fn read_line_through(
    reader: &mut BufReader<UnixStream>,
    wait: &Deadline,
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
        let left = wait.left();
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
/// bounded by one wait started at entry. At zero it returns a `TimedOut`
/// error naming `what` without writing.
#[track_caller]
pub(crate) fn write_line(
    stream: &mut UnixStream,
    deadline: Deadline,
    bytes: &[u8],
    what: &str,
) -> io::Result<()> {
    let wait = deadline.start_wait();
    write_line_through(stream, &wait, bytes, what)
}

/// Writes all of `bytes` through an already-started wait, without renewing
/// its bound. At zero, it returns a `TimedOut` error naming `what` without
/// writing.
#[track_caller]
pub(crate) fn write_line_through(
    stream: &mut UnixStream,
    wait: &Deadline,
    bytes: &[u8],
    what: &str,
) -> io::Result<()> {
    let mut rest = bytes;
    while !rest.is_empty() {
        let left = wait.left();
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
    fakes::within(what, deadline, work)
}

/// `fakes::kill_matching` bounded by `deadline.cleanup()`.
#[track_caller]
pub(crate) fn kill_matching(deadline: Deadline, text: &str) -> io::Result<()> {
    let text = text.to_owned();
    fakes::within(
        &format!("killing every process matching {text}"),
        deadline.cleanup_phase(),
        move || fakes::kill_matching(&text),
    )
}

/// For `Drop` impls: kills every process whose command line holds
/// `text`, and its process group, on a thread that returns at once,
/// never waiting and never panicking. A thread that cannot start
/// sends nothing; the group's watchdog still kills it when the test
/// process exits.
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
    match fakes::kill_group(group, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let reaped = deadline.cleanup_phase().recv(reap).is_ok();
    assert!(
        fakes::group_empties(group, deadline.cleanup_phase()),
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
        Self::new_under(&std::env::temp_dir())
    }

    /// As [`Setup::new`], looking up the shared MCP fixture before the
    /// deadline starts: the lookup warms the script past its first-exec
    /// stall under its own bound, so the stall never eats the test's own
    /// wait margin. Tests that never start the fixture keep [`Setup::new`].
    pub(crate) fn new_with_mcp_fixture() -> Self {
        fakes::mcp_fixture();
        Self::new()
    }

    /// As [`Setup::new`], rooted under `parent` for a short socket path.
    pub(crate) fn new_under(parent: &Path) -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::under(parent, "fh");
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
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
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

    /// As [`Setup::provider`], with the model's data setting
    /// `deferred_tools` as `deferred` says: the deferral tests run the
    /// provider with deferral on, and the no-deferral case with it off.
    /// Existing callers of [`Setup::provider`] are unchanged.
    pub(crate) fn provider_with_deferral(&self, server: &ProviderServer, deferred: bool) {
        let mut model = json!({"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000});
        if deferred {
            model["deferred_tools"] = json!(true);
        }
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
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

/// A `hub.log` wait that wakes on a native file-change notification
/// (Linux inotify, macOS kqueue, through `rustix`): registered before
/// the event-producing command, it rereads the snapshot after
/// registration and after each signal, so an append landing between
/// registration and the first wait is never lost. It returns only after
/// `ready` holds for a fresh reread, or the deadline expires: no polling
/// interval or sleep is introduced.
pub(crate) struct HubLogWatch {
    log: PathBuf,
    watch: PlatformWatch,
}

impl HubLogWatch {
    /// Registers the native change watch on `log`: a reread after this
    /// returns sees every line appended later.
    pub(crate) fn register(log: &Path) -> io::Result<Self> {
        Ok(Self {
            log: log.to_path_buf(),
            watch: PlatformWatch::register(log)?,
        })
    }

    /// Blocks until `ready` holds for a reread of the log, or the wait
    /// started at entry expires: each wake rereads and re-parks under the
    /// wait's remainder, and expiry fails naming `what`.
    pub(crate) fn wait_until(
        &self,
        deadline: Deadline,
        what: &str,
        ready: impl Fn(&str) -> bool,
    ) -> io::Result<()> {
        let wait = deadline.start_wait();
        loop {
            let text = fs::read_to_string(&self.log).unwrap_or_default();
            if ready(&text) {
                return Ok(());
            }
            let left = wait.left();
            if left.is_zero() {
                return Err(timed_out(what));
            }
            self.watch.wait_once(left)?;
        }
    }
}

#[cfg(target_os = "linux")]
struct PlatformWatch {
    notify: rustix::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
impl PlatformWatch {
    fn register(log: &Path) -> io::Result<Self> {
        let notify = rustix::fs::inotify::init(
            rustix::fs::inotify::CreateFlags::CLOEXEC | rustix::fs::inotify::CreateFlags::NONBLOCK,
        )?;
        rustix::fs::inotify::add_watch(&notify, log, rustix::fs::inotify::WatchFlags::MODIFY)?;
        Ok(Self { notify })
    }

    fn wait_once(&self, timeout: Duration) -> io::Result<()> {
        let mut fds = [rustix::event::PollFd::new(
            &self.notify,
            rustix::event::PollFlags::IN,
        )];
        let spec = rustix::event::Timespec {
            tv_sec: timeout.as_secs().min(i64::MAX as u64).cast_signed(),
            tv_nsec: timeout.subsec_nanos() as _,
        };
        // The park itself is the wait: a woken fd, a signal and an
        // expired wait all end it, and the reread decides, so the count
        // is unused.
        #[allow(
            clippy::let_underscore_must_use,
            reason = "a woken fd and an expired wait both end the park: the count is unused"
        )]
        let _ = rustix::event::poll(&mut fds, Some(&spec));
        // Drain the queued change reports so the next wait parks instead
        // of spinning on a still-readable descriptor.
        let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 512];
        let mut reader = rustix::fs::inotify::Reader::new(&self.notify, &mut buf);
        loop {
            match reader.next() {
                Ok(_) => {}
                Err(error) if error == rustix::io::Errno::AGAIN => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
struct PlatformWatch {
    queue: rustix::fd::OwnedFd,
    file: rustix::fd::OwnedFd,
}

#[cfg(target_os = "macos")]
impl PlatformWatch {
    fn register(log: &Path) -> io::Result<Self> {
        use rustix::event::kqueue::kqueue;
        let file = rustix::fs::open(
            log,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        let queue = kqueue()?;
        // The first wait registers and parks atomically, so no change
        // between registration and the first wait is lost.
        Ok(Self { queue, file })
    }

    // Mutants run on Linux, where this is not compiled; the macOS test job runs it.
    #[cfg_attr(false, mutants::skip)]
    #[allow(
        unsafe_code,
        reason = "kqueue waits for hub.log writes on macOS: the registered vnode fd is the watch's own open file, alive for every wait, and the event buffer is a stack array the kernel writes only within its length"
    )]
    fn wait_once(&self, timeout: Duration) -> io::Result<()> {
        use rustix::event::kqueue::{Event, EventFilter, EventFlags, VnodeEvents, kevent};
        let change = Event::new(
            EventFilter::Vnode {
                vnode: std::os::unix::io::AsRawFd::as_raw_fd(&self.file),
                flags: VnodeEvents::WRITE,
            },
            EventFlags::ADD | EventFlags::CLEAR,
            std::ptr::null_mut(),
        );
        let mut events = [std::mem::MaybeUninit::<Event>::uninit(); 1];
        // SAFETY: `self.file` stays open for the watch's lifetime, so the
        // registered vnode fd is valid for every wait; `events` is a stack
        // array the kernel fills with at most one event, and the null
        // udata is never dereferenced.
        let _ = unsafe {
            kevent(
                &self.queue,
                std::slice::from_ref(&change),
                &mut events,
                Some(timeout),
            )
        }?;
        Ok(())
    }
}

/// A finished `fiber` run whose stdout is one JSON line per line: every
/// binary-level `ask` test reads the same shape, so the event kinds and
/// the session id are shared here (`docs/testing.md`, "Levels").
#[derive(Debug)]
pub(crate) struct Run {
    pub(crate) code: Option<i32>,
    pub(crate) lines: Vec<Value>,
    pub(crate) stderr: String,
}

impl Run {
    pub(crate) fn session_id(&self) -> &str {
        self.lines[0]["session_id"].as_str().unwrap()
    }

    pub(crate) fn kinds(&self) -> Vec<&str> {
        kinds(&self.lines)
    }

    pub(crate) fn first(&self, kind: &str) -> &Value {
        self.lines
            .iter()
            .find(|line| line["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} line"))
    }

    pub(crate) fn all(&self, kind: &str) -> Vec<&Value> {
        self.lines
            .iter()
            .filter(|line| line["kind"] == kind)
            .collect()
    }
}

/// A finished `fiber` run that keeps stdout's text alongside its parsed
/// lines: every binary-level test that pins stdout text while reading
/// parsed events shares this shape (`docs/testing.md`, "Levels").
/// `session_status` lines are dropped; file-specific readers live in local
/// `impl` blocks.
#[derive(Debug)]
pub(crate) struct StdoutRun {
    pub(crate) run: Run,
    pub(crate) stdout: String,
}

impl std::ops::Deref for StdoutRun {
    type Target = Run;

    fn deref(&self) -> &Self::Target {
        &self.run
    }
}

impl From<std::process::Output> for StdoutRun {
    fn from(output: std::process::Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            run: Run {
                code: output.status.code(),
                lines,
                stderr: String::from_utf8(output.stderr).unwrap(),
            },
            stdout,
        }
    }
}

/// A finished `fiber` run with stdout and stderr as text: every
/// binary-level text test reads the same shape, so the exit code and the
/// two streams are shared here (`docs/testing.md`, "Levels").
pub(crate) struct Output {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl From<std::process::Output> for Output {
    fn from(output: std::process::Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

#[test]
fn output_from_a_zero_exit_has_empty_text() {
    use std::os::unix::process::ExitStatusExt;
    let output = Output::from(std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    });
    assert_eq!(output.code, Some(0));
    assert_eq!(output.stdout, "");
    assert_eq!(output.stderr, "");
}

pub(crate) fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Every regular file under `roots` holding `needle`, sorted: the
/// secret-output invariant's probe (`import.rs`, `mcp_commands.rs`). A
/// root that cannot be read holds nothing, and symlinked directories are
/// not descended into, so a linked tree cannot loop the walk.
pub(crate) fn files_holding(roots: &[PathBuf], needle: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs: Vec<PathBuf> = roots.to_vec();
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !path.is_symlink() {
                    dirs.push(path);
                }
            } else if path.is_file()
                && fs::read(&path)
                    .map(|bytes| {
                        needle.is_empty()
                            || bytes.windows(needle.len()).any(|window| window == needle)
                    })
                    .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The hub stderr a [`HubProc::spawn_command_captured`] drain thread keeps:
/// the live text plus the drain's EOF completion, so a teardown read waits
/// for the drain instead of racing it. Rides beside `HubProc`, so no struct
/// field is added and every existing construction and destructure stays
/// unchanged.
pub(crate) struct CapturedStderr {
    snapshot: Arc<Mutex<String>>,
    drained: mpsc::Receiver<()>,
}

impl CapturedStderr {
    /// An already-drained capture holding `text`: for callers with no drain
    /// thread, whose snapshot is complete by construction.
    pub(crate) fn drained_text(text: String) -> Self {
        let (done_tx, drained) = mpsc::channel();
        done_tx.send(()).unwrap_or(());
        Self {
            snapshot: Arc::new(Mutex::new(text)),
            drained,
        }
    }

    /// The drained text: waits for the drain thread's EOF under `bound`,
    /// then clones the complete snapshot, yielding what was captured even
    /// when the lock is poisoned. On a missed bound clones what arrived
    /// with an incompleteness marker instead of replacing the caller's
    /// panic: EOF is already available (every write end is closed once the
    /// child is reaped), so only thread scheduling is outstanding.
    pub(crate) fn text(&self, bound: Deadline) -> String {
        let complete = bound.recv(&self.drained).is_ok();
        let mut text = self
            .snapshot
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
        if !complete {
            text.push_str("(stderr drain incomplete: showing what arrived)\n");
        }
        text
    }
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

    /// Spawns the `hub serve` command `serve` like
    /// [`spawn_command`](Self::spawn_command), keeping its piped stderr: a
    /// drain thread appends every chunk to the returned capture as it
    /// arrives, plus the first read error as its own trailing marker, then
    /// signals EOF and exits with no join. The capture rides beside
    /// `HubProc`, so no struct field is added and every existing
    /// construction and destructure stays unchanged.
    pub(crate) fn spawn_command_captured(
        deadline: Deadline,
        serve: &mut Command,
    ) -> (Self, CapturedStderr) {
        let mut child = serve.spawn().unwrap();
        let group = child.id();
        let _ = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let snapshot = Arc::new(Mutex::new(String::new()));
        let (done_tx, drained) = mpsc::channel();
        if let Some(mut pipe) = stderr_pipe {
            let copy = Arc::clone(&snapshot);
            thread::spawn(move || {
                let (_, error) = drain_stderr_to_snapshot_reported(&mut pipe, &copy);
                if let Some(error) = error {
                    let marker = format!("hub stderr drain error: {error}\n");
                    match copy.lock() {
                        Ok(mut guard) => guard.push_str(&marker),
                        Err(poisoned) => poisoned.into_inner().push_str(&marker),
                    }
                }
                done_tx.send(()).unwrap_or(());
            });
        } else {
            done_tx.send(()).unwrap_or(());
        }
        let watchdog = Watchdog::group(group);
        (
            Self {
                child,
                watchdog,
                group,
                deadline,
            },
            CapturedStderr { snapshot, drained },
        )
    }

    #[track_caller]
    pub(crate) fn kill(&self, signal: &'static str) {
        fakes::kill_group(self.group, signal).unwrap();
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
            !fakes::group_alive(group),
            "the hub left a process in its group behind"
        );
        watchdog.stand_down(deadline.cleanup_phase());
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
        fakes::kill_group(group, "KILL").unwrap();
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
        watchdog.stand_down(deadline.cleanup_phase());
        status
    }
}

/// `fiber ask` with its stdout kept drained and its stderr kept for a failure.
pub(crate) struct Running {
    pub(crate) child: Child,
    pub(crate) watchdog: Watchdog,
    pub(crate) group: u32,
    pub(crate) stdout: mpsc::Receiver<String>,
    pub(crate) stderr: Arc<Mutex<String>>,
    pub(crate) deadline: Deadline,
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
    /// Set by [`Socket::pumped`]: the lines a reader thread took off the
    /// socket, `None` once the far end closed it.
    pump: Option<Mutex<mpsc::Receiver<Option<Value>>>>,
    /// Context for the timeout dump, set once after connect at the sites
    /// that hold a session's stderr snapshot and log directory.
    diag: Option<Diag>,
    deadline: Deadline,
}

/// What a session-socket wait prints when the deadline expires: the live
/// stderr snapshot the drain thread keeps, and the directory holding the
/// session's `events.jsonl`. The snapshot rides an `Arc` so the drain
/// thread appends while the wait reads.
pub(crate) struct Diag {
    stderr: Arc<Mutex<String>>,
    log_dir: PathBuf,
}

impl Diag {
    pub(crate) fn new(stderr: Arc<Mutex<String>>, log_dir: PathBuf) -> Self {
        Self { stderr, log_dir }
    }

    /// The timeout message for `what` with the lines `got` so far: the old
    /// first line, then the stderr snapshot, then the session-log tail. The
    /// stderr lock is released before the log file is read, and a poisoned
    /// lock yields what was captured, so the dump never replaces the
    /// timeout panic with its own.
    pub(crate) fn report(&self, what: &str, got: &[Value]) -> String {
        let stderr = self
            .stderr
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
        timeout_message(what, got, &stderr, &session_log_tail(&self.log_dir))
    }

    /// The closed-socket message for `what` with the lines `got` so far:
    /// the old first line, then the stderr snapshot and the session-log
    /// tail, with the received-lines dump last so a long dump cannot push
    /// the diagnostics past the CI output cap. The stderr lock is released
    /// before the log file is read, and a poisoned lock yields what was
    /// captured, so the dump never replaces the closed panic with its own.
    pub(crate) fn report_closed(&self, what: &str, got: &[Value]) -> String {
        let stderr = self
            .stderr
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
        closed_message(what, got, &stderr, &session_log_tail(&self.log_dir))
    }
}

/// The session-log lines a timeout dump shows: the last 20 complete lines
/// of `dir/events.jsonl`, mirroring the hub-start report's tail. A trailing
/// fragment without its newline is still being written, so it is dropped;
/// a missing log reads as empty.
pub(crate) fn session_log_tail(dir: &Path) -> String {
    const TAIL: usize = 20;
    let text = fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    let complete = text.rfind('\n').map_or("", |end| &text[..=end]);
    let lines: Vec<&str> = complete.lines().collect();
    let start = lines.len().saturating_sub(TAIL);
    lines[start..].join("\n")
}

/// The session-log tails a failure report shows: one `session <name>:`
/// block per session directory under `sessions`, sorted by name, each
/// holding that session's [`session_log_tail`]. A missing or unreadable
/// directory reads as empty, and a stray non-directory entry is skipped;
/// an empty block means "unreadable or absent", never evidence of an
/// absent job. It never panics, so it is safe on a failing run's path.
pub(crate) fn session_logs_tail(sessions: &Path) -> String {
    let Ok(entries) = fs::read_dir(sessions) else {
        return String::new();
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    names
        .iter()
        .map(|name| {
            format!(
                "session {name}:\n{}",
                session_log_tail(&sessions.join(name))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Asserts a session-socket timeout dump: the received lines before the
/// stderr section, the stderr snapshot section, then the session-log tail
/// holding a `session_started` line. The three acceptance tests share it.
#[track_caller]
pub(crate) fn assert_timeout_dump(message: &str) {
    let head = message.split("fiber stderr:").next().unwrap_or_default();
    assert!(head.contains("command_accepted"), "{message}");
    assert!(message.contains("fiber stderr:"), "{message}");
    let tail_header = "session log tail (events.jsonl, last 20 lines):";
    assert!(message.contains(tail_header), "{message}");
    let tail = message.split(tail_header).nth(1).unwrap_or_default();
    assert!(tail.contains("\"kind\":\"session_started\""), "{message}");
}

/// The timeout message: the old first line naming `what` and `got`, then
/// the stderr snapshot (or the still-running marker when nothing was
/// captured yet), then the session-log tail.
pub(crate) fn timeout_message(what: &str, got: &[Value], stderr: &str, log_tail: &str) -> String {
    let stderr = if stderr.is_empty() {
        "(still running: no stderr captured yet)"
    } else {
        stderr
    };
    format!(
        "waited until the deadline for {what}; got {got:?}\n\
         fiber stderr:\n\
         {stderr}\n\
         session log tail (events.jsonl, last 20 lines):\n\
         {log_tail}"
    )
}

/// The closed-socket message: the old first line naming `what`, then the
/// stderr snapshot (or the captured-nothing marker when it is empty),
/// then the session-log tail, with the received-lines dump last so a long
/// dump cannot push the diagnostics past the CI output cap. The empty
/// marker never says the session is still running: the socket already
/// closed.
pub(crate) fn closed_message(what: &str, got: &[Value], stderr: &str, log_tail: &str) -> String {
    let stderr = if stderr.is_empty() {
        "(no stderr captured)"
    } else {
        stderr
    };
    format!(
        "the socket closed while waiting for {what}\n\
         fiber stderr:\n\
         {stderr}\n\
         session log tail (events.jsonl, last 20 lines):\n\
         {log_tail}\n\
         got {got:?}"
    )
}

/// Reads `pipe` to its end like [`drain_stderr_to_snapshot`], also
/// returning the first read error's text: the hub-hello capture prints it
/// as its own section, while existing callers keep the wrapper that drops
/// it. The text and the snapshot hold exactly what the wrapper would.
pub(crate) fn drain_stderr_to_snapshot_reported(
    pipe: &mut impl io::Read,
    snapshot: &Arc<Mutex<String>>,
) -> (String, Option<String>) {
    let mut text = String::new();
    let mut first_error: Option<String> = None;
    let mut chunk = [0_u8; 4096];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                pending.extend_from_slice(&chunk[..read]);
                let piece = take_decoded_prefix(&mut pending);
                text.push_str(&piece);
                match snapshot.lock() {
                    Ok(mut guard) => guard.push_str(&piece),
                    Err(poisoned) => poisoned.into_inner().push_str(&piece),
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error.to_string());
                }
                break;
            }
        }
    }
    if !pending.is_empty() {
        let piece = String::from_utf8_lossy(&pending);
        text.push_str(&piece);
        match snapshot.lock() {
            Ok(mut guard) => guard.push_str(&piece),
            Err(poisoned) => poisoned.into_inner().push_str(&piece),
        }
    }
    (text, first_error)
}

/// Reads `pipe` to its end, appending every chunk to `snapshot` as it
/// arrives while returning the full text: the timeout dump sees stderr
/// written before the timeout, and the caller still gets the whole stream
/// once at EOF. A multibyte character split across two reads is kept whole:
/// trailing bytes that do not yet decode wait for the next read, so neither
/// the text nor the snapshot ever holds a replacement character for one.
/// An unreadable or empty stream snapshots as empty.
pub(crate) fn drain_stderr_to_snapshot(
    pipe: &mut impl io::Read,
    snapshot: &Arc<Mutex<String>>,
) -> String {
    drain_stderr_to_snapshot_reported(pipe, snapshot).0
}

/// Moves every complete character out of `pending`, leaving only trailing
/// bytes that may complete a character once more arrive: the valid prefix,
/// with each invalid byte replaced. A trailing fragment with no error
/// length is an incomplete character, so it stays for the next read.
fn take_decoded_prefix(pending: &mut Vec<u8>) -> String {
    let mut piece = String::new();
    loop {
        match str::from_utf8(pending) {
            Ok(valid) => {
                piece.push_str(valid);
                pending.clear();
                return piece;
            }
            Err(error) => {
                let up_to = error.valid_up_to();
                if up_to > 0 {
                    // `valid_up_to` is always a character boundary.
                    let valid = str::from_utf8(&pending[..up_to]).unwrap_or_default();
                    piece.push_str(valid);
                    pending.drain(..up_to);
                    continue;
                }
                match error.error_len() {
                    Some(len) => {
                        piece.push('\u{FFFD}');
                        pending.drain(..len);
                    }
                    None => return piece,
                }
            }
        }
    }
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

    /// Writes one socket line through an already-started wait without
    /// renewing its bound.
    #[track_caller]
    pub(crate) fn send_through(&self, wait: &Deadline, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        if !line.ends_with('\n') {
            bytes.push(b'\n');
        }
        let mut write = self.write.lock().unwrap();
        if let Err(error) = write_line_through(&mut write, wait, &bytes, "sending a line") {
            panic!("writing the socket: {error}");
        }
    }

    /// One socket line: a line, or `None` when the far end closed the
    /// socket. Neither by the deadline panics naming `what`, with the lines
    /// before it.
    pub(crate) fn next(&self, what: &str, got: &[Value]) -> Option<Value> {
        self.next_through(&self.deadline, what, got)
    }

    /// Fixes one wait's bound now on this socket's deadline: a loop helper
    /// takes every line of its wait through the result.
    pub(crate) fn start_wait(&self) -> Deadline {
        self.deadline.start_wait()
    }

    /// One socket line through one started wait's bound instead of the
    /// root's: a loop helper takes every line of its wait through the wait
    /// it started at entry.
    pub(crate) fn next_through(&self, wait: &Deadline, what: &str, got: &[Value]) -> Option<Value> {
        if let Some(line) = self.kept.lock().unwrap().pop_front() {
            return Some(line);
        }
        if let Some(pump) = &self.pump {
            return match wait.recv(&pump.lock().unwrap()) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => match &self.diag {
                    Some(diag) => panic!("{}", diag.report(what, got)),
                    None => panic!("waited until the deadline for {what}; got {got:?}"),
                },
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            };
        }
        match read_line_through(&mut self.read.lock().unwrap(), wait, what) {
            Ok(None) => None,
            Ok(Some(buf)) => {
                let line = buf.trim_end_matches(&['\r', '\n'][..]).to_owned();
                Some(serde_json::from_str(&line).unwrap_or(Value::String(line)))
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => match &self.diag {
                Some(diag) => panic!("{}", diag.report(what, got)),
                None => panic!("waited until the deadline for {what}; got {got:?}"),
            },
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

/// The answer to command `id`: the line whose `payload.command_id` is `id`.
/// `docs/invocation.md` orders no other line against it, so a feed line may
/// come first: lines before it stay for the caller's next [`recv`] or
/// [`until`], in order, except `attention` lines, which are dropped too.
#[track_caller]
pub(crate) fn recv_answer(client: &Socket, id: &str, what: &str) -> Value {
    let wait = client.deadline.start_wait();
    let mut passed = Vec::new();
    let answer = loop {
        match client.next_through(&wait, what, &passed) {
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

/// Collects socket lines until `done`, every line through one wait
/// started at entry: expiry panics naming `what`, and the socket closing
/// first panics too. To wait for a live line written after a subscription,
/// skipping the fold's replay, use [`until_after`]; [`until_replayed`]
/// waits for a replayed line instead.
pub(crate) fn until(
    client: &Socket,
    what: &str,
    mut done: impl FnMut(&Value) -> bool,
) -> Vec<Value> {
    let wait = client.deadline.start_wait();
    let mut lines = Vec::new();
    loop {
        let line = match client.next_through(&wait, what, &lines) {
            Some(line) => line,
            None => match &client.diag {
                Some(diag) => panic!("{}", diag.report_closed(what, &lines)),
                None => panic!("the socket closed while waiting for {what}; got {lines:?}"),
            },
        };
        let stop = done(&line);
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

/// Collects socket lines until the far end closes the socket, every line
/// through one wait started at entry.
pub(crate) fn until_close(client: &Socket) -> Vec<Value> {
    let wait = client.deadline.start_wait();
    let mut lines = Vec::new();
    while let Some(line) = client.next_through(&wait, "the socket to close", &lines) {
        lines.push(line);
    }
    lines
}

/// A `full` subscription and the bound its acknowledgement carries: a wait
/// for a live event starts after this `last_seq`, and an answer is matched
/// by its command id (`docs/invocation.md`, "What each command does").
pub(crate) struct Subscribed {
    /// The `command_accepted` acknowledgement.
    pub(crate) ack: Value,
    /// `ack["payload"]["result"]["last_seq"]` as `u64`: the `seq` of the
    /// last line the fold sends before the stream goes live, `None` when
    /// `result` is absent on an empty log.
    pub(crate) last_seq: Option<u64>,
    /// The `session_id` the subscribe named, `None` on a direct socket.
    pub(crate) session: Option<String>,
}

/// Subscribes `full` to `session` through the hub.
#[track_caller]
pub(crate) fn subscribe(client: &Socket, session: &str) -> Subscribed {
    subscribe_as(client, "c_sub", session)
}

/// Subscribes `full` to `session` through the hub under the caller's
/// command id.
#[track_caller]
pub(crate) fn subscribe_as(client: &Socket, id: &str, session: &str) -> Subscribed {
    subscribe_through(client, id, Some(session))
}

/// Subscribes `full` on a session's own socket, which takes no
/// `session_id`.
#[track_caller]
pub(crate) fn subscribe_direct(client: &Socket) -> Subscribed {
    subscribe_through(client, "c_sub", None)
}

/// Sends the subscribe line with `id`, naming `session` through the hub
/// and none on a direct socket, and takes the acknowledgement: lines
/// before it stay for the caller's next [`recv`] or [`until`] in order
/// ([`recv_answer`]'s contract).
fn subscribe_through(client: &Socket, id: &str, session: Option<&str>) -> Subscribed {
    match session {
        Some(session) => client.send(&format!(
            "{{\"id\":\"{id}\",\"session_id\":\"{session}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
        )),
        None => client.send(&format!(
            "{{\"id\":\"{id}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
        )),
    }
    let ack = recv_answer(client, id, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    accepted(ack, session.map(str::to_owned))
}

/// The subscription an accepted acknowledgement carries.
fn accepted(ack: Value, session: Option<String>) -> Subscribed {
    let last_seq = ack["payload"]["result"]["last_seq"].as_u64();
    Subscribed {
        ack,
        last_seq,
        session,
    }
}

/// Subscribes `full` on a fake session socket, waiting for the
/// acknowledgement under the caller's deadline alone.
#[track_caller]
pub(crate) fn subscribe_client(deadline: Deadline, client: &fakes::Client) -> Subscribed {
    if let Err(error) = client.send_by(
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
        &|| deadline.left(),
    ) {
        panic!("writing the socket: {error}");
    }
    let Some(ack) = client.recv_until(deadline, |line| {
        line.get("kind").and_then(Value::as_str) == Some("command_accepted")
            && line["payload"]["command_id"] == "c_sub"
    }) else {
        panic!("no subscribe acknowledgement arrived before the deadline");
    };
    accepted(ack, None)
}

/// Collects socket lines until `done`, every line through one wait started
/// at entry, like [`until`]: expiry and the socket closing panic as
/// [`until`] does. A line the fold replayed never reaches `done`: only a
/// line written after the subscription's [`Subscribed::last_seq`] for the
/// subscribed session does. Lines without `seq` reach `done` unchanged, as
/// do lines of another session on a hub connection. Every line read is
/// returned, replayed ones included.
#[track_caller]
pub(crate) fn until_after(
    client: &Socket,
    since: &Subscribed,
    what: &str,
    done: impl FnMut(&Value) -> bool,
) -> Vec<Value> {
    until_matching(client, since, what, done, false)
}

/// Collects socket lines until `done`, every line through one wait started
/// at entry, like [`until_after`], but only a line the fold replayed
/// reaches `done`: a line carrying a `seq` at or below the subscription's
/// [`Subscribed::last_seq`] for the subscribed session.
#[track_caller]
pub(crate) fn until_replayed(
    client: &Socket,
    since: &Subscribed,
    what: &str,
    done: impl FnMut(&Value) -> bool,
) -> Vec<Value> {
    until_matching(client, since, what, done, true)
}

/// Collects socket lines through one wait started at entry, like [`until`]:
/// expiry and the socket closing panic as [`until`] does. When `replayed`
/// only a replayed line reaches `done`, else only a live one; every line
/// read is returned either way, ending on the first line `done` accepts.
fn until_matching(
    client: &Socket,
    since: &Subscribed,
    what: &str,
    mut done: impl FnMut(&Value) -> bool,
    replayed: bool,
) -> Vec<Value> {
    let wait = client.start_wait();
    let mut lines = Vec::new();
    loop {
        let line = match client.next_through(&wait, what, &lines) {
            Some(line) => line,
            None => match &client.diag {
                Some(diag) => panic!("{}", diag.report_closed(what, &lines)),
                None => panic!("the socket closed while waiting for {what}; got {lines:?}"),
            },
        };
        if is_reopened(&line, since) {
            let session = since.session.as_deref().unwrap_or("the session");
            panic!(
                "read stream_reopened for {session} while waiting for {what}; \
                 the new fold restarts seq, so take a new Subscribed"
            );
        }
        let stop = if is_replayed(&line, since) == replayed {
            done(&line)
        } else {
            false
        };
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

/// Whether `line` is a line the fold replayed: it carries a `seq` at or
/// below the subscription's bound, for the subscribed session. Without a
/// bound nothing is replayed; a line without `seq` is ephemeral
/// (`docs/events.md`, "Durable and ephemeral") and never replayed. A
/// subscription naming no session matches every line.
fn is_replayed(line: &Value, since: &Subscribed) -> bool {
    let Some(bound) = since.last_seq else {
        return false;
    };
    let Some(seq) = line.get("seq").and_then(Value::as_u64) else {
        return false;
    };
    if seq > bound {
        return false;
    }
    match &since.session {
        None => true,
        Some(session) => line.get("session_id").and_then(Value::as_str) == Some(session),
    }
}

/// Whether `line` reopens the subscribed session with a new fold: a
/// `stream_reopened` carries no `seq`, and its `payload` holds the
/// `session_id` the new fold belongs to. A subscription naming no session
/// matches every reopening.
fn is_reopened(line: &Value, since: &Subscribed) -> bool {
    line.get("kind").and_then(Value::as_str) == Some("stream_reopened")
        && match &since.session {
            None => true,
            Some(session) => line["payload"]["session_id"].as_str() == Some(session.as_str()),
        }
}

/// A peer that reads the subscribe line, reports it, then writes `lines`:
/// every assertion on the reported line stays on the test thread, where a
/// failure fails the test.
fn subscribe_peer(lines: Vec<&'static str>) -> (Socket, mpsc::Receiver<String>) {
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let (mine, peer) = UnixStream::pair().unwrap();
    let (sent_tx, sent) = mpsc::channel();
    thread::spawn(move || {
        let mut peer = peer;
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let line = read_line(&mut reader, deadline, "the subscribe line")
            .unwrap()
            .unwrap();
        sent_tx.send(line).unwrap_or(());
        for line in lines {
            write_line(&mut peer, deadline, line.as_bytes(), "the fixed lines").unwrap();
        }
    });
    (Socket::from(deadline, mine), sent)
}

/// A peer that writes `lines` without reading: for waits, where the test
/// sends nothing first.
fn replay_peer(lines: Vec<&'static str>) -> Socket {
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let (mine, peer) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        let mut peer = peer;
        for line in lines {
            write_line(&mut peer, deadline, line.as_bytes(), "the fixed lines").unwrap();
        }
    });
    Socket::from(deadline, mine)
}

fn subscribed_since(last_seq: Option<u64>, session: Option<&str>) -> Subscribed {
    Subscribed {
        ack: json!({}),
        last_seq,
        session: session.map(str::to_owned),
    }
}

#[test]
fn subscribe_direct_returns_last_seq_and_keeps_the_replay() {
    let (client, sent) = subscribe_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_sub\",\"result\":{\"last_seq\":7}}}\n",
    ]);
    let sub = subscribe_direct(&client);
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_sub", "{sent}");
    assert_eq!(sent["command"], "subscribe", "{sent}");
    assert!(sent.get("session_id").is_none(), "{sent}");
    assert_eq!(sub.last_seq, Some(7));
    assert!(sub.session.is_none());
    assert_eq!(
        recv(&client, "the replayed exit")
            .get("seq")
            .and_then(Value::as_u64),
        Some(5)
    );
    assert_eq!(
        recv(&client, "the replayed exit")
            .get("seq")
            .and_then(Value::as_u64),
        Some(6)
    );
}

#[test]
fn subscribe_direct_returns_no_bound_on_an_empty_log() {
    // The peer reads the subscribe line before answering, so it is still
    // alive when the send takes the socket timeout: a peer that only
    // writes can exit first, and on macOS the send then fails with
    // `EINVAL` instead of reaching the acknowledgement.
    let (client, sent) = subscribe_peer(vec![
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_sub\"}}\n",
    ]);
    let sub = subscribe_direct(&client);
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_sub", "{sent}");
    assert_eq!(sub.last_seq, None);
}

#[test]
fn subscribe_direct_panics_on_a_rejected_subscribe() {
    // As above: the peer reads first, so the send cannot race its exit.
    let (client, _sent) = subscribe_peer(vec![
        "{\"kind\":\"command_rejected\",\"payload\":{\"command_id\":\"c_sub\",\"code\":\"gone\"}}\n",
    ]);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        subscribe_direct(&client);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("command_rejected"), "{message}");
}

#[test]
fn subscribe_names_the_session_and_returns_its_last_seq() {
    let (client, sent) = subscribe_peer(vec![
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_sub\",\"result\":{\"last_seq\":3}}}\n",
    ]);
    let sub = subscribe(&client, "s_9");
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_sub", "{sent}");
    assert_eq!(sent["session_id"], "s_9", "{sent}");
    assert_eq!(sub.last_seq, Some(3));
    assert_eq!(sub.session, Some("s_9".to_owned()));
}

#[test]
fn subscribe_as_sends_the_caller_command_id() {
    let (client, sent) = subscribe_peer(vec![
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_watch\"}}\n",
    ]);
    let sub = subscribe_as(&client, "c_watch", "s_2");
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_watch", "{sent}");
    assert_eq!(sent["session_id"], "s_2", "{sent}");
    assert_eq!(sub.last_seq, None);
    assert_eq!(sub.session, Some("s_2".to_owned()));
}

#[test]
fn subscribe_client_matches_the_acknowledgement_by_command_id() {
    let dir = fakes::TempDir::new("subscribe-client");
    let path = dir.path().join("session.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let (sent_tx, sent) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let line = read_line(&mut reader, deadline, "the subscribe line")
            .unwrap()
            .unwrap();
        sent_tx.send(line).unwrap_or(());
        let mut stream = stream;
        // A decoy answer for another command, then a decoy rejection of
        // this one: neither is the acknowledgement.
        for line in [
            "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_9\"}}\n",
            "{\"kind\":\"command_rejected\",\"payload\":{\"command_id\":\"c_sub\"}}\n",
            "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_sub\",\"result\":{\"last_seq\":11}}}\n",
        ] {
            write_line(&mut stream, deadline, line.as_bytes(), "the answers").unwrap();
        }
    });
    let client = fakes::Client::connect(&path).unwrap();
    let sub = subscribe_client(deadline, &client);
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_sub", "{sent}");
    assert!(sent.get("session_id").is_none(), "{sent}");
    assert_eq!(sub.last_seq, Some(11));
    assert!(sub.session.is_none());
}

#[test]
fn until_after_skips_the_replay_and_ends_on_the_live_line() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":4}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after(&client, &since, "the live exit", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("seq").and_then(Value::as_u64));
        line.get("seq").and_then(Value::as_u64) == Some(6)
    });
    assert_eq!(*seen.lock().unwrap(), [Some(6)]);
    assert_eq!(
        lines
            .iter()
            .map(|line| line.get("seq").and_then(Value::as_u64))
            .collect::<Vec<_>>(),
        [Some(4), Some(5), Some(6)]
    );
}

#[test]
fn until_after_bounds_nothing_on_an_empty_log() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":1}\n",
    ]);
    let since = subscribed_since(None, Some("s_1"));
    let lines = until_after(&client, &since, "the first exit", |_| true);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("seq").and_then(Value::as_u64), Some(1));
}

#[test]
fn until_after_passes_ephemeral_and_other_session_lines() {
    let client = replay_peer(vec![
        "{\"kind\":\"notice\",\"payload\":{\"message\":\"hi\"}}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_2\",\"seq\":2}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after(&client, &since, "the pass-through lines", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("kind").and_then(Value::as_str).unwrap().to_owned());
        seen.lock().unwrap().len() == 2
    });
    assert_eq!(*seen.lock().unwrap(), ["notice", "fiber_exited"]);
    assert_eq!(lines.len(), 2);
}

#[test]
fn until_after_returns_every_line_read() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":4}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let lines = until_after(&client, &since, "the live exit", |line| {
        line.get("seq").and_then(Value::as_u64) == Some(6)
    });
    assert_eq!(
        lines,
        vec![
            json!({"kind":"fiber_exited","session_id":"s_1","seq":4}),
            json!({"kind":"fiber_exited","session_id":"s_1","seq":5}),
            json!({"kind":"fiber_exited","session_id":"s_1","seq":6}),
        ]
    );
}

#[test]
fn until_replayed_passes_only_the_replay() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let lines = until_replayed(&client, &since, "the replayed exit", |line| {
        assert_ne!(
            line.get("seq").and_then(Value::as_u64),
            Some(6),
            "a live line never reaches done: {line}"
        );
        line.get("seq").and_then(Value::as_u64) == Some(5)
    });
    assert_eq!(
        lines
            .iter()
            .map(|line| line.get("seq").and_then(Value::as_u64))
            .collect::<Vec<_>>(),
        [Some(6), Some(5)]
    );
}

#[test]
fn until_after_names_the_wait_when_the_socket_closes() {
    let (mine, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let client = Socket::from(Deadline::after(fakes::MUST_SUCCEED_WITHIN), mine);
    let since = subscribed_since(Some(5), Some("s_1"));
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until_after(&client, &since, "the live exit", |_| true);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert_eq!(
        message,
        "the socket closed while waiting for the live exit; got []"
    );
}

#[test]
fn until_after_panics_on_a_new_fold_for_the_session() {
    let client = replay_peer(vec![
        "{\"kind\":\"stream_reopened\",\"payload\":{\"session_id\":\"s_1\",\"last_seq\":14}}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until_after(&client, &since, "the live exit", |_| true);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("new Subscribed"), "{message}");
}

#[test]
fn until_after_passes_a_new_fold_for_another_session() {
    let client = replay_peer(vec![
        "{\"kind\":\"stream_reopened\",\"payload\":{\"session_id\":\"s_2\",\"last_seq\":14}}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after(&client, &since, "the live exit", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("kind").and_then(Value::as_str).unwrap().to_owned());
        line.get("seq").and_then(Value::as_u64) == Some(6)
    });
    assert_eq!(*seen.lock().unwrap(), ["stream_reopened", "fiber_exited"]);
    assert_eq!(lines.len(), 2);
}

/// The `hub.log` lines a startup-failure report shows, matching the
/// installed-hub start helper's tail.
pub(crate) const HUB_LOG_TAIL: usize = 20;

/// The tail of `log`: its last [`HUB_LOG_TAIL`] lines, or the whole log
/// when shorter. An empty log stays empty.
pub(crate) fn hub_log_tail(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(HUB_LOG_TAIL)..].join("\n")
}

#[test]
fn hub_log_tail_shows_an_empty_log_empty() {
    assert_eq!(hub_log_tail(""), "");
}

#[test]
fn hub_log_tail_shows_a_short_log_whole() {
    assert_eq!(hub_log_tail("a\nb"), "a\nb");
}

// No asserted count cites a `docs/<area>.md` line promising it
// (`docs/code-quality.md`, "What a reviewer checks"): the tail's width is
// the helper's own bound, so these tests pin presence and recency, not the
// bound. A long log keeps its last line and drops an early one.
#[test]
fn hub_log_tail_keeps_the_last_line() {
    let log = (1..=25)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tail = hub_log_tail(&log);
    assert!(
        tail.lines().any(|line| line == "line 25"),
        "the tail keeps the log's last line: {tail}"
    );
}

#[test]
fn hub_log_tail_drops_an_early_line() {
    let log = (1..=25)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tail = hub_log_tail(&log);
    assert!(
        !tail.lines().any(|line| line == "line 1"),
        "the tail drops the log's early lines: {tail}"
    );
}

/// What a hub that did not start left behind: the child status a
/// `try_wait` snapshots (`None` for a hub still running or never spawned)
/// and the tail of `hub.log`. The snapshot neither waits on the child nor
/// kills it; the watchdog `HubProc::spawn_command` arms stays the teardown
/// owner.
fn hub_start_report(setup: &Setup, hub: &Mutex<Option<HubProc>>) -> String {
    let status = hub
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
        .and_then(|process| process.child.try_wait().ok().flatten());
    let state = match status {
        Some(status) => format!("exited ({status})"),
        None => "was still running".to_owned(),
    };
    format!(
        "{state}; hub.log tail (last {HUB_LOG_TAIL} lines):\n{}",
        hub_log_tail(&setup.hub_log())
    )
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
        Ok(Ok(connected)) => connected,
        Ok(Err(error)) => panic!(
            "hub connect failed with {error} waiting for the hub to start and say hub_hello: the hub {}",
            hub_start_report(setup, hub)
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "waited {wait:?} for the hub to start and say hub_hello: the hub {}",
            hub_start_report(setup, hub)
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!(
            "the hub connect thread ended without a result: the hub {}",
            hub_start_report(setup, hub)
        ),
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
            pump: None,
            diag: None,
            deadline,
        }
    }

    /// Registers the timeout dump's context: the stderr snapshot the drain
    /// thread keeps and the session's log directory. Without it the timeout
    /// branch keeps the old message, so other `Socket` users are unaffected.
    pub(crate) fn set_diag(&mut self, diag: Diag) {
        self.diag = Some(diag);
    }

    /// Replaces the socket's deadline: a test that proved one wait expires
    /// proves it on a zero or short bound instead of spending what remains
    /// of the test's own deadline (`docs/testing.md`, "Waits and timeouts").
    pub(crate) fn set_deadline(&mut self, deadline: Deadline) {
        self.deadline = deadline;
    }

    /// Reads the socket on a thread from now on, so a client the test holds
    /// open and does not read never fills its socket buffer and wedges the
    /// hub's relay (macOS buffers 8 KiB per Unix socket, Linux ~212 KiB).
    /// [`Socket::next`] takes the lines from the thread, still bounded by
    /// the deadline. Call it before the first line is read.
    pub(crate) fn pumped(mut self) -> Self {
        let stream = self.write.lock().unwrap().try_clone().unwrap();
        let (lines, pump) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut buf = String::new();
            loop {
                buf.clear();
                let line = match reader.read_line(&mut buf) {
                    Ok(0) | Err(_) => None,
                    Ok(_) => {
                        let line = buf.trim_end_matches(&['\r', '\n'][..]).to_owned();
                        Some(serde_json::from_str(&line).unwrap_or(Value::String(line)))
                    }
                };
                let closed = line.is_none();
                if lines.send(line).is_err() || closed {
                    return;
                }
            }
        });
        self.pump = Some(Mutex::new(pump));
        self
    }
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
            watchdog.stand_down(self.deadline.cleanup_phase());
        }
    }

    /// Called after the signal that every session in the workspace is ending
    /// (its socket closed, its session_left, or the guard's SIGKILL): waits
    /// under the test's [`Deadline`] for every process holding the workspace
    /// path to exit, then stands the guard down.
    #[track_caller]
    pub(crate) fn wait_gone(mut self) {
        match fakes::try_matching_exits(&self.workspace, self.deadline) {
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

/// Waits for the fake provider's first model request under `deadline`:
/// the session sends it only after the pre-send fsync, which is the
/// fsync that makes the appended `session_started` first line durable.
/// A stop after this point always leaves a record the resume finds.
/// A subscribe ack proves only that the loop serves, not that the line
/// reached the disk, so it cannot stand in for this wait.
#[track_caller]
pub(crate) fn await_first_request(server: &ProviderServer, deadline: Deadline) {
    assert!(
        server.await_requests(1, deadline),
        "the session sends its first model request"
    );
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
pub(crate) fn run_to_exit(
    deadline: Deadline,
    what: &str,
    mut command: Command,
) -> std::process::Output {
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
        !fakes::group_alive(group),
        "{what} left a process in its group behind"
    );
    watchdog.stand_down(deadline.cleanup_phase());
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

/// A `session_status` line: ephemeral, and written by an observer thread, so
/// where it falls among the loop's own lines is not what these tests pin.
/// `tests/socket.rs` reads it.
pub(crate) fn is_status(line: &str) -> bool {
    line.contains(r#""kind":"session_status""#)
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
    log::sessions_dir(&setup.home(), &worktree::project(&setup.workspace())).join(id)
}

/// The newline-terminated lines of `text` as JSON: a trailing fragment
/// without its `\n` is still being written, so it is dropped. A complete
/// line that fails to parse still panics.
pub(crate) fn complete_lines(text: &str) -> Vec<Value> {
    let complete = text.rfind('\n').map_or("", |end| &text[..=end]);
    complete
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
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

/// Independently triggered lines have no position in an ordered projection
/// (`docs/testing.md`, "Event streams"). Fixed counts use the raw lines.
const UNORDERED_KINDS: &[&str] = &["clients", "session_status", "attention", "date_changed"];

/// The ordered event kinds, setting aside independently triggered lines.
pub(crate) fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .filter(|kind| !UNORDERED_KINDS.contains(kind))
        .collect()
}

/// The relation a cited doc line promises between an exact kind pair. The
/// relation is passed explicitly at each call site so its strength matches
/// the wording of the cited line: "after" maps to [`Relation::After`],
/// "just before" to [`Relation::JustBefore`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Relation {
    /// The first kind appears before the second, with any lines between.
    After,
    /// The first kind sits immediately before the second: no line of the
    /// same run falls between them.
    JustBefore,
}

/// Asserts a whole stream as a multiset: every ordered kind in `expected`,
/// plus each set-aside kind the test names in `counted_set_aside` because
/// its count is fixed (such as one `clients` line per attach) or under
/// test. Every other set-aside kind is ignored, as is `job_delta` when
/// `ignore_job_delta` carries the combined citation for its documented
/// exception and its ephemeral-progress definition. `count_doc` is the
/// verbatim `docs/testing.md`, "Event streams" count rule, shown in each
/// call. A set-aside kind in `expected` must be named in `counted_set_aside`,
/// and `expected` must not hold `job_delta` while it is excluded: both are
/// rejected as contradictory policies. A counted set-aside kind may be
/// absent from `expected` to assert its expected count is zero. Only the
/// documented `job_delta` is excludable; no other kind can be ignored.
#[track_caller]
pub(crate) fn assert_kinds_multiset(
    lines: &[Value],
    expected: &[&str],
    counted_set_aside: &[&str],
    ignore_job_delta: Option<&str>,
    count_doc: &str,
) {
    for kind in expected {
        if *kind == "job_delta" && ignore_job_delta.is_some() {
            panic!(
                "contradictory policy: `job_delta` is expected yet excluded ({})",
                ignore_job_delta.unwrap_or_default()
            );
        }
        if UNORDERED_KINDS.contains(kind) && !counted_set_aside.contains(kind) {
            panic!(
                "contradictory policy: set-aside kind `{kind}` is expected but not counted; \
                 name it in `counted_set_aside` or drop it ({count_doc})"
            );
        }
    }
    let mut actual: Vec<&str> = Vec::with_capacity(lines.len());
    for line in lines {
        let kind = line["kind"].as_str().unwrap();
        if kind == "job_delta" && ignore_job_delta.is_some() {
            continue;
        }
        if UNORDERED_KINDS.contains(&kind) && !counted_set_aside.contains(&kind) {
            continue;
        }
        actual.push(kind);
    }
    let mut actual_sorted = actual;
    actual_sorted.sort_unstable();
    let mut expected_sorted = expected.to_vec();
    expected_sorted.sort_unstable();
    assert_eq!(
        actual_sorted, expected_sorted,
        "event kinds as a set with counts ({count_doc})"
    );
}

/// Asserts an exact kind pair in every run of one job: runs are delimited
/// in the full stream from each `job_started` carrying `job_id` through its
/// matching `job_completed` (or the end of the stream when the run never
/// completed), and the pair must hold in each run. A later run cannot borrow
/// the pair from an earlier one. `After` checks an ordered subsequence within
/// the run, allowing other jobs to interleave. `JustBefore` checks adjacency
/// in the full ordered stream after removing only kinds explicitly set aside
/// by the testing contract (`clients`, `session_status`, `attention`,
/// `date_changed`); other jobs or ordered kinds between the pair fail it.
/// `doc` is the verbatim source line promising the order.
#[track_caller]
pub(crate) fn assert_promised_order(
    lines: &[Value],
    job_id: &str,
    pair: (&str, &str),
    relation: Relation,
    doc: &str,
) {
    fn job_of(line: &Value) -> Option<&str> {
        line.get("payload")
            .and_then(|payload| payload.get("job_id"))
            .and_then(Value::as_str)
    }
    fn kind_of(line: &Value) -> &str {
        line["kind"].as_str().unwrap()
    }
    let (first, second) = pair;
    let has_job = lines.iter().any(|line| job_of(line) == Some(job_id));
    assert!(
        !lines.is_empty() && has_job,
        "no lines for job {job_id}: {doc}",
    );
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| kind_of(line) == "job_started" && job_of(line) == Some(job_id))
        .map(|(i, _)| i)
        .collect();
    assert!(
        !starts.is_empty(),
        "no run for job {job_id}: no `job_started` for it ({doc})",
    );
    // Each run holds its start through its matching completion: the first
    // `job_completed` for this job at or after the start. Runs of one job
    // never overlap, so that completion closes the run that opened it.
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(starts.len());
    for start in &starts {
        let end = lines[*start..]
            .iter()
            .position(|line| kind_of(line) == "job_completed" && job_of(line) == Some(job_id))
            .map(|at| start + at + 1)
            .unwrap_or(lines.len());
        spans.push((*start, end));
    }
    for (n, (start, end)) in spans.iter().enumerate() {
        let span = &lines[*start..*end];
        let holds = match relation {
            Relation::After => {
                let mut seen_first = false;
                let mut holds = false;
                for line in span {
                    if kind_of(line) == first && job_of(line) == Some(job_id) {
                        seen_first = true;
                    } else if seen_first && kind_of(line) == second && job_of(line) == Some(job_id)
                    {
                        holds = true;
                        break;
                    }
                }
                // A `first` at the very end with no later `second` fails;
                // the loop above only sets `holds` on a later `second`.
                holds
            }
            Relation::JustBefore => {
                let mut previous: Option<&Value> = None;
                let mut holds = false;
                for line in span {
                    if UNORDERED_KINDS.contains(&kind_of(line)) {
                        continue;
                    }
                    if let Some(previous) = previous.take()
                        && kind_of(previous) == first
                        && job_of(previous) == Some(job_id)
                        && kind_of(line) == second
                        && job_of(line) == Some(job_id)
                    {
                        holds = true;
                        break;
                    }
                    previous = Some(line);
                }
                holds
            }
        };
        // The failure renders the run's set-aside-projected kinds: the full
        // ordered stream decides adjacency, so the projection is what the
        // `JustBefore` check saw.
        let shown: Vec<&str> = span
            .iter()
            .map(kind_of)
            .filter(|kind| !UNORDERED_KINDS.contains(kind))
            .collect();
        assert!(
            holds,
            "job {job_id} run {n}: `{first}` is not {relation:?} `{second}` in {shown:?} ({doc})",
        );
    }
}

/// Asserts a complete session log's lifecycle edges: the log is filtered to
/// one `session_id`, its first ordered kind is `session_started`, every
/// `fiber_started` opens a process run its `fiber_exited` closes, and the
/// last ordered kind is `fiber_exited`. Tests whose socket or stdout capture
/// holds only a resumed process pass that session's complete durable log
/// here; a partial stream never claims an exit. The helper takes the three
/// verbatim source lines it enforces. It does not model `rewound` or
/// `moved` endings, which these sites do not exercise.
#[track_caller]
pub(crate) fn assert_session_process_boundaries(
    lines: &[Value],
    session_id: &str,
    session_first_doc: &str,
    process_start_doc: &str,
    process_end_doc: &str,
) {
    let mine: Vec<&Value> = lines
        .iter()
        .filter(|line| line.get("session_id").and_then(Value::as_str) == Some(session_id))
        .collect();
    assert!(
        !mine.is_empty(),
        "no lines for session {session_id}: {session_first_doc}",
    );
    let ordered: Vec<&str> = mine
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .filter(|kind| !UNORDERED_KINDS.contains(kind))
        .collect();
    assert_eq!(
        ordered.first(),
        Some(&"session_started"),
        "session {session_id} log does not start with `session_started`: {session_first_doc}; got {ordered:?}",
    );
    assert!(
        ordered.contains(&"fiber_started"),
        "session {session_id} log holds no `fiber_started`: {process_start_doc}; got {ordered:?}",
    );
    let mut open = false;
    for kind in &ordered[1..] {
        match (*kind, open) {
            ("fiber_started", false) => open = true,
            ("fiber_started", true) => panic!(
                "session {session_id}: `fiber_started` while a process run is still open \
                 (missing `fiber_exited`): {process_end_doc}; got {ordered:?}",
            ),
            ("fiber_exited", true) => open = false,
            ("fiber_exited", false) => panic!(
                "session {session_id}: `fiber_exited` with no process run open \
                 (missing `fiber_started`): {process_end_doc}; got {ordered:?}",
            ),
            _ => {}
        }
    }
    assert!(
        !open,
        "session {session_id}: last process run never closed with `fiber_exited`: {process_end_doc}; got {ordered:?}",
    );
    assert_eq!(
        ordered.last(),
        Some(&"fiber_exited"),
        "session {session_id} log does not end with the process exit: {process_end_doc}; got {ordered:?}",
    );
}

/// Asserts exact multiset membership strictly between identity-selected
/// anchors. Callers separately assert the anchors and any promised order.
pub(crate) fn assert_set_between(lines: &[Value], lo: usize, hi: usize, expected: &[&str]) {
    assert!(lo < hi, "anchors must increase: {lo}, {hi}");
    assert!(
        hi < lines.len(),
        "anchor {hi} outside {} lines",
        lines.len()
    );
    let mut actual = kinds(&lines[lo + 1..hi]);
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(
        actual,
        expected,
        "event window {lo}..{hi}: {:?}",
        kinds(lines)
    );
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
        drop(fakes::kill_group(self.0, "KILL"));
    }
}

pub(crate) fn expected(kinds: &[&str]) -> Vec<Value> {
    kinds.iter().map(|kind| json!({"kind": kind})).collect()
}

/// The request's tool order: the loop keys tools by name, so this is name
/// order, whatever order `main` pushes them in.
pub(crate) const TOOL_NAMES: [&str; 17] = [
    "ask_user",
    "delegate_fork",
    "delegate_message",
    "delegate_models",
    "delegate_spawn",
    "edit",
    "handoff",
    "jobs",
    "name_session",
    "read",
    "session_list",
    "session_message",
    "session_search",
    "shell",
    "skill",
    "web_fetch",
    "write",
];

/// The event kinds of a turn whose first reply calls one reads-only tool
/// and whose second is [`hello`]: no `permission_` line is written, as for
/// a reads-only workspace call (`docs/permissions.md`, "Fast paths").
pub(crate) fn read_kinds() -> Vec<&'static str> {
    let mut kinds = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
    ];
    kinds.extend(["assistant_message_delta"; 2]);
    kinds.extend([
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

/// The Session card once its `cost` row shows a figure: the reserved row
/// draws its label from the first frame with a dim dash, so a wait that
/// supplies panel coordinates for a later write, or that snapshots the
/// card, holds until the figure lands (`crates/tui/src/view/panel.rs`,
/// `session_rows`). The `cost` label draws with the reserved row too:
/// the row counts only once its value is not the dash. `cost on
/// subscription` draws without it, so its `cost` prefix does not count.
/// `left` is the panel's first column: a `cost` elsewhere (a
/// conversation echo, another card) does not count.
pub(crate) fn session_cost_drawn(grid: &pty::Grid, left: u16) -> bool {
    let (width, _) = grid.size();
    (0..grid.rows.len()).any(|row| {
        let Some(y) = u16::try_from(row).ok() else {
            return false;
        };
        // The reserved row ends in the dim dash; a figured row does not.
        let figured = grid
            .rows
            .get(row)
            .is_none_or(|line| !line.trim_end().ends_with('—'));
        figured
            && (left..width.saturating_sub(3)).any(|x| {
                // Four cells spell `cost`: `x + 3` stays below `width`, so no
                // cell read leaves the grid.
                let spells = ["c", "o", "s", "t"]
                    .iter()
                    .zip(0..4u16)
                    .all(|(want, dx)| grid.cell(x + dx, y).symbol == *want);
                // A whole word: the cells around the match hold no word char.
                let before_ok = x == 0
                    || grid
                        .cell(x - 1, y)
                        .symbol
                        .chars()
                        .last()
                        .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                let after_ok = x + 4 >= width
                    || grid
                        .cell(x + 4, y)
                        .symbol
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                // The label ends there: the subscription label continues
                // ` on subscription`, with its value always after it, so
                // sixteen cells fit while the row is the subscription one;
                // `&&` short-circuits the cell reads otherwise.
                let continued = width - x >= 20
                    && " on subscription"
                        .chars()
                        .zip(4..20u16)
                        .all(|(want, dx)| grid.cell(x + dx, y).symbol == want.to_string());
                spells && before_ok && after_ok && !continued
            })
    })
}

#[test]
fn timeout_message_holds_the_old_first_line_and_all_three_sections() {
    let dir = fakes::TempDir::new("diag");
    let dir_path = dir.path().join("s_1");
    fs::create_dir_all(&dir_path).unwrap();
    fs::write(
        dir_path.join("events.jsonl"),
        "{\"kind\":\"session_started\"}\n{\"kind\":\"fiber_started\"}\n",
    )
    .unwrap();
    let stderr = Arc::new(Mutex::new("boom on fd 2\n".to_owned()));
    let diag = Diag::new(Arc::clone(&stderr), dir_path);
    let got = vec![json!({"kind": "command_accepted"})];
    let message = diag.report("the refused prompt", &got);
    let first = message.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "waited until the deadline for the refused prompt; got [Object {\"kind\": String(\"command_accepted\")}]"
    );
    assert!(message.contains("fiber stderr:"), "{message}");
    assert!(message.contains("boom on fd 2"), "{message}");
    assert!(
        message.contains("session log tail (events.jsonl, last 20 lines):"),
        "{message}"
    );
    assert!(
        message.contains("\"kind\":\"session_started\""),
        "{message}"
    );
}

#[test]
fn timeout_message_marks_empty_stderr_as_still_running() {
    let message = timeout_message("the refused prompt", &[], "", "{\"kind\":\"x\"}\n");
    assert!(
        message.contains("(still running: no stderr captured yet)"),
        "{message}"
    );
    let message = timeout_message("the refused prompt", &[], "late line\n", "");
    assert!(!message.contains("still running"), "{message}");
    assert!(message.contains("late line"), "{message}");
}

#[test]
fn closed_socket_dump_with_diag_shows_lines_stderr_and_log_tail() {
    let dir = fakes::TempDir::new("diag-closed");
    let dir_path = dir.path().join("s_9");
    fs::create_dir_all(&dir_path).unwrap();
    fs::write(
        dir_path.join("events.jsonl"),
        "{\"kind\":\"session_started\"}\n{\"kind\":\"fiber_exited\"}\n",
    )
    .unwrap();
    let stderr = Arc::new(Mutex::new("boom on fd 2\n".to_owned()));
    let diag = Diag::new(Arc::clone(&stderr), dir_path);
    // One line arrives before the peer goes away, so the panic names a
    // received line; the close itself needs no timing.
    let (mine, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"{\"kind\":\"command_accepted\"}\n")
        .unwrap();
    drop(peer);
    let mut client = Socket::from(Deadline::after(fakes::MUST_SUCCEED_WITHIN), mine);
    client.set_diag(diag);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until(&client, "the stopped child exits", |_| false);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    let first = message.lines().next().unwrap_or_default();
    assert_eq!(
        first, "the socket closed while waiting for the stopped child exits",
        "{message}"
    );
    assert!(message.contains("command_accepted"), "{message}");
    assert!(message.contains("fiber stderr:"), "{message}");
    assert!(message.contains("boom on fd 2"), "{message}");
    assert!(
        message.contains("session log tail (events.jsonl, last 20 lines):"),
        "{message}"
    );
    assert!(
        message.contains("\"kind\":\"session_started\""),
        "{message}"
    );
    // A closed socket already ended, so an empty snapshot says nothing was
    // captured, never that the session is still running.
    let empty = closed_message("the stopped child exits", &[], "", "{\"kind\":\"x\"}\n");
    assert!(empty.contains("(no stderr captured)"), "{empty}");
    assert!(!empty.contains("still running"), "{empty}");
}

#[test]
fn closed_socket_dump_keeps_stderr_and_log_tail_within_the_output_cap() {
    // The CI output cap keeps the first 6000 characters: with a long
    // received-lines dump, the stderr and log-tail sections still fit
    // because the dump comes last.
    const OUTPUT_CAP: usize = 6000;
    let stderr_sentinel = "stderr-sentinel-9f3a1c";
    let log_sentinel = "\"kind\":\"log-tail-sentinel-7e2b\"";
    let got: Vec<Value> = (0..2000)
        .map(|n| json!({"kind": "filler", "n": n, "pad": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}))
        .collect();
    let message = closed_message(
        "the stopped child exits",
        &got,
        stderr_sentinel,
        log_sentinel,
    );
    assert!(
        message.len() > OUTPUT_CAP,
        "dump fits the cap: {}",
        message.len()
    );
    let visible = &message[..OUTPUT_CAP];
    // A large value never prints whole on failure (`docs/testing.md`, "What
    // a test asserts"): report the length and the offsets instead.
    assert!(
        visible.contains("fiber stderr:"),
        "stderr header missing from first {OUTPUT_CAP} of {}-byte message",
        message.len()
    );
    assert!(
        visible.contains(stderr_sentinel),
        "stderr sentinel at {} outside first {OUTPUT_CAP} of {}-byte message",
        message.find(stderr_sentinel).unwrap_or(usize::MAX),
        message.len()
    );
    let tail_header = "session log tail (events.jsonl, last 20 lines):";
    assert!(
        visible.contains(tail_header),
        "log-tail header missing from first {OUTPUT_CAP} of {}-byte message",
        message.len()
    );
    assert!(
        visible.contains(log_sentinel),
        "log-tail sentinel at {} outside first {OUTPUT_CAP} of {}-byte message",
        message.find(log_sentinel).unwrap_or(usize::MAX),
        message.len()
    );
    // The diagnostics precede the dump: flipping the order back would
    // push both sentinels past the cap.
    let stderr_at = message.find(stderr_sentinel).unwrap_or(usize::MAX);
    let log_at = message.find(log_sentinel).unwrap_or(usize::MAX);
    let dump_at = message.find("got [").unwrap_or(usize::MAX);
    assert!(
        stderr_at < dump_at,
        "stderr sentinel at {stderr_at} not before dump at {dump_at} ({}-byte message)",
        message.len()
    );
    assert!(
        log_at < dump_at,
        "log-tail sentinel at {log_at} not before dump at {dump_at} ({}-byte message)",
        message.len()
    );
}

#[test]
fn until_without_diag_keeps_the_old_closed_message() {
    let (mine, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let client = Socket::from(Deadline::after(fakes::MUST_SUCCEED_WITHIN), mine);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until(&client, "the stopped child exits", |_| false);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert_eq!(
        message,
        "the socket closed while waiting for the stopped child exits; got []"
    );
}

#[test]
fn session_log_tail_keeps_only_complete_lines() {
    // No asserted count cites a `docs/<area>.md` line promising it
    // (`docs/code-quality.md`, "What a reviewer checks"): the tail's width
    // is the helper's own bound, so this test pins presence and recency,
    // not the bound. A short log comes back whole.
    let dir = fakes::TempDir::new("diag-tail");
    assert_eq!(session_log_tail(&dir.path().join("missing")), "");
    let file = dir.path().join("s_2");
    fs::create_dir_all(&file).unwrap();
    fs::write(file.join("events.jsonl"), "{\"kind\":\"partial\"}").unwrap();
    assert_eq!(session_log_tail(&file), "");
    let short: String = (0..5).map(|n| format!("{{\"n\":{n}}}\n")).collect();
    fs::write(file.join("events.jsonl"), &short).unwrap();
    assert_eq!(session_log_tail(&file), short.trim_end());
    // A long log keeps its last line and drops an early one; a trailing
    // fragment without its newline is still being written, so it is dropped.
    let long: String = (0..40).map(|n| format!("{{\"n\":{n}}}\n")).collect();
    fs::write(file.join("events.jsonl"), &long).unwrap();
    let tail = session_log_tail(&file);
    assert!(tail.contains("{\"n\":39}"), "{tail}");
    assert!(!tail.contains("{\"n\":0}"), "{tail}");
    let mut cut = long.clone();
    cut.push_str("{\"n\":40");
    fs::write(file.join("events.jsonl"), &cut).unwrap();
    let tail = session_log_tail(&file);
    assert!(!tail.contains("{\"n\":40"), "{tail}");
    assert!(tail.contains("{\"n\":39}"), "{tail}");
}

#[test]
fn session_logs_tail_names_each_session_in_sorted_order() {
    // `read_dir` order is the filesystem's, so the assertion pins the
    // sorted output the function promises, not the listing order: the
    // folders are created out of sorted order on purpose.
    let sessions = fakes::TempDir::new("diag-tails");
    for (name, log) in [
        ("s_b", "{\"b\":0}\n{\"b\":1}\n"),
        ("s_a", "{\"a\":0}\n{\"a\":1"),
    ] {
        let dir = sessions.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), log).unwrap();
    }
    // A stray non-directory entry is skipped without a panic and does
    // not appear in the report.
    fs::write(sessions.path().join("stray"), "x").unwrap();
    assert_eq!(
        session_logs_tail(sessions.path()),
        "session s_a:\n{\"a\":0}\nsession s_b:\n{\"b\":0}\n{\"b\":1}"
    );
    assert_eq!(session_logs_tail(&sessions.path().join("missing")), "");
}

#[test]
fn stderr_drain_keeps_a_live_snapshot_of_the_full_text() {
    let snapshot = Arc::new(Mutex::new(String::new()));
    let mut pipe = io::Cursor::new(b"first line\nsecond line\n".to_vec());
    let text = drain_stderr_to_snapshot(&mut pipe, &snapshot);
    assert_eq!(text, "first line\nsecond line\n");
    assert_eq!(*snapshot.lock().unwrap(), "first line\nsecond line\n");
    let mut empty = io::Cursor::new(Vec::new());
    let blank = Arc::new(Mutex::new(String::new()));
    assert_eq!(drain_stderr_to_snapshot(&mut empty, &blank), "");
    assert_eq!(*blank.lock().unwrap(), "");
}

/// A reader that dribbles out at most `max` bytes per read, so a multibyte
/// character lands split across reads.
struct Dribble<'a> {
    rest: &'a [u8],
    max: usize,
}

impl io::Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.rest.is_empty() {
            return Ok(0);
        }
        let take = self.rest.len().min(buf.len()).min(self.max);
        buf[..take].copy_from_slice(&self.rest[..take]);
        self.rest = &self.rest[take..];
        Ok(take)
    }
}

#[test]
fn stderr_drain_keeps_a_character_split_across_reads() {
    // One byte per read splits `é` and `ö` across reads: decoding each read
    // alone would leave a replacement character per half.
    let snapshot = Arc::new(Mutex::new(String::new()));
    let mut pipe = Dribble {
        rest: "héllo wörld\n".as_bytes(),
        max: 1,
    };
    let text = drain_stderr_to_snapshot(&mut pipe, &snapshot);
    assert_eq!(text, "héllo wörld\n");
    assert_eq!(*snapshot.lock().unwrap(), "héllo wörld\n");
    assert!(
        !snapshot.lock().unwrap().contains('\u{FFFD}'),
        "{}",
        snapshot.lock().unwrap()
    );
}

/// A reader that yields `rest` once, then fails every read after.
struct FailAfter<'a> {
    rest: Option<&'a [u8]>,
}

impl io::Read for FailAfter<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.rest.take() {
            Some(chunk) => {
                let take = chunk.len().min(buf.len());
                buf[..take].copy_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    self.rest = Some(&chunk[take..]);
                }
                Ok(take)
            }
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "stub read error")),
        }
    }
}

#[test]
fn stderr_drain_sibling_preserves_the_first_read_error() {
    // The sibling keeps the text and the snapshot exactly as the wrapper
    // would, and carries the first read error beside them.
    let snapshot = Arc::new(Mutex::new(String::new()));
    let mut pipe = FailAfter {
        rest: Some(b"half\n"),
    };
    let (text, error) = drain_stderr_to_snapshot_reported(&mut pipe, &snapshot);
    assert_eq!(text, "half\n");
    assert_eq!(*snapshot.lock().unwrap(), "half\n");
    assert!(
        error.is_some_and(|error| error.contains("stub read error")),
        "the sibling names the first read error"
    );
    // The wrapper drops the error but keeps the text and the snapshot, so
    // every existing caller reads exactly what it read before.
    let blank = Arc::new(Mutex::new(String::new()));
    let mut again = FailAfter {
        rest: Some(b"half\n"),
    };
    assert_eq!(drain_stderr_to_snapshot(&mut again, &blank), "half\n");
    assert_eq!(*blank.lock().unwrap(), "half\n");
}

#[test]
fn captured_stderr_marks_an_incomplete_drain() {
    // No completion sent: the drain is still running, so a zero bound
    // expires at once and the clone carries what arrived with the marker,
    // never a panic. The sender stays alive, so the miss is a timeout, not
    // a disconnect.
    let snapshot = Arc::new(Mutex::new("half\n".to_owned()));
    let (_live, drained) = mpsc::channel::<()>();
    let capture = CapturedStderr { snapshot, drained };
    let text = capture.text(Deadline::after(Duration::ZERO));
    assert!(
        text.contains("half\n"),
        "an incomplete drain still clones what arrived: {text}"
    );
    assert!(
        text.contains("(stderr drain incomplete: showing what arrived)"),
        "an incomplete drain reads as its own marker, never as whole: {text}"
    );
    // A drained capture clones whole with no marker, even on a zero bound:
    // the completion is already there.
    let whole = CapturedStderr::drained_text("all\n".to_owned());
    assert_eq!(whole.text(Deadline::after(Duration::ZERO)), "all\n");
}

/// Waits under `deadline` until `path` is gone from the filesystem, polling
/// with no new process per pass (`docs/testing.md`, "Waits and timeouts"):
/// a session's socket path after its process exits, or a record a revoke
/// deletes. Expiry fails naming `what`.
#[track_caller]
pub(crate) fn wait_for_path_gone(deadline: Deadline, path: &Path, what: &str) {
    let path = path.to_owned();
    let what = what.to_owned();
    fakes::wait_until(&what, deadline, move || {
        fs::symlink_metadata(&path).is_err()
    });
}

/// Waits under `deadline` until `dir`'s session lock is free: a probe
/// `try_hold` succeeds, and the probe guard drops at once. A session
/// unlinks its socket and releases its lock at exit (`docs/state.md`,
/// "Sockets"), and `fiber_exited` only proves the exit, so a test that
/// deletes or resumes next waits here first. Expiry fails naming `what`.
#[track_caller]
pub(crate) fn wait_for_lock_released(deadline: Deadline, dir: &Path, what: &str) {
    let dir = dir.to_owned();
    let what = what.to_owned();
    fakes::wait_until(&what, deadline, move || {
        matches!(log::try_hold(&dir), Ok(log::Hold::Held(_)))
    });
}

/// Collects socket lines through `next` until the command's effect after
/// its `command_accepted`, matched by `id`: every line takes what remains
/// of the test's deadline through the socket's own read, so expiry names
/// the effect, not the acceptance. `next` is the socket's line reader, so
/// every client type with a `next` fits. The acceptance only arms the wait;
/// the completing line must carry the same command id and satisfy
/// `is_effect`, so an unrelated event for another command never completes
/// it. A test that stops at the acceptance and then asserts the effect
/// races the command's run; this waits past the acceptance for the
/// id-matched effect instead.
#[track_caller]
pub(crate) fn wait_for_effect_after_accept(
    what: &str,
    id: &str,
    mut next: impl FnMut(&str, &[Value]) -> Option<Value>,
    mut is_effect: impl FnMut(&Value) -> bool,
) -> Vec<Value> {
    let mut lines = Vec::new();
    let mut accepted = false;
    loop {
        let line = match next(what, &lines) {
            Some(line) => line,
            None => panic!("the socket closed while waiting for {what}; got {lines:?}"),
        };
        if line["kind"] == "command_accepted" && line["payload"]["command_id"] == id {
            accepted = true;
        }
        let done = accepted && line["payload"]["command_id"] == id && is_effect(&line);
        lines.push(line);
        if done {
            return lines;
        }
    }
}

/// Registers the native change watch on the log at `log`: the session's
/// diagnostic record of a detached fetch, or `hub.log` after a
/// client-visible pairing event. The file may not exist yet when the
/// emitter starts it: the registration first waits under `deadline` for
/// the file to appear, then registers, so the watch always precedes the
/// wait and the reread after registration never loses a line. Call this
/// before starting the emitter whose line the test waits for. Each expiry
/// fails naming `what`.
#[track_caller]
pub(crate) fn watch_log(log: &Path, deadline: Deadline, what: &str) -> HubLogWatch {
    let pending = log.to_owned();
    fakes::wait_until(&format!("{what}: the log to appear"), deadline, move || {
        pending.exists()
    });
    HubLogWatch::register(log)
        .unwrap_or_else(|error| panic!("watching {} for {what}: {error}", log.display()))
}

/// Blocks on a registered watch until `ready` holds for a reread of its
/// log, or the wait started at entry expires: every wake rereads the whole
/// file, so a line landing between registration and the first wait is never
/// lost. Expiry fails naming `what`.
#[track_caller]
pub(crate) fn wait_for_watched_line(
    watch: &HubLogWatch,
    deadline: Deadline,
    what: &str,
    ready: impl Fn(&str) -> bool,
) {
    watch
        .wait_until(deadline, what, ready)
        .unwrap_or_else(|error| panic!("{error}"));
}

/// Waits under `deadline` for every process matching `pattern` to exit:
/// the detached refresh child after `fiber models` returns, which rewrites
/// the cache before it exits. On expiry kills each match and its group,
/// checks they are gone, and fails naming the pattern.
#[track_caller]
pub(crate) fn wait_for_refresh_exit(deadline: Deadline, pattern: &str) {
    if fakes::matching_exits(pattern, deadline) {
        return;
    }
    match kill_matching(deadline, pattern) {
        Ok(()) | Err(_) => {}
    }
    fakes::try_matching_exits(pattern, deadline.cleanup_phase()).unwrap_or_else(|err| {
        panic!("the refresh child outlived SIGKILL past the cleanup deadline: {err}")
    });
    panic!("waited until the deadline for the refresh child to exit: {pattern}");
}

#[test]
fn run_kinds_sets_aside_independently_triggered_lines() {
    // `Run::kinds` delegates to the free helper, so a `date_changed` line
    // the contract writes on its own trigger never reaches an ordered
    // expectation (`docs/testing.md`, "Event streams"; #3758).
    let run = Run {
        code: Some(0),
        lines: vec![
            json!({"kind": "preamble_built"}),
            json!({"kind": "clients"}),
            json!({"kind": "session_status"}),
            json!({"kind": "turn_started"}),
            json!({"kind": "attention"}),
            json!({"kind": "date_changed"}),
            json!({"kind": "turn_completed"}),
        ],
        stderr: String::new(),
    };
    assert_eq!(
        run.kinds(),
        ["preamble_built", "turn_started", "turn_completed"]
    );
}

/// A held file between a signal and its removal: the writer signals, then
/// blocks until the test releases it, then removes the state.
struct PausedRemoval {
    release: mpsc::Sender<()>,
    removed: mpsc::Receiver<()>,
}

fn pause_before_removal(path: PathBuf) -> PausedRemoval {
    let (release_tx, release) = mpsc::channel::<()>();
    let (removed_tx, removed) = mpsc::channel::<()>();
    thread::spawn(move || {
        release.recv().unwrap_or(());
        fs::remove_file(&path).unwrap_or(());
        removed_tx.send(()).unwrap_or(());
    });
    PausedRemoval {
        release: release_tx,
        removed,
    }
}

#[test]
fn path_gone_fails_fast_while_held_and_passes_after_release() {
    let dir = fakes::TempDir::new("wait-path-gone");
    let path = dir.path().join("gone");
    fs::write(&path, "state").unwrap();
    // The old wait is the bare assertion right after the signal: with the
    // removal held, it fails, proving the signal alone is not the proof.
    let held = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(!path.exists(), "the path is gone");
    }));
    assert!(held.is_err(), "the bare assertion must fail while held");
    let paused = pause_before_removal(path.clone());
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (done_tx, done) = mpsc::channel::<()>();
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    thread::spawn(move || {
        entered_tx.send(()).unwrap_or(());
        wait_for_path_gone(deadline, &path, "the held path to go");
        done_tx.send(()).unwrap_or(());
    });
    // The waiter signals it entered before the release goes out, so the
    // wait below answers the release, never a thread that never ran.
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&entered)
        .expect("the waiter entered its wait");
    paused.release.send(()).unwrap_or(());
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("the wait answers once released");
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&paused.removed)
        .expect("the remover finished removing the path");
}

#[test]
fn lock_released_fails_fast_while_held_and_passes_after_release() {
    let dir = fakes::TempDir::new("wait-lock");
    let held_guard = log::try_hold(dir.path()).unwrap();
    assert!(
        matches!(held_guard, log::Hold::Held(_)),
        "the test holds the lock"
    );
    // The old wait probes once right after the exit signal: with the lock
    // still held, the probe is busy, so the bare assertion fails.
    let held = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(
            matches!(log::try_hold(dir.path()), Ok(log::Hold::Held(_))),
            "the session lock is released"
        );
    }));
    assert!(held.is_err(), "the bare probe must fail while held");
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (done_tx, done) = mpsc::channel::<()>();
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let waited = dir.path().to_path_buf();
    thread::spawn(move || {
        entered_tx.send(()).unwrap_or(());
        wait_for_lock_released(deadline, &waited, "the held session lock");
        done_tx.send(()).unwrap_or(());
    });
    // The waiter signals it entered while the lock is still held, so the
    // wait below answers the release, never a thread that never ran.
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&entered)
        .expect("the waiter entered its wait");
    drop(held_guard);
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("the wait answers once released");
}

#[test]
fn effect_after_accept_ignores_other_ids_and_passes_on_its_own() {
    fn parked_pair() -> (Socket, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
        let (mine, peer) = UnixStream::pair().unwrap();
        let client = Socket::from(deadline, mine);
        let (release_tx, release) = mpsc::channel::<()>();
        let (accepted_tx, accepted) = mpsc::channel::<()>();
        thread::spawn(move || {
            let mut peer = peer;
            write_line(
                &mut peer,
                deadline,
                b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_1\"}}\n",
                "the acceptance",
            )
            .unwrap();
            accepted_tx.send(()).unwrap_or(());
            release.recv().unwrap_or(());
            // An unrelated answer for another command: result-bearing, but
            // the helper must not complete on it.
            write_line(
                &mut peer,
                deadline,
                b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_9\",\"result\":{\"answer\":2}}}\n",
                "the unrelated answer",
            )
            .unwrap();
            write_line(
                &mut peer,
                deadline,
                b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_1\",\"result\":{\"answer\":1}}}\n",
                "the answer",
            )
            .unwrap();
        });
        (client, accepted, release_tx)
    }
    // The old wait stops at the acceptance and reads the result off it:
    // with the command parked on a bare acceptance, there is none.
    let (old_client, old_accepted, old_release) = parked_pair();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&old_accepted)
        .expect("the parked command was accepted");
    let old = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let line = old_client
            .next("the acceptance", &[])
            .expect("the acceptance arrived");
        assert_eq!(line["kind"], "command_accepted");
        assert!(
            line["payload"].get("result").is_some(),
            "the answer is already there: {line}"
        );
    }));
    assert!(old.is_err(), "the old wait must fail while parked");
    old_release.send(()).unwrap_or(());
    // The helper waits past the acceptance for the id-matched answer on a
    // fresh pair: the waiter signals it entered, the unrelated answer must
    // not complete it, and the release ends it at its own answer.
    let (client, accepted, release) = parked_pair();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&accepted)
        .expect("the parked command was accepted");
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (done_tx, done) = mpsc::channel::<Vec<Value>>();
    thread::spawn(move || {
        entered_tx.send(()).unwrap_or(());
        let lines = wait_for_effect_after_accept(
            "the c_1 answer",
            "c_1",
            |what, got| client.next(what, got),
            |line| line["payload"].get("result").is_some(),
        );
        done_tx.send(lines).unwrap_or(());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&entered)
        .expect("the waiter entered its wait");
    release.send(()).unwrap_or(());
    let lines = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("the wait answers once released");
    assert!(
        lines
            .iter()
            .any(|line| line["payload"]["command_id"] == "c_9"),
        "the wait saw the unrelated answer and skipped it: {lines:?}"
    );
    let last = lines.last().unwrap();
    assert_eq!(last["payload"]["command_id"], "c_1", "{lines:?}");
    assert_eq!(last["payload"]["result"], json!({"answer": 1}), "{lines:?}");
}

#[test]
fn log_line_fails_fast_while_held_and_passes_after_release() {
    let dir = fakes::TempDir::new("wait-log-line");
    let log = dir.path().join("hub.log");
    fs::write(&log, "signal\n").unwrap();
    // The watch registers before the writer thread starts, so nothing the
    // writer appends lands between registration and the first wait unseen.
    // Shared with the waiter thread: every assertion stays on the test
    // thread, where a failure fails the test.
    let watch = std::sync::Arc::new(watch_log(
        &log,
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        "the state line",
    ));
    // The old wait reads once right after the signal and asserts the state
    // line is there: with the writer held, it is not.
    let old = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let text = fs::read_to_string(&log).unwrap_or_default();
        assert!(text.contains("state"), "the state line landed: {text}");
    }));
    assert!(old.is_err(), "the old wait must fail while held");
    let (release_tx, release) = mpsc::channel::<()>();
    let logged = log.clone();
    thread::spawn(move || {
        release.recv().unwrap_or(());
        let mut file = fs::OpenOptions::new().append(true).open(&logged).unwrap();
        file.write_all(b"state\n").unwrap();
    });
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (done_tx, done) = mpsc::channel::<()>();
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let waited = std::sync::Arc::clone(&watch);
    thread::spawn(move || {
        entered_tx.send(()).unwrap_or(());
        wait_for_watched_line(&waited, deadline, "the state line", |text| {
            text.contains("state")
        });
        done_tx.send(()).unwrap_or(());
    });
    // The waiter signals it entered while the writer is still held, so the
    // wait below answers the release, never a thread that never ran.
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&entered)
        .expect("the waiter entered its wait");
    release_tx.send(()).unwrap_or(());
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("the wait answers once released");
    // The landed line stays visible to a later wait on the same watch.
    wait_for_watched_line(
        &watch,
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        "the already-landed state line",
        |text| text.contains("state"),
    );
}

#[test]
fn refresh_exit_fails_fast_while_held_and_passes_after_release() {
    let dir = fakes::TempDir::new("wait-refresh-exit");
    let fifo = dir.path().join("hold");
    fakes::fifo(&fifo);
    let pattern = format!("wait-refresh-exit-{}", dir.path().display());
    // The marker rides the child's command line, so the match is this
    // test's own; the FIFO keeps the child parked between the old wait's
    // signal and the exit: opening it for read blocks until the test
    // opens the write end to release it.
    let child = Command::new("sh")
        .args([
            "-c",
            &format!(
                "read line < '{fifo}'; exit 0 # {pattern}",
                fifo = fifo.display()
            ),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    // A reaper, so the exit leaves no zombie for the match to keep seeing.
    let (reaped_tx, reaped) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut child = child;
        match child.wait() {
            Ok(_) | Err(_) => {}
        }
        reaped_tx.send(()).unwrap_or(());
    });
    // The old wait checks the match at once: the parked child is still
    // listed, so the bare assertion fails.
    let old = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(
            fakes::matching(&pattern).unwrap_or_default().is_empty(),
            "the refresh child already exited"
        );
    }));
    assert!(old.is_err(), "the old wait must fail while held");
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (done_tx, done) = mpsc::channel::<()>();
    let waited = pattern.clone();
    thread::spawn(move || {
        entered_tx.send(()).unwrap_or(());
        wait_for_refresh_exit(deadline, &waited);
        done_tx.send(()).unwrap_or(());
    });
    // The waiter signals it entered while the child is still parked, so the
    // wait below answers the release, never a thread that never ran.
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&entered)
        .expect("the waiter entered its wait");
    // The release is opening the write end and closing it at once: the
    // parked read sees end of file, the child exits, and the reaper
    // clears it. The close happens before the wait below, never after.
    fs::OpenOptions::new()
        .write(true)
        .open(&fifo)
        .expect("the held child opened its FIFO for read");
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("the wait answers once released");
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&reaped)
        .expect("the reaper cleared the exited child");
}
