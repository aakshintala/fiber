//! Tests first for the delegate group extension: signalling a live
//! group, retiring an empty one, killing a surviving member, and the reap
//! holding the lock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;
use fakes::{Watchdog, group_empties, kill_group, within};
use support::group::Listing;

use super::{listed, reap_locked, retire_if_empty, serial_shared, signal};

/// How long a test waits on a child before it fails.
const DEADLINE: Duration = Duration::from_secs(3);

/// A group that holds `sleep 60`, listed through the shared spawn. Its
/// stdio is null, so even a child that outlives the test holds no harness
/// pipe.
fn sleeping() -> (std::process::Child, Listing, fakes::Watchdog) {
    let mut command = Command::new("sleep");
    command
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let (child, listing) = support::group::spawn(&mut command).unwrap();
    let pgid = child.id();
    let watchdog = Watchdog::group(pgid);
    (child, listing, watchdog)
}

#[test]
fn a_spawned_group_is_listed() {
    let _serial = serial_shared();
    let (mut child, _listing, watchdog) = sleeping();
    let pgid = child.id();
    assert!(listed(pgid));
    assert!(support::group::alive(pgid));
    child.kill().unwrap();
    child.wait().unwrap();
    watchdog.stand_down(DEADLINE);
}

/// Reaps `child` on its own thread, at most `DEADLINE` of wall clock:
/// the caller cannot block on it, or a child that ignores the signal
/// would hang the test instead of failing it.
#[track_caller]
fn reap(mut child: std::process::Child, signal: &str) -> std::process::ExitStatus {
    let (done, waited) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(child.wait());
    });
    Deadline::after(DEADLINE)
        .recv(&waited)
        .unwrap_or_else(|_| panic!("{signal} did not end the child within {DEADLINE:?}"))
        .unwrap_or_else(|source| panic!("waiting the child failed: {source}"))
}

