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
use contract::clock::Clock;

use super::{CancelOnDrop, Reads, alive, deadline_due, kill_group, signallable};
use fakes::Deadline;
use fakes::children::Ready;
use fakes::clock::FakeClock;
use rustix::io::Errno;
use support::group::GROUP_POLL;

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
    assert!(fakes::kill_group(group, "0").unwrap(), "sleep lives"); // A later command prunes only empty groups: `sleep` keeps this one.
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

/// Runs `reads.credential` for `command` on a thread of its own, returning
/// the channel its result arrives on. The test receives through
/// [`Deadline`], never a raw `recv_timeout`.
fn credential_on_thread(
    reads: &Arc<Reads>,
    command: Command,
    timeout: Duration,
    clock: &Arc<FakeClock>,
) -> mpsc::Receiver<std::io::Result<std::process::Output>> {
    let (done, finished) = mpsc::channel();
    let (reads, clock) = (Arc::clone(reads), Arc::clone(clock));
    std::thread::spawn(move || {
        let mut command = command;
        done.send(reads.credential(&mut command, timeout, &*clock))
            .unwrap_or(());
    });
    finished
}

#[test]
fn a_credential_command_still_running_at_its_timeout_is_killed_and_times_out() {
    let root = fakes::TempDir::new("fiber-read-credential-timeout");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    let mark = clock.advance_marked(timeout.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked_since(&mark, Some(deadline), DEADLINE),
        "the wait parked again after the advance"
    );
    // Armed while the wait is parked: the bump drives one more pass
    // through its checks, which pauses at the checkpoint still short of
    // the deadline. The acknowledgement proves the checks ran with
    // nothing to answer, so the immediate check proves no early answer
    // arrived without reading the real clock.
    let (checked, is_checked) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_checks(checked, freed);
    reads.wake.bump();
    Deadline::after(DEADLINE).recv_or_fail(&is_checked, "the wait's checks before its timeout");
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the wait answered before its timeout"
    );
    release.send(()).unwrap();
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline again"
    );
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "the group died before its timeout"
    );
    clock.advance(Duration::from_millis(1));
    let error = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a command still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the timeout killed the group"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn a_leader_that_exits_while_its_descendant_holds_stdout_times_out_and_the_group_dies() {
    let root = fakes::TempDir::new("fiber-read-credential-stdout");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    // The `sleep` inherits stdout, so the leader's exit is not the run's end.
    let script = format!("echo $$ > '{}'; sleep 600 & exit 0", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    let mark = clock.advance_marked(timeout.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked_since(&mark, Some(deadline), DEADLINE),
        "the wait parked again after the advance"
    );
    // Armed while the wait is parked: the bump drives one more pass
    // through its checks, which pauses at the checkpoint still short of
    // the deadline. The acknowledgement proves the checks ran with
    // nothing to answer, so the immediate check proves no early answer
    // arrived without reading the real clock.
    let (checked, is_checked) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_checks(checked, freed);
    reads.wake.bump();
    Deadline::after(DEADLINE).recv_or_fail(&is_checked, "the wait's checks before its timeout");
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the wait answered before its timeout"
    );
    release.send(()).unwrap();
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline again"
    );
    clock.advance(Duration::from_millis(1));
    // The timeout re-kills on the poll cadence and parks between passes:
    // drive each confirmed poll park, as the re-kill test does. The advance
    // above wakes only one pass, whose probe can still see a
    // reaped-but-not-yet-reaped zombie alive under load; without further
    // advances nothing ever bumps again and the wait parks forever. Each
    // advance follows a confirmed park, and each park waits only a poll
    // pause of the drive, so an already-retired group still ends the loop
    // on its answer instead of stalling on a park that never comes.
    let drive = Deadline::after(DEADLINE);
    let mut answer: Option<std::io::Result<std::process::Output>> = None;
    while answer.is_none() {
        match finished.try_recv() {
            Ok(got) => {
                answer = Some(got);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        if drive.left().is_zero() {
            break;
        }
        let poll = clock
            .now()
            .checked_add(GROUP_POLL)
            .expect("the poll is countable");
        if clock.await_parked(poll, drive.left().min(PARK_PAUSE)) {
            clock.advance(GROUP_POLL);
        }
    }
    let error = match answer {
        Some(got) => got.expect_err("a command holding stdout past its timeout"),
        None => Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv_or_fail(&finished, "the timed-out read")
            .expect_err("a command holding stdout past its timeout"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the timeout killed the group"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// The timeout kill re-kills a group that survives the first SIGKILL until
/// no process is left in it: the script answers the first two probes alive
/// — a fork landing inside the kernel kill walk — and SRCH only after the
/// second send, so the wait must deliver twice and retire the group on
/// SRCH. An accepted send never retires the group, only SRCH does.
#[test]
fn timeout_rekills_a_group_that_survives_its_first_kill() {
    let root = fakes::TempDir::new("fiber-read-timeout-rekill");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    // Both wakes before the timeout must land before the poll cadence
    // below: the spawn's listing and the reaper's answer. Captured before
    // the spawn, so both are still to come.
    let sent = reads.wake.generation();
    // The `sleep` inherits stdout, so the leader's exit is not the run's
    // end: the wait parks at its deadline with the leader already gone,
    // and the timeout below needs no reap wait, only the group's kills.
    let script = format!("echo $$ > '{}'; sleep 600 & exit 0", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    // The reaper's answer bump can otherwise still be in flight here:
    // landing after the first poll park below, it would drive an extra
    // settlement pass with `now` still at the deadline, spending a probe
    // early so the second poll park never comes. Both wakes before the
    // timeout — the spawn's listing and the reaper's answer — land first,
    // so each advance below drives exactly one pass. The stdout reader
    // never answers while `sleep` holds the pipe, nothing is cancelled,
    // and the clock has not moved, so these are the only two.
    let wake = Arc::clone(&reads.wake);
    fakes::within("the leader's wake", DEADLINE, {
        let wake = Arc::clone(&wake);
        move || loop {
            if wake.generation() >= sent + 2 {
                return;
            }
            std::thread::yield_now();
        }
    });
    assert_eq!(
        wake.generation(),
        sent + 2,
        "the spawn's listing and the reaper's answer are the only wakes before the timeout"
    );
    // Alive past the first kill, gone after the second.
    reads.script_group(
        pgid,
        vec![Ok(()), Ok(()), Err(Errno::SRCH)],
        vec![Ok(()), Ok(())],
    );
    clock.advance(timeout);
    // The timeout re-kills on the poll cadence: each advance follows a
    // confirmed park, so the first two parks each earn one more send and
    // the third observation retires the group.
    let poll = deadline + GROUP_POLL;
    assert!(
        clock.await_parked(poll, DEADLINE),
        "the timeout parked at its first poll"
    );
    clock.advance(GROUP_POLL);
    let repoll = poll + GROUP_POLL;
    assert!(
        clock.await_parked(repoll, DEADLINE),
        "the timeout parked at its second poll"
    );
    clock.advance(GROUP_POLL);
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a command still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(reads.scripted_sends(), 2, "one send per pass before SRCH");
    assert!(
        !listed(&reads).contains(&pgid),
        "SRCH retires the group: {:?}",
        listed(&reads)
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap it on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the real group is gone"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// The timeout gives up re-killing at its bound: one nanosecond short it
/// is still parked with no answer, at the bound it answers `TimedOut` with
/// the group still listed for later settlement. Pins the boundary both
/// sides.
#[test]
fn timeout_leaves_a_stubborn_group_listed_at_its_bound() {
    let root = fakes::TempDir::new("fiber-read-timeout-bound");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let end = deadline + doors::SHUTDOWN_BOUND;
    let reads = Arc::new(Reads::default());
    // As in the re-kill test above: the leader's exit is not the run's
    // end, so the timeout needs no reap wait, only the group's kills.
    let script = format!("echo $$ > '{}'; sleep 600 & exit 0", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    // Alive past every probe within the bound: 600 scripted sends at a
    // 10 ms cadence outlast the 5 s bound, as in the settle retry test.
    reads.script_group(
        pgid,
        (0..600).map(|_| Ok(())).collect(),
        (0..600).map(|_| Ok(())).collect(),
    );
    clock.advance(timeout);
    assert!(
        clock.await_parked(deadline + GROUP_POLL, DEADLINE),
        "the timeout parked at its first poll"
    );
    // Jump to one nanosecond short of the end: every pass between still
    // found the scripted group alive and sent again.
    let mark = clock.advance_marked(
        doors::SHUTDOWN_BOUND
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), DEADLINE),
        "the timeout parked again short of its bound"
    );
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "timeout answered short of its bound"
    );
    clock.advance(Duration::from_nanos(1));
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a command still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        listed(&reads).contains(&pgid),
        "the bound leaves the group listed: {:?}",
        listed(&reads)
    );
    let sends = reads.scripted_sends();
    assert!(
        sends >= 2,
        "the timeout retried within the bound, not once: {sends}"
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap it on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the real group is gone"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// The timeout shares one absolute end for settlement and the reap: the
/// leader below never exits, so an unbounded reap would hang past the
/// end. One nanosecond short of the end the wait is still parked with no
/// answer; at the end it answers `TimedOut` with the group still listed
/// for later settlement. Pins the boundary both sides.
#[test]
fn timeout_with_a_live_leader_answers_at_the_bound_without_the_reap() {
    let root = fakes::TempDir::new("fiber-read-timeout-live-leader");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let end = deadline + doors::SHUTDOWN_BOUND;
    let reads = Arc::new(Reads::default());
    // The leader never exits: `sleep` holds both the group and stdout, so
    // neither the exit nor the output ever arrives.
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    // Alive past every probe within the bound: 600 scripted sends at a
    // 10 ms cadence outlast the 5 s bound, as in the settle retry test.
    reads.script_group(
        pgid,
        (0..600).map(|_| Ok(())).collect(),
        (0..600).map(|_| Ok(())).collect(),
    );
    clock.advance(timeout);
    assert!(
        clock.await_parked(deadline + GROUP_POLL, DEADLINE),
        "the timeout parked at its first poll"
    );
    // Jump to one nanosecond short of the end: still parked, no answer.
    let mark = clock.advance_marked(
        doors::SHUTDOWN_BOUND
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), DEADLINE),
        "the timeout parked again short of its bound"
    );
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "timeout answered short of its bound"
    );
    clock.advance(Duration::from_nanos(1));
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a live leader still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        listed(&reads).contains(&pgid),
        "the bound leaves the group listed: {:?}",
        listed(&reads)
    );
    let sends = reads.scripted_sends();
    assert!(
        sends >= 2,
        "the timeout retried within the bound, not once: {sends}"
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap it on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the real group is gone"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// A cancel that lands while the timeout settles its group answers
/// `Interrupted`, never `TimedOut`: the cancel bumps the parked
/// settlement pass, which observes it at once. The settlement below is
/// parked at its first poll when the cancel lands, on every run.
#[test]
fn cancel_during_settlement_answers_interrupted() {
    let root = fakes::TempDir::new("fiber-read-cancel-during-settlement");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    // Scripted alive through the controlled poll: a real SIGKILL and the
    // reaper wake would otherwise complete the timeout before the cancel
    // below, so settlement stays pending until the cancel ends it. The
    // scripted sends never touch the real group; the test kills it for
    // real after the answer.
    reads.script_group(
        pgid,
        (0..10).map(|_| Ok(())).collect(),
        (0..10).map(|_| Ok(())).collect(),
    );
    clock.advance(timeout);
    // The settlement re-kills on the poll cadence: parked at its first
    // poll, the wait can only leave through the cancel below.
    assert!(
        clock.await_parked(deadline + GROUP_POLL, DEADLINE),
        "the timeout parked at its first poll"
    );
    reads.cancel();
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the cancelled read")
        .expect_err("a read cancelled while timing out");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    // The scripted sends never touched the real group: kill it for real
    // and reap it on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the real group is gone"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn a_quick_command_answers_without_the_clock_moving() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let mut printf = Command::new("printf");
    printf
        .arg("key")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    let guard = fakes::Watchdog::matching("printf key");
    let finished = credential_on_thread(&reads, printf, Duration::from_secs(30), &clock);
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the quick command")
        .unwrap();
    assert_eq!(output.stdout, b"key");
    assert_eq!(
        reads.lock().spawning,
        0,
        "the successful spawn dropped the count"
    );
    guard.stand_down(DEADLINE);
}

/// A finish that lands between the wait's checks and its park is not
/// missed: the checkpoint holds the wait past its checks, so the helper's
/// finish lands there on every run. The generation captured before the
/// checks already holds those wakes, so the park ends at once; captured
/// after the checks, the park would miss them and hang on the frozen
/// clock until this test's deadline.
#[test]
fn a_completion_between_the_checks_and_the_park_is_not_missed() {
    let root = fakes::TempDir::new("fiber-read-credential-interleave");
    let ready = Ready::new(root.path());
    let go = root.path().join("go");
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo $$ > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf key",
        ready.path().display(),
        go.display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // Armed before the wait starts, so its first pass already pauses: the
    // acknowledgement below proves its checks ran with nothing to read,
    // since the helper cannot finish before `go` exists.
    let (checked, is_checked) = mpsc::channel();
    let (release, freed) = mpsc::channel();
    reads.pause_at_checks(checked, freed);
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE).recv_or_fail(&is_checked, "the wait's checks");
    // Both wakes land while the wait is held past its checks: the park
    // below already holds them in an earlier generation, so it must
    // return at once. Each thread bumps after its send, so two bumps mean
    // both answers are in.
    let wake = Arc::clone(&reads.wake);
    let seen = wake.generation();
    std::fs::write(&go, "").unwrap();
    fakes::within("the two wakes", DEADLINE, move || {
        loop {
            if wake.generation() >= seen + 2 {
                return;
            }
            std::thread::yield_now();
        }
    });
    release.send(()).unwrap();
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the interleaved read")
        .unwrap();
    assert_eq!(output.stdout, b"key");
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the group reaped its leader"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// A key finished after its timeout is refused: the state lock holds the
/// wait between its checks while the helper finishes and the clock moves
/// past the deadline, so the wait observes both done past its end. The
/// late key is never accepted; the group is killed and the read times out.
#[test]
fn a_key_finished_after_its_timeout_is_refused() {
    let root = fakes::TempDir::new("fiber-read-credential-late-key");
    let ready = Ready::new(root.path());
    let go = root.path().join("go");
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo $$ > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf key",
        ready.path().display(),
        go.display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    // The helper finishes while the wait is held here, then the clock
    // moves past the deadline: the wait's next checks see a finish past
    // its end, which times out instead of answering the late key.
    let wake = Arc::clone(&reads.wake);
    let seen = wake.generation();
    {
        let _held = reads.lock();
        std::fs::write(&go, "").unwrap();
        fakes::within("the two wakes", DEADLINE, move || {
            loop {
                if wake.generation() >= seen + 2 {
                    return;
                }
                std::thread::yield_now();
            }
        });
        clock.advance(timeout + Duration::from_secs(1));
    }
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the late read")
        .expect_err("a key finished after its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the timeout killed the group"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// A helper that prints its key, closes stdout, then finishes cleanup
/// answers without the clock moving: the stdout reader's wake lands while
/// the leader still runs, and only the leader's own wake ends the wait.
/// `go` releases the print and `out` releases the exit, so stdout closes
/// strictly before the leader exits on every run; the second checkpoint
/// proves the wait consumed stdout with the exit still missing, since
/// `out` does not exist yet.
#[test]
fn a_leader_that_closes_stdout_before_it_exits_answers_without_the_clock_moving() {
    let root = fakes::TempDir::new("fiber-read-credential-cleanup");
    let ready = Ready::new(root.path());
    let go = root.path().join("go");
    let out = root.path().join("out");
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo $$ > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf key; exec >&-; while [ ! -e '{}' ]; do sleep 0.01; done; exit 0",
        ready.path().display(),
        go.display(),
        out.display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // Armed before the wait starts, so its first pass already pauses: the
    // acknowledgement proves its checks ran with nothing to read, since
    // the helper cannot print before `go` exists.
    let (checked_one, is_checked_one) = mpsc::channel::<()>();
    let (release_one, freed_one) = mpsc::channel::<()>();
    reads.pause_at_checks(checked_one, freed_one);
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let wake = Arc::clone(&reads.wake);
    let sent = wake.generation();
    Deadline::after(DEADLINE).recv_or_fail(&is_checked_one, "the wait's first checks");
    // Armed again while the wait is still held, so its next pass pauses
    // too.
    let (checked_two, is_checked_two) = mpsc::channel::<()>();
    let (release_two, freed_two) = mpsc::channel::<()>();
    reads.pause_at_checks(checked_two, freed_two);
    release_one.send(()).unwrap();
    std::fs::write(&go, "").unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&is_checked_two, "the wait's second checks");
    // Only the print's bump could have woken the wait, so the second
    // acknowledgement proves stdout is consumed with the leader still
    // running. Only now may the leader exit: its own bump lands while
    // the wait is still held, which is what ends the wait. Each thread
    // bumps after its send, so two bumps mean both answers are in.
    std::fs::write(&out, "").unwrap();
    fakes::within("the leader's wake", DEADLINE, {
        let wake = Arc::clone(&wake);
        move || loop {
            if wake.generation() >= sent + 2 {
                return;
            }
            std::thread::yield_now();
        }
    });
    release_two.send(()).unwrap();
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the cleaning-up leader")
        .unwrap();
    assert_eq!(output.stdout, b"key");
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the group reaped its leader"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn cancel_ends_a_credential_wait_with_interrupted_and_kills_the_group() {
    let root = fakes::TempDir::new("fiber-read-credential-cancel");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    reads.cancel();
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the cancelled read")
        .expect_err("a cancelled read");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the cancel killed the group"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn a_timeout_past_what_the_clock_can_count_starts_nothing() {
    let root = fakes::TempDir::new("fiber-read-credential-overflow");
    let marker = root.path().join("ran");
    let clock = FakeClock::new();
    let reads = Reads::default();
    // The watchdog only fires if a mutant spawns the command below; the
    // script exits at once, so a spawn still ends the test with a failure.
    let guard = fakes::Watchdog::matching(&marker.display().to_string());
    let mut command = sh(&format!("echo x > '{}'", marker.display()));
    let error = reads
        .credential(&mut command, Duration::MAX, &*clock)
        .expect_err("a timeout past what the clock can count");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        error
            .to_string()
            .contains("its timeout is longer than this system's clock can count"),
        "{error}"
    );
    assert!(!marker.exists(), "the command never started");
    assert!(listed(&reads).is_empty());
    guard.stand_down(DEADLINE);
}

#[test]
fn deadline_due_is_true_from_the_deadline_on() {
    let clock = FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(1);
    for (now, due) in [
        (
            deadline.checked_sub(Duration::from_nanos(1)).unwrap(),
            false,
        ),
        (deadline, true),
        (deadline + Duration::from_nanos(1), true),
    ] {
        assert_eq!(deadline_due(now, deadline), due, "{now:?}");
    }
}

#[test]
fn dropping_cancel_on_drop_kills_what_a_finished_command_left() {
    let root = fakes::TempDir::new("fiber-read-cancel-on-drop");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!(
        "sleep 600 >/dev/null 2>&1 & echo $$ > '{}'; printf key",
        ready.path().display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // On a thread under a wall-clock deadline: a missed wake fails here,
    // never hangs the test binary.
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the finished read")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "key");
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(fakes::kill_group(pgid, "0").unwrap(), "sleep lives");
    drop(CancelOnDrop(Arc::clone(&reads)));
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "dropping the guard killed sleep"
    );
    let error = reads
        .credential(&mut sh("true"), Duration::from_secs(30), &*clock)
        .expect_err("a read after the drop");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// A cancel lands while a spawn is blocked before it spawns: the cancel
/// returns at once and kills every listed group, and the late spawn lists
/// its own group for settlement, kills it and is `Interrupted`. Before the
/// fix the spawn holds the state lock across the spawn, so the cancel
/// blocks behind it and the deadline below fails the test.
#[test]
fn cancel_passes_a_blocked_spawn_and_the_late_child_is_killed() {
    let root = fakes::TempDir::new("fiber-read-blocked-spawn");
    let ready_a = root.path().join("ready-a");
    let marker = format!("fiber-read-late-{}", root.path().display());
    let reads = Arc::new(Reads::default());
    let script_a = format!("echo $$ > '{}'; exec sleep 30", ready_a.display());
    let pre_a = fakes::Watchdog::matching(&ready_a.display().to_string());
    // Group A pauses after it lists its group, so the assertion below
    // sees it listed on every run: the ready file alone does not prove
    // the spawn listed it yet.
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (listed_release_tx, listed_release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, listed_release_rx);
    let running_a = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script_a)))
    };
    let pid_a: u32 = ready_line(&ready_a).parse().unwrap();
    let watchdog_a = fakes::Watchdog::group(pid_a);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("group A listed its group");
    assert_eq!(listed(&reads), vec![pid_a], "group A listed while it runs");
    listed_release_tx.send(()).unwrap();
    // The next spawn blocks before it spawns; the marker stays in the
    // late shell's command line, so a leaked late child still matches.
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered_tx, release_rx);
    let late = fakes::Watchdog::matching(&marker);
    let script_b = format!("sleep 30 # {marker}");
    let running_b = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script_b)))
    };
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the spawn reached its pause");
    let (cancel_done_tx, cancel_done_rx) = mpsc::channel::<()>();
    {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || {
            reads.cancel();
            match cancel_done_tx.send(()) {
                Ok(()) | Err(_) => {}
            }
        });
    }
    Deadline::after(DEADLINE)
        .recv(&cancel_done_rx)
        .expect("the cancel returns while the spawn is blocked");
    assert!(
        fakes::group_empties(pid_a, DEADLINE),
        "the cancel killed group A"
    );
    match release_tx.send(()) {
        Ok(()) | Err(_) => {}
    }
    let error = fakes::within("the late command", DEADLINE, move || {
        running_b.join().unwrap()
    })
    .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    // The late group is listed for settlement: A was already empty (the
    // wait above), so the late spawn's prune retires it and lists only
    // the late group. The leader's reap never retires surviving
    // descendants; settlement does, once the probe says SRCH.
    assert_eq!(
        listed(&reads).len(),
        1,
        "only the late group is listed: {:?}",
        listed(&reads)
    );
    assert!(
        !listed(&reads).contains(&pid_a),
        "A retired once empty: {:?}",
        listed(&reads)
    );
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    // Settlement retires the reaped late group: nothing stays listed.
    let clock = FakeClock::new();
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the settle"),
        "the settle retires the late group"
    );
    assert!(
        listed(&reads).is_empty(),
        "settlement retires the late group: {:?}",
        listed(&reads)
    );
    let late_a = fakes::within("the killed command", DEADLINE, move || {
        running_a.join().unwrap()
    })
    .expect_err("a killed command");
    assert_eq!(late_a.kind(), std::io::ErrorKind::Interrupted);
    watchdog_a.stand_down(DEADLINE);
    pre_a.stand_down(DEADLINE);
    late.stand_down(DEADLINE);
}

