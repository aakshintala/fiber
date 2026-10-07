use std::io::{BufRead, BufReader};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::Watchdog;
use crate::kill_group;

const DEADLINE: Duration = Duration::from_secs(5);

fn group_alive(group: u32) -> bool {
    kill_group(group, "0").unwrap()
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

#[test]
fn group_refuses_group_zero_and_one() {
    for group in [0, 1] {
        let sentinel = sentinel();
        let refused = panic::catch_unwind(AssertUnwindSafe(|| Watchdog::group(group)));
        assert!(refused.is_err(), "group {group} was not refused");
        assert!(survived(sentinel), "group {group} signalled the sentinel");
    }
}

#[test]
fn dropping_the_watchdog_kills_the_group() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    drop(Watchdog::group(group));
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match finished.recv_timeout(DEADLINE) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for the process group to die"),
    };
    assert_eq!(status.signal(), Some(9));
    assert!(!group_alive(group));
}

#[test]
fn stand_down_leaves_the_group_alive() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    Watchdog::group(group).stand_down(DEADLINE);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    // Long enough that dropping the watchdog would have killed the group.
    assert!(
        finished.recv_timeout(DEADLINE).is_err(),
        "the group died after stand_down"
    );
    match kill_group(group, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the slept process to exit"
    );
}

#[test]
fn matching_refuses_an_empty_match() {
    let refused = panic::catch_unwind(AssertUnwindSafe(|| Watchdog::matching("")));
    assert!(refused.is_err());
}

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
                    Err(_) => break,
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

#[test]
fn dropping_a_matching_watchdog_kills_every_match() {
    let dir = crate::TempDir::new("wm");
    let marker = dir.path().to_string_lossy().into_owned();
    let mut child = marked(&marker);
    let out = child.stdout.take().unwrap();
    let piped = Piped::new(out);
    piped.forked("the sleep to fork");
    drop(Watchdog::matching(&marker));
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match finished.recv_timeout(DEADLINE) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for the matching process to die"),
    };
    assert_eq!(status.signal(), Some(9));
    piped.closed("the matching process's group to empty");
}

#[test]
fn stand_down_leaves_matching_processes_alive() {
    let dir = crate::TempDir::new("ws");
    let marker = dir.path().to_string_lossy().into_owned();
    let mut child = marked(&marker);
    let group = child.id();
    Watchdog::matching(&marker).stand_down(DEADLINE);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    // Long enough that dropping the watchdog would have killed the match.
    assert!(
        finished.recv_timeout(DEADLINE).is_err(),
        "the matching process died after stand_down"
    );
    match kill_group(group, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the matching process to exit"
    );
}
