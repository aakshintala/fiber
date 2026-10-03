//! The one way a test signals a process group or a single process
//! (`docs/testing.md`, "Running tests"). `kill(-1)` signals every process the
//! user owns, and `kill(-0)` or `kill -- 0` the caller's own group, so an id
//! of 1 or less is refused before anything runs.

use std::io;
use std::process::{Command, Stdio};

/// The shell script of a watchdog: `sh -c WATCHDOG_SCRIPT watchdog <group>`.
/// Reading a line from stdin means the run finished; EOF means the test
/// process died, so the script kills `<group>`. An argument of 1 or less
/// exits with status 2 before `kill` can run.
pub const WATCHDOG_SCRIPT: &str =
    r#"[ "$1" -gt 1 ] || exit 2; read -r line || kill -s KILL -- "-$1""#;

/// Sends `signal` (a name such as `KILL`, or `0` to probe) to process group
/// `group` and returns whether `kill` succeeded. An error means `kill` could
/// not be run at all, so a probe must not read it as an empty group.
///
/// # Errors
///
/// When the `kill` command cannot be started.
///
/// # Panics
///
/// When `group` is 1 or less, before running anything.
pub fn kill_group(group: u32, signal: &str) -> io::Result<bool> {
    assert!(
        group > 1,
        "refusing to signal process group {group}: kill(-1) signals every process the user owns"
    );
    Command::new("kill")
        .args([format!("-{signal}"), "--".into(), format!("-{group}")])
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
}

/// Sends `signal` (a name such as `KILL`, or `0` to probe) to process `pid`
/// and returns whether `kill` succeeded. An error means `kill` could not be
/// run at all, so a probe must not read it as a dead process.
///
/// # Errors
///
/// When the `kill` command cannot be started.
///
/// # Panics
///
/// When `pid` is 1 or less, before running anything. `kill -- 0` signals the
/// caller's own process group.
pub fn kill_pid(pid: u32, signal: &str) -> io::Result<bool> {
    assert!(
        pid > 1,
        "refusing to signal pid {pid}: kill -- 0 signals the caller's own process group"
    );
    Command::new("kill")
        .args([format!("-{signal}"), "--".into(), pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
}

#[cfg(test)]
#[path = "process_group_tests.rs"]
mod tests;