/// Drives a settle to completion on the fake clock and returns its
/// answer, through a controlled interleaving that shows both halves
/// while the reap stays held. The blind-probe phase keeps every hold
/// and runs ten advance-to-confirmed-re-park cycles exactly like the old
/// drive, proving it spends budget with zero progress: the clock moves
/// exactly ten advances while the held leader stays unreaped, so the
/// settle still lists the real group. The listing is read from the
/// settle itself: a killed-but-unreaped leader probes `EPERM` on some
/// platforms, which keeps the group pending in the settle (only `SRCH`
/// retires) while reading empty through the raw probe. The gated phase
/// then proves the fixed drive holds its budget while the group is
/// held: a few iterations still list the group, and an await for a park
/// past the current one must time out — without a clock advance the
/// settle never leaves its park (its kill path bumps nothing), so no
/// re-park arrives. Only then are the release senders dropped and the
/// settle driven to completion: past the release zero-move wakes let
/// the settle observe it without spending budget, and every budgeted
/// advance needs both a confirmed re-park and the settle no longer
/// listing `real`, so a slow real death parks the settle without
/// spending the bound. Completion and every acknowledgement share the
/// drive's one deadline: each acknowledgement waits only a poll pause
/// of it. Fails the test when the drive's deadline runs out first.
#[track_caller]
fn drive_settle_to_completion(
    clock: &Arc<FakeClock>,
    reads: &Arc<Reads>,
    finished: &mpsc::Receiver<bool>,
    drive: &Deadline,
    real: u32,
    releases: Vec<mpsc::Sender<()>>,
) -> bool {
    let start = clock.origin();
    let poll = start + GROUP_POLL;
    assert!(
        clock.await_parked(poll, drive.left()),
        "the settle parked at its poll"
    );
    // Blind-probe phase, holds maintained: ten advances. Each confirmed
    // re-park is the held path; once the settle answers early instead
    // (its group died unheld) later cycles keep the cadence, so the
    // still-listed assert below names the failure, not these lines.
    let mut answered = false;
    let mut mark = clock.advance_marked(Duration::from_millis(50));
    for _ in 1..10 {
        let reparked = clock.await_any_parked_since(&mark, drive.left().min(PARK_PAUSE));
        if !reparked && !answered {
            answered = finished.try_recv().is_ok();
        }
        assert!(
            reparked || answered,
            "the settle parked again after the probe advance"
        );
        mark = clock.advance_marked(Duration::from_millis(50));
    }
    let reparked = clock.await_any_parked_since(&mark, drive.left().min(PARK_PAUSE));
    if !reparked && !answered {
        answered = finished.try_recv().is_ok();
    }
    assert!(
        reparked || answered,
        "the settle parked again after the last probe advance"
    );
    assert_eq!(
        clock.now(),
        start + Duration::from_millis(500),
        "the blind probe spent exactly ten advances"
    );
    assert!(
        listed(reads).contains(&real),
        "the held group is still listed after the blind probe: {:?}",
        listed(reads)
    );
    // Retention phase, holds maintained: the fixed drive spends nothing
    // while the group is held. Four iterations still list the group,
    // then an await for a park past the current one must time out:
    // without a clock advance the settle never leaves its park (its
    // kill path bumps nothing), so no re-park arrives. The mark below
    // is taken without waking the settle, so the await is genuine.
    for _ in 0..4 {
        assert!(
            listed(reads).contains(&real),
            "the held group stays listed while frozen: {:?}",
            listed(reads)
        );
    }
    let frozen = clock
        .mark_parked(clock.now() + GROUP_POLL, drive.left())
        .expect("the settle parked at its poll");
    assert!(
        !clock.await_any_parked_since(&frozen, PARK_PAUSE),
        "the settle re-parked without a clock advance"
    );
    assert_eq!(
        clock.now(),
        start + Duration::from_millis(500),
        "the retention spent no budget"
    );
    assert!(
        listed(reads).contains(&real),
        "the held group is still listed before release: {:?}",
        listed(reads)
    );
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the settle answered while its group was held"
    );
    for release in releases {
        drop(release);
    }
    // Release observation: the settle learns of the release only on a
    // wake (its kill path bumps nothing), so each pass below wakes one
    // without moving the clock — the park the mark names proves the
    // settle waits on it first. The loop ends once the settle retires
    // the reaped group (and answers in the same pass) or answers
    // outright; the clock still sits at the blind-probe mark after.
    let mut answer = finished.try_recv().ok();
    while answer.is_none() && listed(reads).contains(&real) {
        if drive.left().is_zero() {
            break;
        }
        let parked_at = clock.now() + GROUP_POLL;
        match clock.mark_parked(parked_at, drive.left().min(PARK_PAUSE)) {
            Some(_) => {
                clock.advance_marked(Duration::ZERO);
            }
            None => {
                // Never parked again: the pass answered instead.
                answer = finished.try_recv().ok();
                break;
            }
        }
        answer = finished.try_recv().ok();
    }
    assert_eq!(
        clock.now(),
        start + Duration::from_millis(500),
        "the release observation spent no budget"
    );
    // Gated tail: every budgeted advance needs both a confirmed re-park
    // and the settle no longer listing `real`, so a slow real death
    // parks the settle without spending the bound. The marks below are
    // taken without waking the settle, so a hold iteration's
    // `PARK_PAUSE`-bounded await genuinely times out while frozen.
    // Usually skipped: the observation above already holds the answer.
    let answer = match answer {
        Some(answer) => answer,
        None => loop {
            if let Ok(answer) = finished.try_recv() {
                break answer;
            }
            if drive.left().is_zero() {
                break drive.recv(finished).expect("the settle");
            }
            let parked_at = clock.now() + GROUP_POLL;
            match clock.mark_parked(parked_at, drive.left().min(PARK_PAUSE)) {
                // No park again: the settle answered instead; take it
                // under the drive.
                None => break drive.recv(finished).expect("the settle"),
                Some(since) => {
                    if !listed(reads).contains(&real)
                        && clock.await_any_parked_since(&since, drive.left().min(PARK_PAUSE))
                    {
                        clock.advance_marked(Duration::from_millis(50));
                    }
                }
            }
        },
    };
    if answer {
        assert!(
            clock.now() < start + doors::SHUTDOWN_BOUND,
            "the gated drive kept budget: {:?} past {start:?}",
            clock.now() - start
        );
    }
    answer
}

