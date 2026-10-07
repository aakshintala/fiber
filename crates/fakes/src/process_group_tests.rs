use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, ChildStdout, Command, Stdio};

use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{
    MATCHING_PATTERN_VAR, MATCHING_WATCHDOG_SCRIPT, WATCHDOG_SCRIPT, group_empties, kill_group,
    kill_matching, kill_pid, matching, pattern,
};

const DEADLINE: Duration = Duration::from_secs(5);

/// A shell in its own process group whose command line carries `marker`.
/// It forks `sleep 30` into the group, echoes the sleep's pid, then waits:
/// the sleep holds the piped stdout, and the pid line follows its fork.
fn marked(marker: &str) -> Child {
    Command::new("sh")
        .args(["-c", "sleep 30 & echo $!; wait", marker])
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Lines of a `marked` shell's stdout, ending at end-of-file.
struct Piped(mpsc::Receiver<Option<String>>);

impl Piped {
    fn new(out: ChildStdout) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(out);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(Some(line)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            match tx.send(None) {
                Ok(()) | Err(_) => {}
            }
        });
        Self(rx)
    }

    /// Waits [`DEADLINE`] for the forked `sleep`'s pid line.
    fn forked(&self, what: &str) {
        match self.0.recv_timeout(DEADLINE) {
            Ok(Some(_)) => {}
            _ => panic!("waited {DEADLINE:?} for {what}"),
        }
    }

    /// Waits [`DEADLINE`] for end-of-file, proving the `sleep` exited.
    fn closed(self, what: &str) {
        loop {
            match self.0.recv_timeout(DEADLINE) {
                Ok(Some(_)) => {}
                Ok(None) => return,
                Err(_) => panic!("waited {DEADLINE:?} for {what}"),
            }
        }
    }
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
    let guard = KillOnDrop(pid);
    // The shell's forked `sleep` may briefly carry the same command line,
    // so the shell's pid is asserted present, not alone.
    assert!(matching(&marker).unwrap().contains(&pid));
    // The dot is literal: a near miss matches nothing.
    let near = marker.replace("a.b", "axb");
    assert!(matching(&near).unwrap().is_empty());
    drop(guard);
    reaped(child, "the marked shell");
    assert!(matching(&marker).unwrap().is_empty());
}

/// Kills a process group on drop, so a failed assertion leaves nothing
/// running.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        kill_group(self.0, "KILL").unwrap_or(false);
    }
}

#[test]
fn kill_matching_kills_each_match_and_its_group() {
    let dir = crate::TempDir::new("pk");
    let marker = dir.path().to_string_lossy().into_owned();
    let mut child = marked(&marker);
    let out = child.stdout.take().unwrap();
    let piped = Piped::new(out);
    piped.forked("the sleep to fork");
    kill_matching(&marker).unwrap();
    assert_eq!(reaped(child, "the killed shell").signal(), Some(9));
    piped.closed("the shell's group to empty");
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

#[test]
fn group_empties_reports_an_empty_group_and_a_live_one() {
    let child = marked("group_empties");
    let group = child.id();
    assert!(
        !group_empties(group, Duration::from_millis(200)),
        "a live group must not read as empty"
    );
    kill_group(group, "KILL").unwrap();
    reaped(child, "the killed group leader");
    assert!(
        group_empties(group, DEADLINE),
        "waited {DEADLINE:?} for the killed group to empty"
    );
}
