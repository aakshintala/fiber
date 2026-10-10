//! Starting `fiber`: the environment it runs under, its own process group
//! under a watchdog, stdout read line by line under deadlines, a socket
//! client, and a bounded stop (`docs/testing.md`, "Running tests" and
//! "Waits and timeouts").

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use fakes::Watchdog;
use serde_json::Value;

/// How long a stopped process may take to exit, and its group to empty,
/// after each signal.
pub(crate) const STOP: Duration = Duration::from_secs(5);

/// The pause between two probes of a condition the harness polls.
pub(crate) const PROBE: Duration = Duration::from_millis(5);

/// The operating system's clock: the one place the harness reads or waits
/// on real time.
pub(crate) struct System;

impl Clock for System {
    #[expect(
        clippy::disallowed_methods,
        reason = "the benchmark measures real time"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the benchmark measures real time"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the idle window and the probes wait on real time"
    )]
    fn sleep(&self, d: Duration) {
        thread::sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        wait(until.map(|until| until.saturating_duration_since(self.now())));
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

/// The time left until `until`, at least a millisecond, or an error naming
/// `what` once it has passed.
pub(crate) fn left(clock: &dyn Clock, until: Instant, what: &str) -> Result<Duration, String> {
    let left = until.saturating_duration_since(clock.now());
    if left.is_zero() {
        return Err(format!("timed out waiting for {what}"));
    }
    Ok(left.max(Duration::from_millis(1)))
}

/// Polls `ready` until it holds or `within` passes, naming `what` on expiry.
pub(crate) fn poll(
    clock: &dyn Clock,
    within: Duration,
    what: &str,
    mut ready: impl FnMut() -> Result<bool, String>,
) -> Result<(), String> {
    let until = clock.now() + within;
    while !ready()? {
        left(clock, until, what)?;
        clock.sleep(PROBE);
    }
    Ok(())
}

/// `fiber` at `fiber` with a cleared environment: `PATH` (the caller's),
/// `HOME` the temporary root, `FIBER_HOME` and the fake provider's key.
/// Nothing else is inherited, so no proxy variable reaches the child and
/// every request goes to `127.0.0.1` directly.
pub(crate) fn command(fiber: &Path, root: &Path, home: &Path, path: Option<&OsStr>) -> Command {
    let mut command = Command::new(fiber);
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", path.unwrap_or_default())
        .env("HOME", root)
        .env("FIBER_HOME", home)
        .env(crate::home::KEY_VAR, "sk-bench");
    command
}

/// A started `fiber`, the leader of its own process group, with a
/// watchdog that kills the group if the harness dies.
pub(crate) struct Proc {
    child: Child,
    group: u32,
    watchdog: Option<Watchdog>,
    /// Read from the clock just before the spawn, after every piece of
    /// harness setup: a startup timing starts here.
    pub(crate) spawned: Instant,
}

impl Proc {
    /// Starts `command` in a new process group.
    pub(crate) fn spawn(command: &mut Command, clock: &dyn Clock) -> Result<Self, String> {
        command.process_group(0);
        let spawned = clock.now();
        let child = command
            .spawn()
            .map_err(|err| format!("starting {:?}: {err}", command.get_program()))?;
        let group = child.id();
        Ok(Self {
            child,
            group,
            watchdog: Some(Watchdog::group(group)),
            spawned,
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.group
    }

    /// Whether the process is still running: a `try_wait` that waits for
    /// nothing.
    pub(crate) fn running(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|err| format!("checking the process: {err}"))
    }

    /// Waits up to `within` of real time for the process to exit, and
    /// reaps it. A real process exits in real time, so the bound is the
    /// system clock's however fast `clock` advances; each probe still waits
    /// on `clock`, which is where a test's clock hears that the wait is
    /// under way.
    pub(crate) fn exits(&mut self, clock: &dyn Clock, within: Duration) -> Result<bool, String> {
        let until = System.now() + within;
        loop {
            let exited = self
                .child
                .try_wait()
                .map(|status| status.is_some())
                .map_err(|err| format!("waiting for the process: {err}"))?;
            if exited {
                return Ok(true);
            }
            if left(&System, until, "the process to exit").is_err() {
                return Ok(false);
            }
            clock.sleep(PROBE);
        }
    }

    /// Ends the process: SIGTERM, then SIGKILL after [`STOP`], then waits
    /// for its group to empty and stands the watchdog down.
    pub(crate) fn stop(mut self, clock: &dyn Clock) -> Result<(), String> {
        if !self.exits(clock, Duration::ZERO)? {
            signal(self.group, "TERM")?;
        }
        if !self.exits(clock, STOP)? {
            signal(self.group, "KILL")?;
            if !self.exits(clock, STOP)? {
                return Err(format!("process {} survived SIGKILL", self.group));
            }
        }
        if !fakes::group_empties(self.group, STOP) {
            signal(self.group, "KILL")?;
            // It returns once the group is seen empty or the bound passes,
            // never while a member may still be dying unreported.
            let after = if fakes::group_empties(self.group, STOP) {
                "SIGKILL emptied it"
            } else {
                "it outlived SIGKILL"
            };
            return Err(format!(
                "process {} left a process in its group behind; {after}",
                self.group
            ));
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(STOP);
        }
        Ok(())
    }
}

/// A command that ran to its end: its exit status and everything it wrote.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Runs `command` in its own process group with stdin closed, waiting up to
/// `within` on `clock` for it to exit, then stops its group and collects
/// its output. Past `within` it errs naming `what`, followed by the
/// group's cleanup error if there was one. Output still open
/// [`STOP`] after the group is gone, held by a process that left the group,
/// is an error rather than a wait.
pub(crate) fn run_to_end(
    command: &mut Command,
    clock: &dyn Clock,
    within: Duration,
    what: &str,
) -> Result<Finished, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut proc = Proc::spawn(command, clock)?;
    // Both pipes are drained from the start, so a full pipe never stalls
    // the child.
    let stdout = read_on_thread(proc.child.stdout.take(), what)?;
    let stderr = read_on_thread(proc.child.stderr.take(), what)?;
    // The wait ends when the child exits or `within` passes; one still
    // running then has no status.
    proc.exits(clock, within)?;
    let status = proc
        .child
        .try_wait()
        .map_err(|err| format!("waiting for {what}: {err}"))?;
    // Signals reach the group in real time, whatever clock times the run.
    let stopped = proc.stop(&System);
    // The deadline is the cause when the run timed out; a failed cleanup
    // follows it rather than replacing it.
    let Some(status) = status else {
        let timed_out = format!("timed out waiting for {what}");
        return Err(match stopped {
            Ok(()) => timed_out,
            Err(cleanup) => format!("{timed_out}; {cleanup}"),
        });
    };
    stopped?;
    let closed = |text: &mpsc::Receiver<String>| {
        text.recv_timeout(STOP)
            .map_err(|_| format!("the output of {what} stayed open"))
    };
    Ok(Finished {
        status,
        stdout: closed(&stdout)?,
        stderr: closed(&stderr)?,
    })
}

/// Runs `command` in its own process group with stdin closed, timing to
/// its stdout closing: the duration is from the spawn to the clock read
/// when the stdout reader reaches EOF, so waiting for the exit is never
/// timed. Past `within` it errs naming `what`. Output still open [`STOP`]
/// after the group is gone, held by a process that left the group, is an
/// error rather than a wait.
pub(crate) fn timed_to_end(
    command: &mut Command,
    clock: &dyn Clock,
    within: Duration,
    what: &str,
) -> Result<(Finished, Duration), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut proc = Proc::spawn(command, clock)?;
    // Both pipes are drained from the start, so a full pipe never stalls
    // the child. The reader threads are detached, and every receive below
    // is bounded, so the call returns even when a process outside the
    // group holds a pipe open.
    let stdout = read_on_thread(proc.child.stdout.take(), what)?;
    let stderr = read_on_thread(proc.child.stderr.take(), what)?;
    let measured = eof_once(&mut proc, stdout, clock, within, what);
    // Signals reach the group in real time, whatever clock times the run.
    let stopped = proc.stop(&System);
    // The deadline is the cause when the run timed out; a failed cleanup
    // follows it rather than replacing it.
    let (status, stdout, took) = match measured {
        Ok(measured) => measured,
        Err(timed_out) => {
            return Err(match stopped {
                Ok(()) => timed_out,
                Err(cleanup) => format!("{timed_out}; {cleanup}"),
            });
        }
    };
    stopped?;
    let stderr = stderr
        .recv_timeout(STOP)
        .map_err(|_| format!("the output of {what} stayed open"))?;
    Ok((
        Finished {
            status,
            stdout,
            stderr,
        },
        took,
    ))
}

