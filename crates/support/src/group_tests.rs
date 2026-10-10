//! The refusal, the probe, the signals, the list, the spawn window, the
//! shared kill and the signal names. No test passes 0 or 1 to [`signal`](super::signal)
//! or [`signal_process`](super::signal_process): the refusal is pinned through
//! [`refused`](super::refused) alone, and [`alive`](super::alive) of 0 or 1
//! sends nothing, so a mutant that removes the check sends nothing either.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use fakes::children::{Ready, ignores_sigterm};
use fakes::{Deadline, TempDir, Watchdog, kill_group};
use rustix::process::Signal;

use super::{
    Error, GROUP_POLL, alive, kill_every_group, live, refused, signal, signal_name, signal_process,
    spawn,
};

/// How long a test waits on a child before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// An id no process holds in these tests: above `pid_max` on Linux and
/// macOS, so the list is only read and written for it, never signalled.
const UNUSED: u32 = 999_999_001;

/// A second such id, so two list-only tests never share an entry.
const UNUSED_TWO: u32 = 999_999_002;

/// An id past what a pid holds: no conversion reaches a syscall for it.
const PAST_PID: u32 = u32::MAX;

/// Starts `sleep 60` as a group leader and returns the child and its group.
fn sleep() -> std::process::Child {
    Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("sleep started")
}

/// Reaps `child` on a thread: the wait always ends under [`DEADLINE`].
fn reap(mut child: std::process::Child) -> std::process::ExitStatus {
    let (done, waited) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(child.wait());
    });
    Deadline::after(DEADLINE)
        .recv(&waited)
        .expect("the child answered")
        .expect("the wait succeeded")
}

#[test]
fn refused_pins_the_boundary_without_signalling() {
    assert!(refused(0));
    assert!(refused(1));
    assert!(!refused(2));
    assert!(!refused(u32::MAX));
}

#[test]
fn alive_sends_nothing_for_a_refused_id() {
    assert!(!alive(0));
    assert!(!alive(1));
    assert!(!alive(PAST_PID));
}

