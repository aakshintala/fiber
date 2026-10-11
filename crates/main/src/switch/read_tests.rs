//! `Reads`: a read on its own thread, a cancel that ends the wait, and the
//! credential commands it lists and kills.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::ErrorCode;

use super::{Reads, signallable};
use fakes::Deadline;

/// The deadline of each wait in these tests.
const DEADLINE: Duration = Duration::from_secs(10);

/// `sh -c <script>` with stdin and stderr null and stdout piped, as a
/// credential source's built command.
fn sh(script: &str) -> Command {
    let mut command = Command::new("sh");
    command
        .args(["-c", script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    command
}

/// The groups `reads` lists now.
fn listed(reads: &Reads) -> Vec<u32> {
    reads.lock().groups.clone()
}

/// Waits under [`DEADLINE`] for `file` to hold a line, and returns it.
#[track_caller]
fn ready_line(file: &std::path::Path) -> String {
    let file = file.to_path_buf();
    fakes::within("the ready file", DEADLINE, move || {
        loop {
            if let Ok(text) = std::fs::read_to_string(&file)
                && text.ends_with('\n')
            {
                return text.trim().to_owned();
            }
            std::thread::yield_now();
        }
    })
}

#[test]
fn run_returns_the_reads_value() {
    let reads = Reads::default();
    let value = fakes::within("the read", DEADLINE, move || reads.run(|| 7));
    assert_eq!(value.unwrap(), 7);
}

#[test]
fn cancel_ends_a_blocked_run_with_closing_and_a_late_value_changes_nothing() {
    let reads = Arc::new(Reads::default());
    let (open, gate) = mpsc::channel::<()>();
    let (started, began) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let waiter = {
        let (reads, finished) = (Arc::clone(&reads), Arc::clone(&finished));
        std::thread::spawn(move || {
            reads.run(move || {
                started.send(()).unwrap();
                gate.recv().unwrap_or(());
                finished.store(true, Ordering::SeqCst);
                9
            })
        })
    };
    Deadline::after(DEADLINE)
        .recv(&began)
        .expect("the read started");
    reads.cancel();
    let rejected = fakes::within("the cancelled run", DEADLINE, move || {
        waiter.join().unwrap()
    });
    let failure = rejected.expect_err("a cancelled read is no value");
    assert_eq!(failure.code, ErrorCode::Closing);
    assert_eq!(failure.message, "The session is shutting down.");
    // The read finishes after the cancel; nobody receives its value.
    open.send(()).unwrap();
    assert!(reads.lock().waiting.is_empty());
}

#[test]
fn cancel_ends_a_blocked_run_started_before_a_quick_one_with_closing() {
    let reads = Arc::new(Reads::default());
    let (open, gate) = mpsc::channel::<()>();
    let (started, began) = mpsc::channel();
    let waiter = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || {
            reads.run(move || {
                started.send(()).unwrap();
                gate.recv().unwrap_or(());
                9
            })
        })
    };
    Deadline::after(DEADLINE)
        .recv(&began)
        .expect("the read started");
    // A second run takes the next id; under the `*=` mutant it reuses id 0,
    // replaces A's wake, and its finish removes A's entry, so the cancel
    // below never wakes A.
    let probe = Arc::clone(&reads);
    let quick = fakes::within("the quick run", DEADLINE, move || probe.run(|| 1));
    assert_eq!(quick.unwrap(), 1);
    reads.cancel();
    let rejected = fakes::within("the cancelled run", DEADLINE, move || {
        waiter.join().unwrap()
    });
    let failure = rejected.expect_err("a cancelled read is no value");
    assert_eq!(failure.code, ErrorCode::Closing);
    assert_eq!(failure.message, "The session is shutting down.");
    // The read finishes after the cancel; nobody receives its value.
    open.send(()).unwrap();
    assert!(reads.lock().waiting.is_empty());
}

#[test]
fn run_after_cancel_is_closing_without_running_the_read() {
    let reads = Reads::default();
    reads.cancel();
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let failure = fakes::within("the refused read", DEADLINE, move || {
        reads.run(move || flag.store(true, Ordering::SeqCst))
    })
    .expect_err("a cancelled read");
    assert_eq!(failure.code, ErrorCode::Closing);
    assert!(!ran.load(Ordering::SeqCst), "the read never ran");
}