/// The stdout text with its closing time, the exit status and the
/// spawn-to-EOF duration: EOF ends the timing, the exit only ends the run.
fn eof_once(
    proc: &mut Proc,
    eof: mpsc::Receiver<String>,
    clock: &dyn Clock,
    within: Duration,
    what: &str,
) -> Result<(ExitStatus, String, Duration), String> {
    let wait = left(clock, proc.spawned + within, what)?;
    let stdout = eof
        .recv_timeout(wait)
        .map_err(|_| format!("timed out waiting for {what}"))?;
    let took = clock.now().saturating_duration_since(proc.spawned);
    // The exit poll only runs after EOF was read.
    proc.exits(clock, within)?;
    match proc
        .child
        .try_wait()
        .map_err(|err| format!("waiting for {what}: {err}"))?
    {
        None => Err(format!("timed out waiting for {what}")),
        Some(status) => Ok((status, stdout, took)),
    }
}

/// Reads `pipe` to its end on a thread, which sends the text once.
fn read_on_thread(
    pipe: Option<impl Read + Send + 'static>,
    what: &str,
) -> Result<mpsc::Receiver<String>, String> {
    let mut pipe = pipe.ok_or_else(|| format!("{what} has no output pipe"))?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        match pipe.read_to_end(&mut bytes) {
            Ok(_) | Err(_) => {}
        }
        match tx.send(String::from_utf8_lossy(&bytes).into_owned()) {
            Ok(()) | Err(_) => {}
        }
    });
    Ok(rx)
}

