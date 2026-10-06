use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, Command, Stdio};

use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{
    MATCHING_PATTERN_VAR, MATCHING_WATCHDOG_SCRIPT, WATCHDOG_SCRIPT, kill_group, kill_matching,
    kill_pid, matching, pattern,
};

const DEADLINE: Duration = Duration::from_secs(5);

/// A shell in its own process group whose command line carries `marker`,
/// with a `sleep` child in that group.
fn marked(marker: &str) -> Child {
    Command::new("sh")
        .args(["-c", "sleep 30; :", marker])
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Reaps `child` under [`DEADLINE`], naming `what` on expiry.
fn reaped(mut child: Child, what: &str) -> std::process::ExitStatus {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    match finished.recv_timeout(DEADLINE) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for {what}"),
    }
}

/// A process in the test's own group that a stray group signal would kill.
fn sentinel() -> Child {
    Command::new("sleep").arg("30").spawn().unwrap()
}

/// Whether the sentinel is still running; ends it either way.
fn survived(mut sentinel: Child) -> bool {
    let alive = sentinel.try_wait().unwrap().is_none();
    sentinel.kill().unwrap();
    sentinel.wait().unwrap();
    alive
}

// Refusal tests pass the probe signal `0`: a mutant that lets an id of 1
// or less through then sends `kill -0`, which signals nothing, instead of
// SIGKILL to every process the user owns.
#[test]
fn kill_group_refuses_group_zero_and_one() {
    for group in [0, 1] {
        let sentinel = sentinel();
        let refused = panic::catch_unwind(AssertUnwindSafe(|| kill_group(group, "0")));
        assert!(refused.is_err(), "group {group} was not refused");
        assert!(survived(sentinel), "group {group} signalled the sentinel");
    }
}

#[test]
fn kill_group_names_the_refused_group() {
    let message = *panic::catch_unwind(|| kill_group(1, "0"))
        .unwrap_err()
        .downcast::<String>()
        .unwrap();
    assert!(message.contains("process group 1"), "{message}");
}

#[test]
fn kill_group_signals_a_live_group() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    assert!(kill_group(child.id(), "0").unwrap());
    assert!(kill_group(child.id(), "KILL").unwrap());
    child.wait().unwrap();
}

#[test]
fn kill_pid_refuses_pid_zero_and_one() {
    for pid in [0, 1] {
        let sentinel = sentinel();
        let refused = panic::catch_unwind(AssertUnwindSafe(|| kill_pid(pid, "0")));
        assert!(refused.is_err(), "pid {pid} was not refused");
        assert!(survived(sentinel), "pid {pid} signalled the sentinel");
    }
}

#[test]
fn kill_pid_names_the_refused_pid() {
    let message = *panic::catch_unwind(|| kill_pid(0, "0"))
        .unwrap_err()
        .downcast::<String>()
        .unwrap();
    assert!(message.contains("pid 0"), "{message}");
}

#[test]
fn kill_pid_signals_a_live_process() {
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    assert!(kill_pid(child.id(), "0").unwrap());
    assert!(kill_pid(child.id(), "KILL").unwrap());
    child.wait().unwrap();
}

#[test]
fn the_watchdog_script_refuses_group_zero_and_one() {
    for group in ["0", "1"] {
        let sentinel = sentinel();
        // Null stdin is EOF: unguarded, the script would reach `kill`.
        let status = Command::new("sh")
            .args(["-c", WATCHDOG_SCRIPT, "watchdog", group])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(2), "group {group}");
        assert!(survived(sentinel), "group {group} signalled the sentinel");
    }
}

// The empty-match refusals list processes or run nothing: none signals.
#[test]
fn pattern_refuses_an_empty_match() {
    let message = *panic::catch_unwind(|| pattern(""))
        .unwrap_err()
        .downcast::<&str>()
        .unwrap();
    assert!(message.contains("empty command-line match"), "{message}");
}

#[test]
fn matching_refuses_an_empty_match() {
    assert!(panic::catch_unwind(|| matching("")).is_err());
}

#[test]
fn pattern_escapes_every_regex_metacharacter() {
    assert_eq!(pattern("/tmp/a-b_c"), "/tmp/a-b_c");
    assert_eq!(pattern(r".[]()*+?{}|^$\"), r"\.\[\]\(\)\*\+\?\{\}\|\^\$\\");
}

#[test]
fn matching_finds_a_process_by_its_command_line() {
    let dir = crate::TempDir::new("pm");
    let marker = dir.path().join("a.b").to_string_lossy().into_owned();
    let child = marked(&marker);
    let pid = child.id();
    assert_eq!(matching(&marker).unwrap(), vec![pid]);
    // The dot is literal: a near miss matches nothing.
    let near = marker.replace("a.b", "axb");
    assert!(matching(&near).unwrap().is_empty());
    kill_group(pid, "KILL").unwrap();
    reaped(child, "the marked shell");
    assert!(matching(&marker).unwrap().is_empty());
}

#[test]
fn kill_matching_kills_each_match_and_its_group() {
    let dir = crate::TempDir::new("pk");
    let marker = dir.path().to_string_lossy().into_owned();
    let child = marked(&marker);
    let group = child.id();
    kill_matching(&marker).unwrap();
    assert_eq!(reaped(child, "the killed shell").signal(), Some(9));
    let (done, gone) = mpsc::channel();
    thread::spawn(move || {
        while kill_group(group, "0").unwrap() {
            thread::yield_now();
        }
        done.send(()).unwrap();
    });
    assert!(
        gone.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the shell's group to empty"
    );
}

#[test]
fn the_matching_watchdog_script_refuses_an_empty_pattern() {
    let sentinel = sentinel();
    // Null stdin is EOF: unguarded, the script would reach `pgrep`.
    let status = Command::new("sh")
        .args(["-c", MATCHING_WATCHDOG_SCRIPT, "watchdog"])
        .env(MATCHING_PATTERN_VAR, "")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
    assert!(
        survived(sentinel),
        "the empty pattern signalled the sentinel"
    );
}
