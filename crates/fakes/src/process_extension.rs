//! A shared process-extension fixture: the checked-in example speaking the
//! process extension wire over its stdin and stdout, driven with bounded
//! waits (`docs/testing/fakes.md`).
//!
//! The exchange is one [`ProcessFixture`]: spawn the example in a protocol
//! mode, send it JSON lines, capture the lines it prints, and finish it.
//! Holds are acknowledged: [`ProcessFixture::wait_for`] proves the hold is
//! engaged, [`ProcessFixture::release`] closes stdin, and
//! [`ProcessFixture::finish`] joins only after that release, so a bug fails
//! the test instead of hanging it. Finishing also proves no child remains.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};

use crate::Watchdog;
use crate::deadline::Deadline;
use serde_json::Value;

#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test helper; a failure is the test's"
)]
#[track_caller]
fn example_path() -> crate::WarmedJig {
    let mut path = std::env::current_exe().expect("the test binary knows its path");
    path.pop();
    path.pop();
    let path = path.join("examples").join("process-extension");
    assert!(
        path.exists(),
        "crates/fakes/examples/process-extension is not built; run cargo build -p fakes --example process-extension (scripts/check does this for you)"
    );
    crate::WarmedJig::warm(path, &["bogus-mode"], 2)
}

/// One run of the fixture: the example's captured stdout lines, its stdin,
/// and its reaper. Dropping it without [`ProcessFixture::finish`] kills its
/// process group through the watchdog, so no child survives a failed test.
pub struct ProcessFixture {
    pid: u32,
    input: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    waited: mpsc::Receiver<(std::io::Result<ExitStatus>, String)>,
    watchdog: Watchdog,
}

#[allow(
    clippy::panic,
    clippy::expect_used,
    reason = "a test helper; a failure is the test's"
)]
impl ProcessFixture {
    /// Returns a warmed command for the shared process extension fixture:
    /// the example and its argv. `configuration` is serialized as one
    /// argv value for protocol modes; `echo` and `hold` ignore it.
    #[track_caller]
    pub fn command(mode: &str, configuration: Option<&Value>) -> (PathBuf, Vec<String>) {
        let warmed = example_path();
        let mut fixture_args = vec![mode.to_owned()];
        if let Some(configuration) = configuration {
            fixture_args.push(configuration.to_string());
        }
        (warmed.path().to_path_buf(), fixture_args)
    }

    /// Starts the example in `mode` (`echo` or `hold`). Warms the binary
    /// past its first-exec stall first, so the exchange keeps its own
    /// deadline.
    #[track_caller]
    pub fn spawn(mode: &str) -> Self {
        let command = Self::command(mode, None);
        Self::spawn_command(command)
    }

    /// Starts a protocol-mode peer through the same bounded controller.
    #[track_caller]
    pub fn spawn_protocol(configuration: &Value) -> Self {
        let command = Self::command("peer", Some(configuration));
        Self::spawn_command(command)
    }

    #[track_caller]
    fn spawn_command(command: (PathBuf, Vec<String>)) -> Self {
        let (program, args) = command;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("the process fixture spawns");
        // The child leads its own process group, so the watchdog kills the
        // group and never the test's own; no pid or `pkill` names it.
        let watchdog = Watchdog::group(child.id());
        let pid = child.id();
        let (lines_tx, lines) = mpsc::channel();
        let mut stdout = child.stdout.take().expect("the fixture pipes its stdout");
        std::thread::spawn(move || {
            for line in BufReader::new(&mut stdout).lines() {
                let Ok(line) = line else { break };
                match lines_tx.send(line) {
                    Ok(()) | Err(mpsc::SendError(_)) => {}
                }
            }
        });
        let input = child.stdin.take().expect("the fixture pipes its stdin");
        let mut stderr = child.stderr.take().expect("the fixture pipes its stderr");
        let (done, waited) = mpsc::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let mut errors = String::new();
            stderr.read_to_string(&mut errors).unwrap_or_default();
            match done.send((status, errors)) {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
        });
        Self {
            pid,
            input: Some(input),
            lines,
            waited,
            watchdog,
        }
    }

    /// Sends one JSON line to the fixture.
    #[track_caller]
    pub fn send(&mut self, line: &str) {
        let Some(input) = self.input.as_mut() else {
            panic!("the fixture's stdin is already released");
        };
        if let Err(err) = writeln!(input, "{line}").and_then(|()| input.flush()) {
            panic!("the fixture takes a line: {err}");
        }
    }

    /// The first captured line `keep` takes, waiting at most one wait's own
    /// bound from when the wait starts. A stall fails naming the wait's
    /// bound instead of hanging the test; a closed stdout fails at once.
    #[track_caller]
    pub fn wait_for(
        &self,
        deadline: Deadline,
        what: &str,
        mut keep: impl FnMut(&str) -> bool,
    ) -> String {
        let wait = deadline.start_wait();
        loop {
            match wait.recv(&self.lines) {
                Ok(line) => {
                    if keep(&line) {
                        return line;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("waited {:?} for {what}", wait.left())
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the fixture closed its stdout while waiting for {what}")
                }
            }
        }
    }

    /// Closes the fixture's stdin, so it reads EOF. The release drops
    /// before any join: [`ProcessFixture::finish`] calls this first.
    pub fn release(&mut self) {
        self.input.take();
    }

    /// Releases the hold, waits for the fixture to exit, and proves no
    /// child remains: the exit is awaited under `deadline`, then the group
    /// is killed and must be empty, and the watchdog stands down. Returns
    /// the fixture's stderr, which a clean run leaves empty.
    #[track_caller]
    pub fn finish(mut self, deadline: Deadline) -> String {
        self.release();
        let (status, stderr) = deadline.recv_or_fail(&self.waited, "the fixture to exit");
        let status = status.expect("reaping the fixture");
        assert!(
            status.success(),
            "the fixture exits 0, not {status}: {stderr}"
        );
        match crate::kill_group(self.pid, "KILL") {
            Ok(_) | Err(_) => {}
        }
        assert!(
            crate::group_empties(self.pid, Deadline::after(deadline.cleanup())),
            "the fixture left its process group {} alive",
            self.pid
        );
        self.watchdog
            .stand_down(Deadline::after(deadline.cleanup()));
        assert!(stderr.is_empty(), "the fixture stays silent: {stderr}");
        stderr
    }
}

#[cfg(test)]
#[path = "process_extension_tests.rs"]
mod tests;
