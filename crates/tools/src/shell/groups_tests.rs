//! The live-group list: a command's group is listed from its spawn until
//! its run saw it empty, and [`kill_every_group`](super::kill_every_group)
//! kills what is listed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::children::{Ready, ignores_sigterm};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder, Watchdog, kill_group};

use super::super::command::{MovePolicy, Ran, execute};
use super::{finished, kill_every_group, listed, register};

/// How long a test waits on a command before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// An id no process holds in these tests: the list is only read and
/// written, never signalled, for it.
const UNUSED: u32 = 999_999_001;

/// Runs `command` in the foreground with no move, on its own thread, and
/// returns how it finished.
fn run(dir: &std::path::Path, command: String) -> mpsc::Receiver<super::super::command::Finished> {
    let dir = dir.to_path_buf();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let clock = FakeClock::new();
        let cancel = CancelToken::new();
        let ran = execute(
            std::path::Path::new("/bin/sh"),
            &command,
            &dir,
            Duration::from_secs(3600),
            clock.as_ref(),
            &cancel,
            &Recorder::default(),
            MovePolicy::Stay,
            None,
        )
        .unwrap();
        let Ran::Finished(finished) = ran else {
            panic!("a command that may not move moved");
        };
        let _sent = tx.send(finished);
    });
    rx
}

#[test]
fn a_group_seen_empty_leaves_the_list_and_an_occupied_one_stays() {
    register(UNUSED);
    finished(UNUSED, false);
    assert!(listed().contains(&UNUSED));
    finished(UNUSED, true);
    assert!(!listed().contains(&UNUSED));
}

#[test]
fn a_command_is_listed_while_it_runs_and_not_once_it_ends() {
    let dir = fakes::TempDir::new("fiber-groups-listed");
    let ready = Ready::new(dir.path());
    let done = run(dir.path(), ignores_sigterm(ready.path()));
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(listed().contains(&pgid), "the running group is listed");

    kill_every_group();
    // The run ends on its own pass: no cancel, no clock move.
    let finished = done.recv_timeout(DEADLINE).expect("the run ended");
    assert_eq!(finished.status.and_then(|status| status.signal()), Some(9));
    assert_eq!(finished.stop, None);
    assert!(!kill_group(pgid, "0").unwrap(), "the group is gone");
    assert!(!listed().contains(&pgid), "an empty group leaves the list");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_finished_command_is_not_listed() {
    let dir = fakes::TempDir::new("fiber-groups-done");
    let ready = Ready::new(dir.path());
    let done = run(
        dir.path(),
        format!("echo $$ > '{}'", ready.path().display()),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let finished = done.recv_timeout(DEADLINE).expect("the run ended");
    assert_eq!(finished.status.and_then(|status| status.code()), Some(0));
    assert!(!listed().contains(&pgid));
}

#[test]
fn kill_every_group_kills_a_listed_group_and_then_drops_it() {
    let mut child = Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = child.id();
    let watchdog = Watchdog::group(pgid);
    register(pgid);

    kill_every_group();
    let (done, waited) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = waited
        .recv_timeout(DEADLINE)
        .expect("the killed child was reaped")
        .unwrap();
    assert_eq!(status.signal(), Some(9));
    // Reaped, the group is empty: the next kill drops it unsignalled.
    assert!(listed().contains(&pgid));
    kill_every_group();
    assert!(!listed().contains(&pgid));
    watchdog.stand_down(DEADLINE);
}
