//! Starting `fiber`: the environment it runs under, its own process group
//! under a watchdog, stdout read line by line under deadlines, a socket
//! client, and a bounded stop (`docs/testing.md`, "Running tests" and
//! "Waits and timeouts").

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
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
const PROBE: Duration = Duration::from_millis(5);

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
}

impl Proc {
    /// Starts `command` in a new process group.
    pub(crate) fn spawn(command: &mut Command) -> Result<Self, String> {
        let child = command
            .process_group(0)
            .spawn()
            .map_err(|err| format!("starting {:?}: {err}", command.get_program()))?;
        let group = child.id();
        Ok(Self {
            child,
            group,
            watchdog: Some(Watchdog::group(group)),
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.group
    }

    /// Waits up to `within` for the process to exit, and reaps it.
    pub(crate) fn exits(&mut self, clock: &dyn Clock, within: Duration) -> Result<bool, String> {
        let exited = poll(clock, within, "the process to exit", || {
            self.child
                .try_wait()
                .map(|status| status.is_some())
                .map_err(|err| format!("waiting for the process: {err}"))
        });
        Ok(exited.is_ok())
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
            return Err(format!(
                "process {} left a process in its group behind",
                self.group
            ));
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(STOP);
        }
        Ok(())
    }
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

/// The internal session command, its stdout read line by line on a thread
/// and its stderr kept for an error.
pub(crate) struct Session {
    pub(crate) proc: Proc,
    lines: mpsc::Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Session {
    /// Starts `command` with stdout and stderr piped.
    pub(crate) fn spawn(command: &mut Command) -> Result<Self, String> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut proc = Proc::spawn(command)?;
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

    /// Reads stdout lines until one of `kind`, waiting until `until`.
    pub(crate) fn wait_for(
        &self,
        clock: &dyn Clock,
        until: Instant,
        kind: &str,
    ) -> Result<(), String> {
        while self
            .line(clock, until, kind)?
            .get("kind")
            .and_then(Value::as_str)
            != Some(kind)
        {}
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