/// A cancelled group with a live member is settled by killing it, not by
/// counting spawns: the command below blocks in `sleep` with nothing in
/// flight, so a count-only settle answers `true` while the group lives.
/// The `true`-means-empty assertion fails at Base on every platform; the
/// fix kills the pending group and answers `true` only once it is empty.
#[test]
fn settle_rekills_a_cancelled_survivor_before_answering_true() {
    let root = fakes::TempDir::new("fiber-read-settle-survivor");
    let ready = root.path().join("ready");
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    // Paused after listing: the acknowledgement proves the group is listed
    // with nothing in flight before the survivor state is seeded below.
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    // Held before the reap: the acknowledgement proves the command thread
    // reached its leader's reap, so the leader stays a zombie and the
    // group stays listed until the drive below releases it. The pause is
    // one-shot, so it is armed before the only command thread starts.
    let (held_tx, is_held) = mpsc::channel::<()>();
    let (free_tx, freed_rx) = mpsc::channel::<()>();
    reads.pause_at_command_reap(held_tx, freed_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    assert_eq!(listed(&reads), vec![pgid], "listed while it runs");
    assert_eq!(reads.lock().spawning, 0, "nothing in flight");
    release_tx.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&is_held)
        .expect("the command held before its reap");
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "the member lives before the settle"
    );
    // The probe's post-cancel survivor: cancelled with a live listed group
    // and no kill sent, as a fork during the kernel kill walk leaves one.
    // Set directly: `cancel` would kill the group and hide the survivor.
    reads.lock().cancelled = true;
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    let drive = Deadline::after(DEADLINE);
    let answer = drive_settle_to_completion(&clock, &reads, &settled, &drive, pgid, vec![free_tx]);
    let lives = fakes::kill_group(pgid, "0").unwrap_or(false);
    // Guarded cleanup before the assertions: the survivor must not
    // outlive the test either way.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let _finished = fakes::within("the killed command", DEADLINE, move || {
        running.join().unwrap()
    });
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
    assert!(answer, "settle answers `true` once the group is empty");
    assert!(
        !lives,
        "settle answered `true` while group {pgid} still lives"
    );
}