#[test]
fn a_live_group_gets_sigterm() {
    let _serial = serial_shared();
    let (child, _listing, watchdog) = sleeping();
    let pgid = child.id();
    assert!(signal(pgid, rustix::process::Signal::TERM));
    // Reaped before the emptiness check: on Linux a zombie still answers
    // kill(-pgid, 0), so the group reads occupied until it is reaped.
    assert_eq!(reap(child, "SIGTERM").signal(), Some(15));
    assert!(group_empties(pgid, DEADLINE), "SIGTERM emptied the group");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_reaped_group_whose_members_are_gone_leaves_the_list() {
    let _serial = serial_shared();
    let (mut child, listing, watchdog) = sleeping();
    let pgid = child.id();
    let mut listing = Some(listing);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(group_empties(pgid, DEADLINE));
    assert!(retire_if_empty(&mut listing));
    assert!(listing.is_none(), "the token is gone with its entry");
    assert!(!listed(pgid));
    // Retired, a signal to it is a no-op: the id may already belong to
    // someone else.
    assert!(!signal(pgid, rustix::process::Signal::KILL));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_listed_but_empty_group_is_not_signalled() {
    let _serial = serial_shared();
    // Listed without a member: the guard needs both, so neither part
    // alone sends.
    let pgid = 999_999_007;
    let mut listing = support::group::live().list(pgid);
    assert!(listing.is_some(), "the id lists");
    assert!(listed(pgid));
    assert!(!support::group::alive(pgid), "nothing holds the group");
    assert!(!signal(pgid, rustix::process::Signal::KILL));
    assert!(retire_if_empty(&mut listing));
}

#[test]
fn a_reaped_leader_with_a_surviving_member_kills_it_and_stays_listed() {
    let _serial = serial_shared();
    let dir = fakes::TempDir::new("fiber-delegate-member");
    let pidfile = dir.path().join("pid");
    // The member outlives the leader in the same group; the watchdog is
    // the backstop if the SIGKILL below never runs.
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            &format!("sleep 60 & echo $! > '{}'; exit 0", pidfile.display()),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let (mut child, listing) = support::group::spawn(&mut command).unwrap();
    let pgid = child.id();
    let mut listing = Some(listing);
    let watchdog = Watchdog::group(pgid);
    let member: u32 = within("the member writes its pid", DEADLINE, move || {
        loop {
            let Ok(text) = std::fs::read_to_string(&pidfile) else {
                thread::yield_now();
                continue;
            };
            let Ok(pid) = text.trim().parse() else {
                thread::yield_now();
                continue;
            };
            return pid;
        }
    });
    child.wait().unwrap();
    reap_locked(&mut child, &mut listing);
    assert!(listing.is_some(), "with a member surviving the token stays");
    assert!(
        listed(pgid),
        "with a member surviving the group stays listed"
    );
    assert!(
        fakes::pids_exit(&[member], DEADLINE),
        "the surviving member got SIGKILL"
    );
    assert!(group_empties(pgid, DEADLINE));
    assert!(retire_if_empty(&mut listing));
    assert!(!listed(pgid));
    // The watchdog finds nothing left to kill.
    watchdog.stand_down(DEADLINE);
}

#[test]
fn an_unlisted_live_group_is_not_signalled() {
    let _serial = serial_shared();
    let mut command = Command::new("sleep");
    command
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().unwrap();
    let pgid = child.id();
    let watchdog = Watchdog::group(pgid);
    assert!(!signal(pgid, rustix::process::Signal::TERM));
    assert!(kill_group(pgid, "0").unwrap(), "nothing was sent");
    child.kill().unwrap();
    child.wait().unwrap();
    watchdog.stand_down(DEADLINE);
}

#[test]
fn the_reap_waits_for_the_lock_and_leaves_a_zombie_until_then() {
    let _serial = serial_shared();
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("exit 0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let (mut child, listing) = support::group::spawn(&mut command).unwrap();
    let pid = child.id();
    let mut listing = Some(listing);
    // Short-lived, but guarded like every spawned child: a mutant that
    // kept it alive would otherwise leave it behind.
    let watchdog = Watchdog::group(pid);
    // The leader has exited, and nobody reaps it but the call below.
    exited(pid);
    // The lock is held while the reap runs on another thread: it cannot
    // reap until the lock is released.
    let held = support::group::live();
    let (done, waited) = mpsc::channel();
    thread::spawn(move || {
        reap_locked(&mut child, &mut listing);
        let _sent = done.send(());
    });
    exited(pid);
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&waited)
            .is_err(),
        "the reap did not run under the held lock"
    );
    drop(held);
    Deadline::after(DEADLINE)
        .recv(&waited)
        .expect("the reap ran");
    assert!(!listed(pid), "the empty group retired under the lock");
    watchdog.stand_down(DEADLINE);
}

/// Whether `pid` has exited without being reaped: `waitid` with `NOWAIT`
/// sees the exit and leaves the zombie.
#[track_caller]
fn exited(pid: u32) {
    use rustix::process::{Pid, WaitId, WaitIdOptions};
    within("the leader exits", DEADLINE, move || {
        let id = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        loop {
            let seen = rustix::process::waitid(
                WaitId::Pid(id),
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
            )
            .ok()
            .flatten()
            .is_some();
            if seen {
                return;
            }
            thread::yield_now();
        }
    });
}

#[test]
fn a_listed_group_with_a_member_is_not_retired() {
    let _serial = serial_shared();
    let (mut child, listing, watchdog) = sleeping();
    let pgid = child.id();
    let mut listing = Some(listing);
    // Listed and alive, the guard keeps it: retiring it here would drop a
    // live group from the list while its id is still in use.
    assert!(!retire_if_empty(&mut listing));
    assert!(listing.is_some(), "a live group keeps its token");
    assert!(listed(pgid), "a live group stays listed");
    // Without a registration there is nothing to retire either.
    let mut none: Option<Listing> = None;
    assert!(!retire_if_empty(&mut none));
    child.kill().unwrap();
    child.wait().unwrap();
    watchdog.stand_down(DEADLINE);
}