#[test]
fn a_command_leads_its_own_group_and_is_listed_until_the_group_empties() {
    let root = fakes::TempDir::new("fiber-read-group");
    let ready = root.path().join("ready");
    let go = root.path().join("go");
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo \"$$ $(ps -o pgid= -p $$ | tr -d ' ')\" > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; echo key",
        ready.display(),
        go.display()
    );
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&go.display().to_string());
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let line = ready_line(&ready);
    let (pid, pgid) = line.split_once(' ').unwrap();
    let pid: u32 = pid.parse().unwrap();
    let watchdog = fakes::Watchdog::group(pid);
    assert_eq!(pid.to_string(), pgid, "the command leads its own group");
    assert_eq!(listed(&reads), vec![pid], "listed while it runs");
    std::fs::write(&go, "").unwrap();
    let output = fakes::within("the command", DEADLINE, move || running.join().unwrap()).unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "key\n");
    assert!(fakes::group_empties(pid, DEADLINE));
    // The next command prunes the empty group.
    let probe = Arc::clone(&reads);
    let pruned = fakes::within("the pruning command", DEADLINE, move || {
        probe.command(&mut sh("true"))
    });
    pruned.unwrap();
    assert!(!listed(&reads).contains(&pid), "{:?}", listed(&reads));
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn cancel_kills_a_running_command_and_it_errs() {
    let root = fakes::TempDir::new("fiber-read-cancel");
    let ready = root.path().join("ready");
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 30", ready.display());
    // Before the spawn, on the ready path the command line carries until
    // it execs; the group watchdog below takes over once the pid is known.
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pid: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(pid);
    reads.cancel();
    let error = fakes::within("the killed command", DEADLINE, move || {
        running.join().unwrap()
    })
    .expect_err("a killed command");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(fakes::group_empties(pid, DEADLINE));
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn a_descendant_keeps_its_group_listed_and_cancel_kills_it() {
    let root = fakes::TempDir::new("fiber-read-descendant");
    let ready = root.path().join("ready");
    let reads = Arc::new(Reads::default());
    let script = format!(
        "sleep 30 >/dev/null 2>&1 & echo $$ > '{}'; echo key",
        ready.display()
    );
    // Before the spawn: the shell exits fast and leaves `sleep` behind,
    // so only a command-line watchdog covers the spawn itself.
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    let first = Arc::clone(&reads);
    let output = fakes::within("the first command", DEADLINE, move || {
        first.command(&mut sh(&script)).unwrap()
    });
    assert_eq!(String::from_utf8_lossy(&output.stdout), "key\n");
    let group: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(group);
    assert!(!fakes::group_empties(group, Duration::ZERO), "sleep lives");
    // A later command prunes only empty groups: `sleep` keeps this one.
    let probe = Arc::clone(&reads);
    fakes::within("the pruning command", DEADLINE, move || {
        probe.command(&mut sh("true")).unwrap()
    });
    assert!(listed(&reads).contains(&group), "{:?}", listed(&reads));
    reads.cancel();
    assert!(fakes::group_empties(group, DEADLINE), "cancel killed sleep");
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn a_cancel_between_a_read_starting_and_its_spawn_starts_no_command() {
    let root = fakes::TempDir::new("fiber-read-interleave");
    let marker = root.path().join("ran");
    let reads = Arc::new(Reads::default());
    // No command starts here; the watchdog only fires if one ever does.
    let pre = fakes::Watchdog::matching(&marker.display().to_string());
    let (open, gate) = mpsc::channel::<()>();
    let (started, began) = mpsc::channel();
    let job = {
        let reads = Arc::clone(&reads);
        let script = format!("echo x > '{}'; echo key", marker.display());
        std::thread::spawn(move || {
            started.send(()).unwrap();
            gate.recv().unwrap();
            reads.command(&mut sh(&script))
        })
    };
    Deadline::after(DEADLINE)
        .recv(&began)
        .expect("the job started");
    reads.cancel();
    open.send(()).unwrap();
    let error = fakes::within("the refused command", DEADLINE, move || job.join().unwrap())
        .expect_err("a cancelled command");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(!marker.exists(), "the command never started");
    assert!(listed(&reads).is_empty());
    pre.stand_down(DEADLINE);
}

#[test]
fn only_a_group_above_one_is_signallable() {
    assert!(!signallable(0));
    assert!(!signallable(1));
    assert!(signallable(2));
}