/// The superseded blind drive spends the whole bound while a held group
/// stays listed: with the reap held through the entire drive, every
/// 50 ms advance only re-parks the settle, so every derived cycle runs,
/// the settle answers `false` at the bound, and the group is still
/// listed. This pins the ticket's core scenario — bound spent while the
/// group stays listed — which the claim tests above now drive through
/// the gated helper instead. Deterministic with no load: every cycle is
/// confirmed by a re-park, the group cannot unlist while its leader
/// stays an unreaped zombie, and the clock moves only through this
/// test's advances.
#[test]
fn settle_withheld_reap_spends_the_bound_with_the_group_listed() {
    let root = fakes::TempDir::new("fiber-read-settle-withheld");
    let ready = root.path().join("ready");
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    // Paused after listing: the acknowledgement proves the group is listed
    // with nothing in flight before the survivor state is seeded below.
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    // Held before the reap: the acknowledgement proves the command thread
    // reached its leader's reap, so the leader stays a zombie and the
    // group stays listed. Unlike the claim tests, the hold stays armed
    // through the whole drive below and is dropped only for cleanup.
    let (held_tx, is_held) = mpsc::channel::<()>();
    let (free_tx, freed_rx) = mpsc::channel::<()>();
    reads.pause_at_command_reap(held_tx, freed_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    assert_eq!(listed(&reads), vec![pgid], "listed while it runs");
    assert_eq!(reads.lock().spawning, 0, "nothing in flight");
    release_tx.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&is_held)
        .expect("the command held before its reap");
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "the member lives before the settle"
    );
    // The probe's post-cancel survivor: cancelled with a live listed group
    // and no kill sent, as a fork during the kernel kill walk leaves one.
    // Set directly: `cancel` would kill the group and hide the survivor.
    reads.lock().cancelled = true;
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The superseded blind pattern, pinned: advance to each confirmed
    // re-park for the whole bound at the 50 ms cadence — the count is
    // derived from the bound, never hardcoded — checking the answer each
    // iteration, since the final advance's pass reaches the bound and
    // answers instead of re-parking.
    let drive = Deadline::after(DEADLINE);
    let poll = clock.origin() + GROUP_POLL;
    assert!(
        clock.await_parked(poll, drive.left()),
        "the settle parked at its poll"
    );
    let cycles = (doors::SHUTDOWN_BOUND.as_millis() / 50) as usize;
    let mut mark = clock.advance_marked(Duration::from_millis(50));
    let mut answer = None;
    for _ in 1..cycles {
        if let Ok(got) = settled.try_recv() {
            answer = Some(got);
            break;
        }
        // A missing re-park means the settle answered instead of
        // parking again; the drive takes it below. While the reap stays
        // held that never happens before the bound.
        if !clock.await_any_parked_since(&mark, drive.left().min(PARK_PAUSE)) {
            answer = Some(drive.recv(&settled).expect("the settle"));
            break;
        }
        mark = clock.advance_marked(Duration::from_millis(50));
    }
    // The final advance's pass answers at the bound instead of re-parking.
    let answer = match answer {
        Some(answer) => answer,
        None => drive.recv(&settled).expect("the settle"),
    };
    assert!(
        !answer,
        "the withheld drive spends the bound and settles false"
    );
    assert_eq!(
        clock.now(),
        clock.origin() + doors::SHUTDOWN_BOUND,
        "the blind drive spent exactly the bound"
    );
    assert!(
        listed(&reads).contains(&pgid),
        "the bound leaves the group listed: {:?}",
        listed(&reads)
    );
    // Cleanup: the release drops before anything joins the holder, then
    // the group is killed for real and the command thread reaped.
    drop(free_tx);
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let _finished = fakes::within("the killed command", DEADLINE, move || {
        running.join().unwrap()
    });
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// A cancelled group that survives the first kill gets SIGKILL again, and
/// none once SRCH retires it: the script answers the cancel's probe and
/// send, the settle's first probe and send, then SRCH. The send count pins
/// both deliveries and the absence of any after retirement.
#[test]
fn settle_sends_again_until_srch_then_never_again() {
    let root = fakes::TempDir::new("fiber-read-settle-retry");
    let ready = root.path().join("ready");
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    release_tx.send(()).unwrap();
    // Alive through the cancel and the settle's first pass, gone on the
    // second: an accepted send never retires the group, only SRCH does.
    reads.script_group(
        pgid,
        vec![Ok(()), Ok(()), Err(Errno::SRCH)],
        vec![Ok(()), Ok(())],
    );
    let finished = cancel_and_settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The cancel's kill and the settle's first pass each deliver once;
    // the settle then parks at its first poll, an exact deadline on the
    // unmoved clock.
    let poll = clock.origin() + GROUP_POLL;
    assert!(
        clock.await_parked(poll, DEADLINE),
        "the settle parked at its poll"
    );
    clock.advance(GROUP_POLL);
    assert!(
        Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the settle"),
        "the settle retires the group on SRCH"
    );
    assert_eq!(reads.scripted_sends(), 2, "one send per pass before SRCH");
    // A retired group earns no more sends: another settle finds nothing.
    let again = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&again, "the second settle"),
        "a retired group settles at once"
    );
    assert_eq!(
        reads.scripted_sends(),
        2,
        "no send after SRCH retired the group"
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap the command thread on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let _finished = fakes::within("the killed command", DEADLINE, move || {
        running.join().unwrap()
    });
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the real group is gone"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// Two cancelled groups drain together within the one bound, though their
/// empty observations arrive on different passes: the scripted group is
/// observed SRCH on the settle's second pass, while the real group is
/// pruned whenever its death lands. The scripted group is also killed for
/// real up front, so the script never disagrees with the kernel past its
/// schedule and no exhaustion fallback can keep it pending.
#[test]
fn settle_drains_two_groups_together_within_one_bound() {
    let root = fakes::TempDir::new("fiber-read-settle-two-groups");
    let ready_one = root.path().join("ready-one");
    let ready_two = root.path().join("ready-two");
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let pre_one = fakes::Watchdog::matching(&ready_one.display().to_string());
    let pre_two = fakes::Watchdog::matching(&ready_two.display().to_string());
    let start = |reads: &Arc<Reads>, ready: &std::path::Path| {
        let reads = Arc::clone(reads);
        let script = format!("echo $$ > '{}'; exec sleep 600", ready.display());
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    // Each spawn pauses after it lists its group: the acknowledgements
    // prove both groups are listed before the assertion below. A ready
    // line alone does not, since the child writes it before the spawn
    // relocks and lists. The pauses are one-shot, so each spawn's pause
    // is armed before it starts, once the earlier spawn proved it listed.
    let (listed_one, is_listed_one) = mpsc::channel::<()>();
    let (free_one, freed_one) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_one, freed_one);
    // Each command thread also holds before it reaps its leader, so the
    // second group's leader stays a zombie and the group stays listed
    // until the drive below releases it. The pauses are one-shot: the
    // first hold is armed before the first thread starts and acknowledged
    // before the second is armed, so each thread consumes its own.
    let (held_one, is_held_one) = mpsc::channel::<()>();
    let (release_one, freed_reap_one) = mpsc::channel::<()>();
    reads.pause_at_command_reap(held_one, freed_reap_one);
    let running_one = start(&reads, &ready_one);
    Deadline::after(DEADLINE)
        .recv(&is_listed_one)
        .expect("the first group listed");
    free_one.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&is_held_one)
        .expect("the first command held before its reap");
    let (listed_two, is_listed_two) = mpsc::channel::<()>();
    let (free_two, freed_two) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_two, freed_two);
    let (held_two, is_held_two) = mpsc::channel::<()>();
    let (release_two, freed_reap_two) = mpsc::channel::<()>();
    reads.pause_at_command_reap(held_two, freed_reap_two);
    let running_two = start(&reads, &ready_two);
    Deadline::after(DEADLINE)
        .recv(&is_listed_two)
        .expect("the second group listed");
    free_two.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&is_held_two)
        .expect("the second command held before its reap");
    let pgid_one: u32 = ready_line(&ready_one).parse().unwrap();
    let pgid_two: u32 = ready_line(&ready_two).parse().unwrap();
    let watchdog_one = fakes::Watchdog::group(pgid_one);
    let watchdog_two = fakes::Watchdog::group(pgid_two);
    assert_eq!(listed(&reads).len(), 2, "both groups listed");
    // The first group follows the script; the second stays real, so every
    // send really kills it. The first is also killed for real up front:
    // its command thread reaps the leader while the script schedules its
    // SRCH observation on the second pass, whatever wall time that takes.
    reads.script_group(
        pgid_one,
        vec![Ok(()), Ok(()), Err(Errno::SRCH)],
        vec![Ok(()), Ok(())],
    );
    match fakes::kill_group(pgid_one, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let finished = cancel_and_settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The cancel's kill and the settle's first pass each deliver to the
    // scripted group; the shared drive below advances past each confirmed
    // park, so the script schedules its SRCH observation on the second
    // pass, whatever wall time the real group's death takes.
    let drive = Deadline::after(DEADLINE);
    assert!(
        drive_settle_to_completion(
            &clock,
            &reads,
            &finished,
            &drive,
            pgid_two,
            vec![release_one, release_two]
        ),
        "both groups drain within the one bound"
    );
    assert_eq!(reads.scripted_sends(), 2, "one send per pass before SRCH");
    assert!(
        fakes::group_empties(pgid_two, DEADLINE),
        "the real group died through the shared settlement"
    );
    // Reap both command threads on every path: the first was killed up
    // front, the second through the shared settlement.
    let _one = fakes::within("the first killed command", DEADLINE, move || {
        running_one.join().unwrap()
    });
    let _two = fakes::within("the second killed command", DEADLINE, move || {
        running_two.join().unwrap()
    });
    assert!(
        fakes::group_empties(pgid_one, DEADLINE),
        "the scripted group is gone"
    );
    watchdog_one.stand_down(DEADLINE);
    watchdog_two.stand_down(DEADLINE);
    pre_one.stand_down(DEADLINE);
    pre_two.stand_down(DEADLINE);
}