fn signal(group: u32, signal: &str) -> Result<(), String> {
    fakes::kill_group(group, signal)
        .map(|_| ())
        .map_err(|err| format!("signalling group {group}: {err}"))
}

/// A stdout line, which must be one JSON value.
pub(crate) fn parse_line(line: &str) -> Result<Value, String> {
    serde_json::from_str(line).map_err(|err| format!("a stdout line is not JSON ({err}): {line:?}"))
}

/// Whether a session's startup has finished, fed its stdout lines in order.
/// The loop writes `extensions_loaded`, then starts the status observer,
/// which probes the workspace's git branch and writes its first
/// `session_status` once it has caught up with the log. That line is the
/// last startup work any line shows, so startup has finished at the first
/// `session_status` after `extensions_loaded`; one before it does not count.
#[derive(Default)]
pub(crate) struct Startup {
    loaded: bool,
}

impl Startup {
    /// What a wait for the end of startup names on expiry.
    pub(crate) const WHAT: &'static str = "the first session_status after extensions_loaded";

    /// Takes the next stdout line; true once startup has finished.
    pub(crate) fn line(&mut self, line: &Value) -> bool {
        match line.get("kind").and_then(Value::as_str) {
            Some("extensions_loaded") => {
                self.loaded = true;
                false
            }
            Some("session_status") => self.loaded,
            Some(_) | None => false,
        }
    }
}

/// The internal session command, its stdout read line by line on a thread
/// and its stderr kept for an error.
pub(crate) struct Session {
    pub(crate) proc: Proc,
    lines: mpsc::Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Session {
    /// Starts `command` with stdout and stderr piped.
    pub(crate) fn spawn(command: &mut Command, clock: &dyn Clock) -> Result<Self, String> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut proc = Proc::spawn(command, clock)?;
        let stdout = proc
            .child
            .stdout
            .take()
            .ok_or("the session has no stdout")?;
        let mut stderr_pipe = proc
            .child
            .stderr
            .take()
            .ok_or("the session has no stderr")?;
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let kept = Arc::clone(&stderr);
        thread::spawn(move || {
            let mut text = String::new();
            match stderr_pipe.read_to_string(&mut text) {
                Ok(_) | Err(_) => {}
            }
            if let Ok(mut kept) = kept.lock() {
                *kept = text;
            }
        });
        Ok(Self {
            proc,
            lines,
            stderr,
        })
    }

    /// The next stdout line, waiting until `until`; the error names `what`
    /// and carries the session's stderr so far.
    pub(crate) fn line(
        &self,
        clock: &dyn Clock,
        until: Instant,
        what: &str,
    ) -> Result<Value, String> {
        let wait = left(clock, until, what)?;
        match self.lines.recv_timeout(wait) {
            Ok(line) => parse_line(&line),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("timed out waiting for {what}")),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!(
                "the session's stdout closed before {what}; stderr: {}",
                self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
            )),
        }
    }

    /// Reads stdout lines until the session's startup has finished (see
    /// [`Startup`]), waiting until `until`.
    pub(crate) fn wait_started(&self, clock: &dyn Clock, until: Instant) -> Result<(), String> {
        let mut startup = Startup::default();
        while !startup.line(&self.line(clock, until, Startup::WHAT)?) {}
        Ok(())
    }
}

/// A connection to a session's socket.
pub(crate) struct Client {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Client {
    pub(crate) fn connect(socket: &Path) -> Result<Self, String> {
        let write = UnixStream::connect(socket)
            .map_err(|err| format!("connecting to {}: {err}", socket.display()))?;
        let read = write
            .try_clone()
            .map_err(|err| format!("cloning the socket: {err}"))?;
        Ok(Self {
            write,
            read: BufReader::new(read),
        })
    }

    pub(crate) fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.write, "{line}").map_err(|err| format!("writing to the session: {err}"))
    }

    /// The next socket line, waiting until `until`.
    pub(crate) fn line(
        &mut self,
        clock: &dyn Clock,
        until: Instant,
        what: &str,
    ) -> Result<Value, String> {
        let wait = left(clock, until, what)?;
        self.read
            .get_ref()
            .set_read_timeout(Some(wait))
            .map_err(|err| format!("setting the socket's timeout: {err}"))?;
        let mut line = String::new();
        match self.read.read_line(&mut line) {
            Ok(0) => Err(format!("the session closed the socket before {what}")),
            Ok(_) => serde_json::from_str(&line)
                .map_err(|err| format!("a socket line is not JSON ({err}): {line:?}")),
            Err(err) => Err(format!("waiting for {what}: {err}")),
        }
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
