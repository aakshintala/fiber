use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, ChildStdout, Command, Stdio};

use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{
    MATCHING_PATTERN_VAR, MATCHING_WATCHDOG_SCRIPT, WATCHDOG_SCRIPT, alive, group_empties,
    group_lives, kill_group, kill_matching, kill_pid, listed_exit, matching, matching_exits,
    pattern, pids_exit,
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

#[test]
fn group_empties_keeps_waiting_while_the_group_lives_and_returns_once_it_empties() {
    let mut child = Command::new("sh")
        .args(["-c", "read line"])
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    let stdin = child.stdin.take().unwrap();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || answered.send(group_empties(group, DEADLINE)).unwrap());
    // The group lives until its stdin closes, so the wait must still be
    // running after several probe intervals: a wait that gave up after one
    // live probe would have answered `false` by now.
    assert!(
        answer.recv_timeout(Duration::from_millis(500)).is_err(),
        "group_empties answered while the group was alive"
    );
    drop(stdin);
    reaped(child, "the group leader to exit once its stdin closed");
    assert_eq!(
        answer.recv_timeout(DEADLINE),
        Ok(true),
        "waited {DEADLINE:?} for group_empties to see the emptied group"
    );
}

/// A shell held alive on a stdin pipe the test owns: dropping the pipe ends
/// it, and the test reaps it.
fn held() -> Child {
    Command::new("sh")
        .args(["-c", "read line"])
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
#[should_panic(expected = "refusing")]
fn alive_refuses_pid_zero() {
    alive(0);
}

#[test]
#[should_panic(expected = "refusing")]
fn alive_refuses_pid_one() {
    alive(1);
}

#[test]
#[should_panic(expected = "refusing")]
fn group_probe_refuses_group_zero() {
    group_lives(0);
}

#[test]
#[should_panic(expected = "refusing")]
fn group_probe_refuses_group_one() {
    group_lives(1);
}

#[test]
fn alive_holds_while_the_process_lives_and_falls_once_it_is_reaped() {
    let mut child = held();
    let pid = child.id();
    assert!(alive(pid), "a live child must read as alive");
    drop(child.stdin.take().unwrap());
    reaped(child, "the held child to exit once its stdin closed");
    assert!(!alive(pid), "a reaped child must not read as alive");
}

#[test]
fn group_probe_holds_while_the_group_lives_and_falls_once_it_is_reaped() {
    let mut child = held();
    let group = child.id();
    assert!(group_lives(group), "a live group must read as live");
    drop(child.stdin.take().unwrap());
    reaped(child, "the group leader to exit once its stdin closed");
    assert!(!group_lives(group), "a reaped group must not read as live");
}

#[test]
fn pids_exit_waits_for_both_pids() {
    let mut a = held();
    let mut b = held();
    let pa = a.id();
    let pb = b.id();
    let stdin_a = a.stdin.take().unwrap();
    let stdin_b = b.stdin.take().unwrap();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || answered.send(pids_exit(&[pa, pb], DEADLINE)).unwrap());
    drop(stdin_a);
    reaped(a, "the first child to exit once its stdin closed");
    assert!(
        answer.recv_timeout(Duration::from_millis(200)).is_err(),
        "pids_exit answered while the second child lived"
    );
    drop(stdin_b);
    reaped(b, "the second child to exit once its stdin closed");
    assert_eq!(
        answer.recv_timeout(DEADLINE),
        Ok(true),
        "waited {DEADLINE:?} for both pids to exit"
    );
}

#[test]
fn pids_exit_is_false_while_a_pid_lives() {
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    assert!(
        !pids_exit(&[pid], Duration::from_millis(200)),
        "a live pid must not read as exited"
    );
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
}

#[test]
fn matching_exits_is_true_when_nothing_matches() {
    let dir = crate::TempDir::new("px");
    let marker = dir
        .path()
        .join("nothing-matches-this-marker")
        .to_string_lossy()
        .into_owned();
    assert!(
        matching_exits(&marker, DEADLINE),
        "waited {DEADLINE:?} for nothing to match"
    );
}

#[test]
fn matching_exits_is_false_while_a_match_lives() {
    let dir = crate::TempDir::new("py");
    let marker = dir.path().to_string_lossy().into_owned();
    let child = marked(&marker);
    let group = child.id();
    assert!(
        !matching_exits(&marker, Duration::from_millis(200)),
        "a live match must not read as exited"
    );
    kill_group(group, "KILL").unwrap();
    reaped(child, "the killed marked shell");
}

#[test]
fn matching_exits_waits_for_a_live_match() {
    let dir = crate::TempDir::new("pz");
    let marker = dir.path().to_string_lossy().into_owned();
    let child = marked(&marker);
    let group = child.id();
    let marker_text = marker.clone();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        answered
            .send(matching_exits(&marker_text, DEADLINE))
            .unwrap()
    });
    assert!(
        answer.recv_timeout(Duration::from_millis(200)).is_err(),
        "matching_exits answered while the match lived"
    );
    kill_group(group, "KILL").unwrap();
    reaped(child, "the killed marked shell");
    assert_eq!(
        answer.recv_timeout(DEADLINE),
        Ok(true),
        "waited {DEADLINE:?} for the match to exit"
    );
}

#[test]
fn listed_exit_is_false_when_the_second_listing_still_matches() {
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let mut calls = 0;
    let listed = listed_exit(
        move || {
            calls += 1;
            if calls == 1 {
                Ok(vec![])
            } else {
                Ok(vec![pid])
            }
        },
        DEADLINE,
    );
    assert!(
        !listed,
        "a second listing that still matches must read as live"
    );
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
}

#[test]
fn listed_exit_is_true_when_the_second_listing_is_empty() {
    let mut child = held();
    let pid = child.id();
    drop(child.stdin.take().unwrap());
    reaped(child, "the held child to exit once its stdin closed");
    let mut calls = 0;
    let listed = listed_exit(
        move || {
            calls += 1;
            if calls == 1 {
                Ok(vec![pid])
            } else {
                Ok(vec![])
            }
        },
        DEADLINE,
    );
    assert!(listed, "waited {DEADLINE:?} for the listed pid to exit");
}