/// PERM and other probe errors keep a cancelled group pending: they never
/// retire it, so the settle retries within the bound and answers `false`
/// at it. The script outlasts the 5 s bound at a 10 ms cadence.
#[test]
fn settle_retries_a_perm_group_until_the_bound_then_fails() {
    let root = fakes::TempDir::new("fiber-read-settle-perm");
    let ready = root.path().join("ready");
    let clock = FakeClock::new();
    let end = clock.origin() + doors::SHUTDOWN_BOUND;
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.display());
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    release_tx.send(()).unwrap();
    let probes: Vec<rustix::io::Result<()>> = (0..300)
        .flat_map(|_| [Err(Errno::PERM), Err(Errno::INTR)])
        .collect();
    let sends: Vec<rustix::io::Result<()>> = vec![Ok(()); 600];
    reads.script_group(pgid, probes, sends);
    let finished = cancel_and_settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // Drive to the bound, one acknowledged park per advance. The settle
    // cannot answer before the bound: every probe keeps it pending.
    let poll = clock.origin() + GROUP_POLL;
    assert!(
        clock.await_parked(poll, DEADLINE),
        "the settle parked at its poll"
    );
    let mut mark = clock.advance_marked(Duration::from_millis(50));
    loop {
        if let Ok(answer) = finished.try_recv() {
            assert!(
                !answer,
                "a group pending past every probe never settles true"
            );
            break;
        }
        if clock.now() >= end {
            let answer = Deadline::after(DEADLINE)
                .recv(&finished)
                .expect("the settle");
            assert!(!answer, "a group still pending at the bound settles false");
            break;
        }
        assert!(
            clock.await_any_parked_since(&mark, DEADLINE),
            "the settle parked again"
        );
        if let Ok(answer) = finished.try_recv() {
            assert!(
                !answer,
                "a group pending past every probe never settles true"
            );
            break;
        }
        mark = clock.advance_marked(Duration::from_millis(50));
    }
    let sends = reads.scripted_sends();
    assert!(
        sends >= 2,
        "the settle retried within the bound, not once: {sends}"
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap the command thread on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let _finished = fakes::within("the killed command", DEADLINE, move || {
        running.join().unwrap()
    });
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the real group is gone"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// Ids no credential group can hold are refused before any syscall: 0, 1
/// and ids past the pid range are never alive, and killing them sends
/// nothing. The signallable edge sits between 1 and 2.
#[test]
fn unusable_ids_are_neither_alive_nor_signalled() {
    assert!(!alive(0));
    assert!(!alive(1));
    assert!(!alive(u32::MAX));
    // Refused, not evidence of anything: these send no KILL anywhere.
    kill_group(0);
    kill_group(1);
    kill_group(u32::MAX);
}

/// A refusal id stays not alive without consulting its script: 0 and 1
/// are unsignallable while `u32::MAX` is unrepresentable, so the guarded
/// validation returns before any probe. Each script answers `Ok` (alive),
/// so the `&&` mutant, which proceeds on either half alone, answers true
/// and fails here.
#[test]
fn refusal_ids_stay_not_alive_without_probing() {
    let reads = Reads::default();
    reads.script_group(0, vec![Ok(())], vec![]);
    assert!(
        !reads.lock().group_alive(0),
        "group 0 is refused before any probe"
    );
    reads.script_group(1, vec![Ok(())], vec![]);
    assert!(
        !reads.lock().group_alive(1),
        "group 1 is refused before any probe"
    );
    reads.script_group(u32::MAX, vec![Ok(())], vec![]);
    assert!(
        !reads.lock().group_alive(u32::MAX),
        "an unrepresentable id is refused before any probe"
    );
}

/// A refusal id is never sent to: 0 and 1 are unsignallable while
/// `u32::MAX` is unrepresentable, so the guarded validation returns
/// before any send. The `&&` mutant, which proceeds on either half
/// alone, records a scripted send and fails the count below.
#[test]
fn refusal_ids_are_never_sent() {
    let reads = Reads::default();
    reads.script_group(0, vec![], vec![Ok(())]);
    reads.lock().kill_listed(0);
    assert_eq!(
        reads.scripted_sends(),
        0,
        "group 0 is refused before any send"
    );
    reads.script_group(1, vec![], vec![Ok(())]);
    reads.lock().kill_listed(1);
    assert_eq!(
        reads.scripted_sends(),
        0,
        "group 1 is refused before any send"
    );
    reads.script_group(u32::MAX, vec![], vec![Ok(())]);
    reads.lock().kill_listed(u32::MAX);
    assert_eq!(
        reads.scripted_sends(),
        0,
        "an unrepresentable id is refused before any send"
    );
}

/// A cancelled settle with nothing listed answers `true` at once: no
/// group means no poll park, on any clock.
#[test]
fn settle_after_cancel_answers_true_at_once_with_nothing_listed() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    reads.cancel();
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the settle"),
        "nothing listed settles at once"
    );
}

