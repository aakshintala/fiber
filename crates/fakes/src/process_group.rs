//! The one way a test signals a process group or a single process
//! (`docs/testing.md`, "Running tests"). `kill(-1)` signals every process the
//! user owns, and `kill(-0)` or `kill -- 0` the caller's own group, so an id
//! of 1 or less is refused before anything runs. The command-line form
//! refuses an empty match, which reaches every process the user owns.
//!
//! The exit probes (`alive`, `group_lives`) and the waits built on them
//! (`pids_exit`, `matching_exits`) learn through the safe `kill(pid, 0)`
//! probes, starting no process per pass.

use std::io;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rustix::process::Pid;

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

/// Waits up to `deadline` on the wall clock for process group `group` to
/// empty, and returns whether it did. One probe right after the group's
/// leader exits is a race: a transient child, such as a `cat` in a command
/// substitution, can outlive it for a moment under load. The probes run on
/// a thread, so the deadline holds even when a probe is slow. Any probe
/// error counts as empty, as `kill`'s non-zero exit did.
///
/// # Panics
///
/// When `group` is 1 or less (see [`kill_group`]), on the probe thread, so the
/// wait then returns `false`.
#[must_use]
pub fn group_empties(group: u32, deadline: Duration) -> bool {
    let (emptied, empty) = mpsc::channel();
    let (stop, stopped) = mpsc::channel::<()>();
    thread::spawn(move || {
        while group_lives(group) {
            if !matches!(
                stopped.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                return;
            }
        }
        match emptied.send(()) {
            Ok(()) | Err(_) => {}
        }
    });
    let result = empty.recv_timeout(deadline).is_ok();
    drop(stop);
    result
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

/// Whether `pid` exists: kill(pid, 0), starting no process. Panics when
/// `pid` is 1 or less.
fn alive(pid: u32) -> bool {
    assert!(
        pid > 1,
        "refusing to probe pid {pid}: kill -- 0 signals the caller's own process group"
    );
    let raw = i32::try_from(pid).ok().and_then(Pid::from_raw);
    assert!(raw.is_some(), "pid {pid} does not fit in an i32");
    match raw {
        Some(id) => rustix::process::test_kill_process(id).is_ok(),
        None => false,
    }
}

/// Whether process group `group` has a member: kill(-group, 0), starting no
/// process. Panics when `group` is 1 or less, with kill_group's message.
fn group_lives(group: u32) -> bool {
    assert!(
        group > 1,
        "refusing to signal process group {group}: kill(-1) signals every process the user owns"
    );
    let raw = i32::try_from(group).ok().and_then(Pid::from_raw);
    assert!(
        raw.is_some(),
        "process group {group} does not fit in an i32"
    );
    match raw {
        Some(id) => rustix::process::test_kill_process_group(id).is_ok(),
        None => false,
    }
}

/// The probe loop: true once every pid fails kill(pid, 0); false once
/// `stop` disconnects.
fn wait_exits(pids: &[u32], stop: &mpsc::Receiver<()>) -> bool {
    loop {
        if !pids.iter().any(|pid| alive(*pid)) {
            return true;
        }
        match stop.try_recv() {
            Err(mpsc::TryRecvError::Empty) => {}
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return false,
        }
        thread::yield_now();
    }
}

/// Waits up to `deadline` on the wall clock for every pid in `pids` to exit,
/// probing with kill(pid, 0) and starting no process. Runs on a thread.
/// Panics (on that thread, so the result is `false`) when a pid is 1 or
/// less.
#[must_use]
pub fn pids_exit(pids: &[u32], deadline: Duration) -> bool {
    let pids = pids.to_vec();
    let (done, finished) = mpsc::channel::<bool>();
    let (stop, stopped) = mpsc::channel::<()>();
    thread::spawn(move || {
        let exited = wait_exits(&pids, &stopped);
        match done.send(exited) {
            Ok(()) | Err(_) => {}
        }
    });
    let result = finished.recv_timeout(deadline).unwrap_or_default();
    drop(stop);
    result
}

/// `matching_exits` with the listing injected: `list` is called once before
/// the probes and once after. The test seam for the final check.
fn listed_exit(
    list: impl FnMut() -> io::Result<Vec<u32>> + Send + 'static,
    deadline: Duration,
) -> bool {
    let (done, finished) = mpsc::channel::<bool>();
    let (stop, stopped) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut list = list;
        let first = match list() {
            Ok(pids) => pids,
            Err(_) => {
                match done.send(false) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
        };
        if !wait_exits(&first, &stopped) {
            match done.send(false) {
                Ok(()) | Err(_) => {}
            }
            return;
        }
        let last = match list() {
            Ok(pids) => pids,
            Err(_) => {
                match done.send(false) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
        };
        match done.send(last.is_empty()) {
            Ok(()) | Err(_) => {}
        }
    });
    let result = finished.recv_timeout(deadline).unwrap_or_default();
    drop(stop);
    result
}

/// Waits up to `deadline` on the wall clock for every process whose command
/// line contains `text` to exit: one `pgrep` lists the matches, the probes
/// wait for each listed pid, and a second `pgrep` checks that nothing
/// matches. One thread, one deadline, at most two `pgrep`s.
#[must_use]
pub fn matching_exits(text: &str, deadline: Duration) -> bool {
    let text = text.to_owned();
    listed_exit(move || matching(&text), deadline)
}

#[cfg(test)]
#[path = "process_group_tests.rs"]
mod tests;
