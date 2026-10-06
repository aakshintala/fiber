//! The one way a test signals a process group or a single process
//! (`docs/testing.md`, "Running tests"). `kill(-1)` signals every process the
//! user owns, and `kill(-0)` or `kill -- 0` the caller's own group, so an id
//! of 1 or less is refused before anything runs. The command-line form
//! refuses an empty match, which reaches every process the user owns.

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

/// The variable naming a matching watchdog's pattern: the environment, not
/// the command line, so the watchdog never matches itself.
pub(crate) const MATCHING_PATTERN_VAR: &str = "FIBER_WATCHDOG_PATTERN";

/// The shell script of a matching watchdog, `sh -c MATCHING_WATCHDOG_SCRIPT
/// watchdog` with [`MATCHING_PATTERN_VAR`] set to a [`pattern`]. Reading a
/// line from stdin means the run finished; EOF means the test process died,
/// so the script kills every process whose command line matches, and its
/// process group. An empty pattern, which matches every process, exits with
/// status 2 before `pgrep` can run.
pub(crate) const MATCHING_WATCHDOG_SCRIPT: &str = r#"[ -n "$FIBER_WATCHDOG_PATTERN" ] || exit 2; read -r line || for p in $(pgrep -f -- "$FIBER_WATCHDOG_PATTERN"); do [ "$p" -gt 1 ] || continue; kill -s KILL -- "-$p"; kill -s KILL "$p"; done"#;

/// `text` as an extended regular expression matching itself, for `pgrep -f`.
///
/// # Panics
///
/// When `text` is empty: it matches every process the user owns.
pub(crate) fn pattern(text: &str) -> String {
    assert!(
        !text.is_empty(),
        "refusing an empty command-line match: it matches every process the user owns"
    );
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if ".[]()*+?{}|^$\\".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// The pids of the processes whose command line contains `text`.
///
/// # Errors
///
/// When `pgrep` cannot be run or fails.
///
/// # Panics
///
/// When `text` is empty, before running anything.
pub fn matching(text: &str) -> io::Result<Vec<u32>> {
    let pattern = pattern(text);
    // Checked again on the result: an empty pattern matches every process.
    assert!(
        !pattern.is_empty(),
        "refusing an empty command-line match: it matches every process the user owns"
    );
    let output = Command::new("pgrep")
        .args(["-f", "--", &pattern])
        .stderr(Stdio::null())
        .output()?;
    // 1 is "no process matched".
    match output.status.code() {
        Some(0 | 1) => {}
        _ => return Err(io::Error::other(format!("pgrep failed: {}", output.status))),
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect())
}

/// Sends SIGKILL to every process whose command line contains `text`, and
/// to its process group.
///
/// # Errors
///
/// When `pgrep` or `kill` cannot be run.
///
/// # Panics
///
/// When `text` is empty, before running anything, or when a match is pid 1
/// or less ([`kill_group`], [`kill_pid`]).
pub fn kill_matching(text: &str) -> io::Result<()> {
    for pid in matching(text)? {
        kill_group(pid, "KILL")?;
        kill_pid(pid, "KILL")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "process_group_tests.rs"]
mod tests;