/// Repeat shutdowns share the first end: a second `cancel_and_settle` and
/// a later `settle` earn no fresh bound. The held spawn keeps both waits
/// parked; past the first end both answer `false`.
#[test]
fn repeat_shutdowns_reuse_the_first_end() {
    let clock = FakeClock::new();
    let bound = doors::SHUTDOWN_BOUND;
    let end = clock.origin() + bound;
    let reads = Arc::new(Reads::default());
    // The marker stays in the held shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the release below.
    let marker = "fiber-shutdown-end-reuse";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let first = cancel_and_settle_on_thread(&reads, &clock, bound);
    assert!(
        clock.await_parked(end, DEADLINE),
        "the first shutdown parked at its end"
    );
    assert_eq!(
        reads.lock().shutdown_end,
        Some(end),
        "the first call stored the end"
    );
    // A repeat one nanosecond before the end must not move it.
    let mark = clock.advance_marked(
        bound
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), DEADLINE),
        "the first shutdown parked again at its end"
    );
    let second = cancel_and_settle_on_thread(&reads, &clock, bound);
    assert_eq!(
        reads.lock().shutdown_end,
        Some(end),
        "the repeat stored no fresh end"
    );
    // Past the first end both answer `false`: the repeat earned nothing.
    clock.advance(Duration::from_nanos(2));
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&first)
            .expect("the first shutdown"),
        "the held spawn outlasts the shared end"
    );
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&second)
            .expect("the second shutdown"),
        "the repeat shares the spent end"
    );
    // A later settle reads the same spent end and answers `false` at
    // once, without parking.
    let settled = settle_on_thread(&reads, &clock, bound);
    assert!(
        !Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the spent settle"),
        "a settle past the shared end answers false at once"
    );
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", DEADLINE, move || held.join().unwrap())
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}

