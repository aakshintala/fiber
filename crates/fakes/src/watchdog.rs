//! Kills a process group if the test process dies (`docs/testing.md`, "Running tests").

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Kills one process group when this value is dropped. [`Watchdog::stand_down`]
/// tells it to exit without signalling.
pub struct Watchdog {
    stdin: Option<std::process::ChildStdin>,
    child: Option<Child>,
}

impl Watchdog {
    /// Starts a watchdog for `group`. Dropping it kills the group. The
    /// watchdog's standard input is a pipe only the caller holds: a newline
    /// means stand down, and EOF means the caller died.
    #[allow(
        clippy::panic,
        reason = "a watchdog that cannot start cannot protect the test"
    )]
    pub fn group(group: u32) -> Self {
        let group_arg = group.to_string();
        let mut shell = Command::new("sh");
        shell
            .args([
                "-c",
                r#"read -r line || kill -s KILL -- "-$1""#,
                "watchdog",
                group_arg.as_str(),
            ])
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

    /// The group is done. Tell the watchdog to exit without signalling, and
    /// wait for it at most `within`.
    #[allow(
        clippy::panic,
        reason = "a watchdog that does not exit leaves a process behind"
    )]
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
            finished.recv_timeout(within).is_ok(),
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