#[test]
fn signal_kill_ends_a_live_group() {
    let child = sleep();
    let pgid = child.id();
    let watchdog = Watchdog::group(pgid);
    assert!(alive(pgid), "the running group probes live");

    assert!(signal(pgid, Signal::KILL).is_ok(), "the kill landed");
    assert_eq!(reap(child).signal(), Some(9));
    assert!(!alive(pgid), "the reaped group probes empty");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn signal_to_a_gone_group_is_ok() {
    assert!(
        signal(UNUSED, Signal::TERM).is_ok(),
        "a gone group reads as Ok"
    );
}

#[test]
fn signal_refuses_an_id_past_a_pid() {
    assert!(matches!(signal(PAST_PID, Signal::TERM), Err(Error::Refused(id)) if id == PAST_PID));
    assert!(matches!(
        signal_process(PAST_PID, Signal::TERM),
        Err(Error::Refused(id)) if id == PAST_PID
    ));
}

#[test]
fn signal_process_term_ends_a_plain_child() {
    let child = sleep();
    let pid = child.id();
    let watchdog = Watchdog::group(pid);
    assert!(signal_process(pid, Signal::TERM).is_ok(), "the kill landed");
    assert_eq!(reap(child).signal(), Some(Signal::TERM.as_raw()));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn spawn_lists_the_new_group() {
    let mut cmd = Command::new("sleep");
    cmd.arg("60").process_group(0);
    let (child, listing) = spawn(&mut cmd).expect("sleep spawned");
    let pgid = child.id();
    let watchdog = Watchdog::group(pgid);
    assert_eq!(listing.pgid(), pgid);
    assert!(live().contains(pgid), "the new group is listed");

    assert!(signal(pgid, Signal::KILL).is_ok(), "the kill landed");
    assert_eq!(reap(child).signal(), Some(9));
    live().unlist(listing);
    assert!(!live().contains(pgid), "the unlisted group leaves the list");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn spawn_failure_lists_nothing() {
    let before = live().len();
    let mut cmd = Command::new("fiber-support-no-such-program");
    let result = spawn(&mut cmd);
    assert!(
        matches!(&result, Err(Error::Spawn(source)) if source.kind() == std::io::ErrorKind::NotFound),
        "a missing program fails with a NotFound spawn error"
    );
    assert_eq!(live().len(), before, "a failed spawn lists nothing");
}

#[test]
fn list_refuses_zero_and_one() {
    let before = live().len();
    assert!(live().list(0).is_none());
    assert!(live().list(1).is_none());
    assert_eq!(live().len(), before, "a refused id lists nothing");
}

#[test]
fn two_listings_are_two_entries() {
    let before = live().len();
    let first = live().list(UNUSED_TWO).expect("listed");
    let second = live().list(UNUSED_TWO).expect("listed again");
    assert_eq!(live().len(), before + 2);

    live().unlist(first);
    assert!(
        live().contains(UNUSED_TWO),
        "unlisting one entry keeps the id"
    );
    live().unlist(second);
    assert!(!live().contains(UNUSED_TWO), "unlisting both drops the id");
}

#[test]
fn unlisting_by_serial_survives_an_id_reuse() {
    let old = live().list(UNUSED).expect("listed");
    // The group holds nothing, so the shared kill prunes the entry.
    kill_every_group();
    assert!(!live().contains(UNUSED), "the empty group was pruned");
    let new = live().list(UNUSED).expect("the reused id lists again");

    live().unlist(old);
    assert!(
        live().contains(UNUSED),
        "the old run's unlist keeps the new registration"
    );
    live().unlist(new);
    assert!(!live().contains(UNUSED));
}

#[test]
fn spawn_takes_no_lock_across_fork_and_exec() {
    let dir = TempDir::new("fiber-support-spawn-window");
    let ready = Ready::new(dir.path());
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(ignores_sigterm(ready.path()))
        .process_group(0);
    // The list stays locked: a spawn that locked across fork and exec
    // could never start its child past this point.
    let guard = live();
    let (listed, result) = mpsc::channel();
    thread::spawn(move || {
        let _sent = listed.send(spawn(&mut cmd));
    });

    // The child started while the guard was still held.
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    // The spawn waits only to list.
    assert!(
        matches!(
            result.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout)
        ),
        "spawn returns only once it can list"
    );
    drop(guard);

    let (child, listing) = Deadline::after(DEADLINE)
        .recv(&result)
        .expect("spawn answered after the lock released")
        .expect("spawn succeeded");
    assert!(live().contains(child.id()), "the child is listed");
    assert!(signal(child.id(), Signal::KILL).is_ok(), "the kill landed");
    assert_eq!(reap(child).signal(), Some(9));
    live().unlist(listing);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn spawn_after_a_kill_is_killed_at_its_listing() {
    kill_every_group();
    let mut cmd = Command::new("sleep");
    cmd.arg("60").process_group(0);
    let (child, listing) = spawn(&mut cmd).expect("a spawn after a kill still returns Ok");
    let pgid = child.id();
    let watchdog = Watchdog::group(pgid);

    assert_eq!(
        reap(child).signal(),
        Some(9),
        "the shutdown kill reached it"
    );
    live().unlist(listing);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn kill_every_group_kills_a_listed_live_group() {
    let child = sleep();
    let pgid = child.id();
    let watchdog = Watchdog::group(pgid);
    let listing = live().list(pgid).expect("listed");
    assert!(!live().is_empty());

    kill_every_group();
    assert_eq!(reap(child).signal(), Some(9));
    assert!(
        live().contains(pgid),
        "a killed group stays listed until reaped"
    );

    // Reaped, the group is empty: the next kill drops it unsignalled.
    kill_every_group();
    assert!(!live().contains(pgid), "an empty group leaves the list");
    live().unlist(listing);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn kill_every_group_drops_an_empty_group() {
    let listing = live().list(UNUSED).expect("listed");
    kill_every_group();
    assert!(!live().contains(UNUSED), "the empty group was dropped");
    live().unlist(listing);
}

#[test]
fn kill_every_group_holds_the_lock_while_it_signals() {
    let child = sleep();
    let pgid = child.id();
    let watchdog = Watchdog::group(pgid);
    let listing = live().list(pgid).expect("listed");

    // The list stays locked: the kill cannot probe or signal past this point.
    let guard = live();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        kill_every_group();
        let _sent = done.send(());
    });
    assert!(
        matches!(
            finished.recv_timeout(GROUP_POLL * 10),
            Err(RecvTimeoutError::Timeout)
        ),
        "the kill waits for the lock"
    );
    assert!(
        kill_group(pgid, "0").expect("the probe ran"),
        "the child still runs while the lock is held"
    );
    drop(guard);

    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the kill ran after the lock released");
    assert_eq!(reap(child).signal(), Some(9));
    live().unlist(listing);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn signal_name_names_every_known_signal() {
    for (signal, name) in [
        (Signal::HUP, "SIGHUP"),
        (Signal::INT, "SIGINT"),
        (Signal::QUIT, "SIGQUIT"),
        (Signal::ILL, "SIGILL"),
        (Signal::TRAP, "SIGTRAP"),
        (Signal::ABORT, "SIGABRT"),
        (Signal::BUS, "SIGBUS"),
        (Signal::FPE, "SIGFPE"),
        (Signal::KILL, "SIGKILL"),
        (Signal::USR1, "SIGUSR1"),
        (Signal::SEGV, "SIGSEGV"),
        (Signal::USR2, "SIGUSR2"),
        (Signal::PIPE, "SIGPIPE"),
        (Signal::ALARM, "SIGALRM"),
        (Signal::TERM, "SIGTERM"),
        (Signal::CHILD, "SIGCHLD"),
        (Signal::CONT, "SIGCONT"),
        (Signal::STOP, "SIGSTOP"),
        (Signal::TSTP, "SIGTSTP"),
        (Signal::TTIN, "SIGTTIN"),
        (Signal::TTOU, "SIGTTOU"),
        (Signal::URG, "SIGURG"),
        (Signal::XCPU, "SIGXCPU"),
        (Signal::XFSZ, "SIGXFSZ"),
        (Signal::VTALARM, "SIGVTALRM"),
        (Signal::PROF, "SIGPROF"),
        (Signal::WINCH, "SIGWINCH"),
        (Signal::SYS, "SIGSYS"),
    ] {
        assert_eq!(signal_name(signal.as_raw()), name);
    }
}

#[test]
fn signal_name_numbers_the_unknown() {
    assert_eq!(signal_name(0), "SIG0");
    assert_eq!(signal_name(999), "SIG999");
}