/// An end past what the clock can count expires at once: a held spawn
/// still answers `false` without parking, and the stored end is now.
#[test]
fn an_overflowing_shutdown_end_expires_at_once() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let marker = "fiber-shutdown-end-overflow";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let now = clock.now();
    let finished = cancel_and_settle_on_thread(&reads, &clock, Duration::MAX);
    assert!(
        !Deadline::after(Duration::from_secs(3))
            .recv(&finished)
            .expect("the shutdown"),
        "an overflowing bound expires at once"
    );
    assert_eq!(
        reads.lock().shutdown_end,
        Some(now),
        "overflow stores now, not None"
    );
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", DEADLINE, move || held.join().unwrap())
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}

/// An empty shutdown answers `true` even past its stored end: expiry only
/// fails a shutdown with anything still pending.
#[test]
fn an_empty_shutdown_answers_true_past_its_end() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let first = cancel_and_settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3))
            .recv(&first)
            .expect("the first shutdown"),
        "an empty shutdown settles at once"
    );
    clock.advance(doors::SHUTDOWN_BOUND + Duration::from_secs(1));
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the spent settle"),
        "an empty shutdown stays true past its end"
    );
}

/// Runs `reads.cancel_and_settle` for `bound` on `clock` on a thread of
/// its own, returning the channel its answer arrives on. The test receives
/// through [`Deadline`], never a raw `recv_timeout`.
fn cancel_and_settle_on_thread(
    reads: &Arc<Reads>,
    clock: &Arc<FakeClock>,
    bound: Duration,
) -> mpsc::Receiver<bool> {
    let (done, finished) = mpsc::channel();
    let (reads, clock) = (Arc::clone(reads), Arc::clone(clock));
    std::thread::spawn(move || {
        done.send(reads.cancel_and_settle(&*clock, bound))
            .unwrap_or(());
    });
    finished
}

/// Runs `reads.settle` for `bound` on `clock` on a thread of its own,
/// returning the channel its answer arrives on. The test receives through
/// [`Deadline`], never a raw `recv_timeout`.
fn settle_on_thread(
    reads: &Arc<Reads>,
    clock: &Arc<FakeClock>,
    bound: Duration,
) -> mpsc::Receiver<bool> {
    let (done, finished) = mpsc::channel();
    let (reads, clock) = (Arc::clone(reads), Arc::clone(clock));
    std::thread::spawn(move || {
        done.send(reads.settle(&*clock, bound)).unwrap_or(());
    });
    finished
}

/// The bound each negative settle receive waits: a settle that may answer
/// does so in milliseconds, so silence for this long proves it is held.
const SHORT: Duration = Duration::from_secs(2);

/// One park acknowledgement's wait of the drive's shared deadline: a
/// re-park after an advance lands in milliseconds, so expiry re-checks
/// completion and awaits the late park under what is left. A poll pause,
/// not a deadline.
const PARK_PAUSE: Duration = Duration::from_millis(200);

