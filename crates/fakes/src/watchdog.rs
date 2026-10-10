//! Kills a process group, or every process matching a command line, if the
//! test process dies (`docs/testing.md`, "Running tests").

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::deadline::Deadline;

/// Kills one process group, or every process matching a command line, when
/// this value is dropped. [`Watchdog::stand_down`] tells it to exit without
/// signalling.
pub struct Watchdog {
    stdin: Option<std::process::ChildStdin>,
    child: Option<Child>,
}

impl Watchdog {
    /// Starts a watchdog for `group`. Dropping it kills the group. The
    /// watchdog's standard input is a pipe only the caller holds: a newline
    /// means stand down, and EOF means the caller died.
    ///
    /// # Panics
    ///
    /// When `group` is 1 or less, before spawning anything, or when the
    /// watchdog cannot be started.
    #[allow(
        clippy::panic,
        reason = "a watchdog that cannot start cannot protect the test"
    )]
    pub fn group(group: u32) -> Self {
        assert!(
            group > 1,
            "refusing to signal process group {group}: kill(-1) signals every process the user owns"
        );
        let group_arg = group.to_string();
        let mut shell = Command::new("sh");
        shell.args(["-c", crate::WATCHDOG_SCRIPT, "watchdog", group_arg.as_str()]);
        Self::spawn(shell)
    }

    /// Starts a watchdog for every process whose command line contains
    /// `text`, and their process groups. Dropping it kills them. The pattern
    /// reaches the watchdog through its environment, so the watchdog's own
    /// command line never matches.
    ///
    /// # Panics
    ///
    /// When `text` is empty, before spawning anything, or when the watchdog
    /// cannot be started.
    pub fn matching(text: &str) -> Self {
        let mut shell = Command::new("sh");
        shell
            .args([
                "-c",
                crate::process_group::MATCHING_WATCHDOG_SCRIPT,
                "watchdog",
            ])
            .env(
                crate::process_group::MATCHING_PATTERN_VAR,
                crate::process_group::pattern(text),
            );
        Self::spawn(shell)
    }

    #[allow(
        clippy::panic,
        reason = "a watchdog that cannot start cannot protect the test"
    )]
    fn spawn(mut shell: Command) -> Self {
        shell
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut spawned = match shell.spawn() {
            Ok(child) => child,
            Err(err) => panic!("the watchdog failed to start: {err}"),
        };
        let stdin = match spawned.stdin.take() {
            Some(stdin) => stdin,
            None => panic!("the watchdog has no standard input"),
        };
        Self {
            stdin: Some(stdin),
            child: Some(spawned),
        }
    }

    /// The processes are done. Tell the watchdog to exit without signalling, and
    /// wait for it at most `within`.
    #[allow(
        clippy::panic,
        reason = "a watchdog that does not exit leaves a process behind"
    )]
    #[track_caller]
    pub fn stand_down(mut self, within: Duration) {
        if let Some(mut stdin) = self.stdin.take() {
            match writeln!(stdin) {
                Ok(()) | Err(_) => {}
            }
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        let (done, finished) = mpsc::channel();
        thread::spawn(move || match done.send(child.wait()) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        });
        assert!(
            Deadline::after(within).recv(&finished).is_ok(),
            "waited {within:?} for the watchdog to exit"
        );
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        // Closing `stdin` is EOF, which tells the watchdog to kill the group.
        // That happens when this value's fields are dropped, including when
        // the test panics. There is nothing to wait on: the test has failed.
    }
}

#[cfg(test)]
#[path = "watchdog_tests.rs"]
mod tests;