/// A cancel mid-spawn is settled before exit: the spawn blocks before it
/// spawns, the cancel lands, and the settle holds until the late spawn
/// lists or kills its group. Without the count the settle answers at once,
/// and the negative receive below turns red.
#[test]
fn a_cancel_mid_spawn_is_settled_before_exit() {
    let root = fakes::TempDir::new("fiber-read-settle-spawn");
    let marker = format!("fiber-read-settle-{}", root.path().display());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches.
    let watching = fakes::Watchdog::matching(&marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let finished = credential_on_thread(
        &reads,
        sh(&format!("sleep 30 # {marker}")),
        Duration::from_secs(30),
        &clock,
    );
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    // Armed before the release, so the late spawn stops between its kill
    // and its reap: the acknowledgement proves the kill is sent while the
    // leader waits unreaped, so the settle below holds on the count, not
    // on process timing.
    let (reaping, is_reaping) = mpsc::channel::<()>();
    let (free_reap, reap_freed) = mpsc::channel::<()>();
    reads.pause_at_reap(reaping, reap_freed);
    reads.cancel();
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(SHORT).recv(&settled).is_err(),
        "settle returned with a spawn in flight"
    );
    release.send(()).expect("the spawn's release");
    Deadline::after(DEADLINE)
        .recv(&is_reaping)
        .expect("the spawn reached its reap");
    // The spawn listed its group before the kill, so the listing holds
    // the late PGID: the shell itself cannot report it, as the kill lands
    // before it could run.
    let groups = listed(&reads);
    assert_eq!(
        groups.len(),
        1,
        "only the late group is listed: {:?}",
        groups
    );
    let pgid = groups[0];
    // The settle holds on the raised count while the reap is held: one
    // bounded negative receive covers the whole hold, so a settle that
    // answered on the kill alone fails here. One deadline for the whole
    // hold, never one per receive.
    assert!(
        Deadline::after(DEADLINE).recv(&settled).is_err(),
        "settle returned with the late child unreaped"
    );
    free_reap.send(()).expect("the reap's release");
    // The late kill landed while the reap was held, so the shell is
    // already dead: the wall-clock waits confirm it before the fake clock
    // moves, never ahead of the real death. The command line only proves
    // the shell is gone; the group wait proves the whole group is empty
    // before the drive advances.
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the late group is empty"
    );
    // A fork that landed inside the kill still lists the group after the
    // reap: the settle re-kills on its poll cadence and parks between
    // passes with no further wake, so each advance follows a confirmed
    // park until the answer arrives, on the stdout-hold timeout's cadence.
    // The answer still arrives through `DEADLINE`.
    let drive = Deadline::after(DEADLINE);
    let mut answer: Option<bool> = None;
    while answer.is_none() {
        match settled.try_recv() {
            Ok(got) => {
                answer = Some(got);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        if drive.left().is_zero() {
            break;
        }
        let poll = clock
            .now()
            .checked_add(GROUP_POLL)
            .expect("the poll is countable");
        if clock.await_parked(poll, drive.left().min(PARK_PAUSE)) {
            clock.advance(GROUP_POLL);
        }
    }
    let answer = match answer {
        Some(answer) => answer,
        None => drive.recv(&settled).expect("the settle"),
    };
    assert!(answer, "settle waits out the late kill");
    let error = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(reads.lock().spawning, 0, "the late kill dropped the count");
    assert!(
        listed(&reads).is_empty(),
        "the settle retired the late group: {:?}",
        listed(&reads)
    );
    watching.stand_down(DEADLINE);
}

/// The settle waits for the late child's reap, not just its kill: the
/// spawn stops between the kill and the wait, and the settle holds until
/// the reap releases it. A decrement at the relock answers early and turns
/// the negative receive red.
#[test]
fn settle_waits_for_the_late_childs_reap() {
    let root = fakes::TempDir::new("fiber-read-settle-reap");
    let marker = format!("fiber-read-reap-{}", root.path().display());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let watching = fakes::Watchdog::matching(&marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let finished = credential_on_thread(
        &reads,
        sh(&format!("sleep 30 # {marker}")),
        Duration::from_secs(30),
        &clock,
    );
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    // Armed before the release, so the late kill stops at the reap.
    let (reaping, is_reaping) = mpsc::channel::<()>();
    let (free_reap, reap_freed) = mpsc::channel::<()>();
    reads.pause_at_reap(reaping, reap_freed);
    reads.cancel();
    release.send(()).expect("the spawn's release");
    Deadline::after(DEADLINE)
        .recv(&is_reaping)
        .expect("the spawn reached its reap");
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The leader is killed but not yet reaped: the count is still raised.
    assert!(
        Deadline::after(SHORT).recv(&settled).is_err(),
        "settle returned before the late child's reap"
    );
    free_reap.send(()).expect("the reap's release");
    assert!(
        Deadline::after(Duration::from_secs(3))
            .recv(&settled)
            .expect("the settle"),
        "settle waits out the reap"
    );
    let error = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(reads.lock().spawning, 0, "the late kill dropped the count");
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}

#[test]
fn a_failed_spawn_does_not_leave_the_count_raised() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let mut missing = Command::new("fiber-test-binary-that-cannot-exist");
    missing
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    // Within a wall-clock bound: a missed decrement hangs the settle, and
    // the bound fails the test instead of hanging the binary. The exact
    // count pins the decrement at once, so a `+=` or `/=` mutant fails
    // here instead of running out the settle below.
    let error = fakes::within("the failed spawn", DEADLINE, {
        let reads = Arc::clone(&reads);
        move || reads.command(&mut missing)
    })
    .expect_err("a command that cannot start");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(reads.lock().spawning, 0, "the error arm dropped the count");
    // The settle runs on its own thread and its answer arrives through
    // `Deadline` in seconds: under the `==`/`!=` mutant the settle at zero
    // parks on the `FakeClock`, so this fails instead of hanging the suite.
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the settle"),
        "the error arm dropped the count"
    );
}

/// The settle gives up at the bound: one nanosecond short it is still
/// parked with no answer, at the bound it answers `false`. Pins the
/// boundary both sides.
#[test]
fn settle_gives_up_at_the_bound() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let bound = doors::SHUTDOWN_BOUND;
    let end = clock.origin() + bound;
    // A spawn held before it spawns: the count stays raised while the test
    // drives the clock.
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the cancel below.
    let marker = "fiber-settle-gives-up-at-the-bound";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let settled = settle_on_thread(&reads, &clock, bound);
    assert!(
        clock.await_parked(end, DEADLINE),
        "the settle parked at its bound"
    );
    let mark = clock.advance_marked(
        bound
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), DEADLINE),
        "the settle parked again short of its bound"
    );
    assert!(
        Deadline::after(SHORT).recv(&settled).is_err(),
        "settle answered short of its bound"
    );
    clock.advance(Duration::from_nanos(1));
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&settled)
            .expect("the settle"),
        "settle gives up at its bound"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", DEADLINE, move || held.join().unwrap())
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}

/// The settle subscribes before it reads the count: on a clock nothing
/// else subscribed, the advance still wakes it. Without the subscription
/// the advance wakes nobody and the receive below fails at the deadline.
#[test]
fn settle_wakes_on_a_clock_advance_with_no_earlier_subscription() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let bound = doors::SHUTDOWN_BOUND;
    let end = clock.origin() + bound;
    // A spawn held before it spawns, on a clock nothing else subscribed:
    // the settle below is the first subscriber.
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the cancel below.
    let marker = "fiber-settle-wakes-with-no-earlier-subscription";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let settled = settle_on_thread(&reads, &clock, bound);
    assert!(
        clock.await_parked(end, DEADLINE),
        "the settle parked at its bound"
    );
    clock.advance(bound + Duration::from_secs(1));
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&settled)
            .expect("the settle"),
        "the advance woke the settle past its bound"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", DEADLINE, move || held.join().unwrap())
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}

#[test]
fn settle_returns_true_at_once_with_nothing_in_flight() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    // The settle runs on its own thread and its answer arrives through
    // `Deadline` in seconds: under the `==`/`!=` mutant the settle at zero
    // parks on the `FakeClock`, so this fails instead of hanging the suite.
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the settle"),
        "nothing in flight settles at once"
    );
}

/// The settle's zero comparison pins both sides at once: `true` at count
/// zero immediately, and `false` within the bound while one spawn is held.
/// Each side fails fast on a wall-clock bound of a few seconds, so the
/// `!=` mutant is caught here instead of running out the suite's
/// deadlines. The bound stays on the injected clock: the test advances it
/// only after the parked settle proves it waits on it.
#[test]
fn settle_pins_zero_true_and_held_false_at_once() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    // Zero side first: true at once with nothing in flight.
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3))
            .recv_or_fail(&settled, "settle with nothing in flight"),
        "settle is true at count zero"
    );
    // Held side: one spawn held before it spawns keeps the count raised
    // while the test drives the clock past the bound.
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the cancel below.
    let marker = "fiber-settle-pins-zero-and-held";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let bound = doors::SHUTDOWN_BOUND;
    let end = clock.origin() + bound;
    let held_settle = settle_on_thread(&reads, &clock, bound);
    assert!(
        clock.await_parked(end, DEADLINE),
        "the settle parked at its bound"
    );
    clock.advance(bound + Duration::from_secs(1));
    assert!(
        !Deadline::after(Duration::from_secs(3)).recv_or_fail(&held_settle, "the held settle"),
        "settle is false at its bound while a spawn is held"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", DEADLINE, move || held.join().unwrap())
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}
